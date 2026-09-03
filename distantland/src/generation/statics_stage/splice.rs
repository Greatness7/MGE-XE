//! Owner-partial shard splicing and assembly verification.

use hashbrown::HashSet;
use itertools::Itertools;

use super::StaticShardPlan;
use super::decode::DecodedShards;
use super::plan::{OwnerPlan, StaticsFallbackReason};
use crate::DistantStatics;
use crate::generation::cache::{fingerprint_static_mesh_shard_inputs, static_mesh_shard_id};
use crate::generation::metrics::{summarize_distant_statics, summarize_merged_distant_statics};
use crate::generation::output::STATIC_MESH_SHARD_COUNT;
use crate::generation::record_key::{StaticRecordKey, parse_merged};
use crate::generation::state_db::StaticShardState;
use crate::generation::storage::state::RequiredArtifact;
use crate::generation::units::MergeUnitKey;

#[derive(Default)]
pub(super) struct AssemblyStats {
    pub(super) ordinary_carried: usize,
    pub(super) ordinary_rebuilt: usize,
    pub(super) ordinary_added: usize,
    pub(super) ordinary_removed: usize,
    pub(super) merged_carried: usize,
    pub(super) merged_rebuilt: usize,
    pub(super) merged_added: usize,
    pub(super) merged_removed: usize,
}

pub(super) struct SpliceResult {
    pub(super) static_mesh_shards: [StaticShardState; STATIC_MESH_SHARD_COUNT],
    pub(super) shards: Vec<StaticShardPlan>,
    pub(super) ordinals: crate::usage::StaticOrdinalView,
    pub(super) final_counts: (usize, usize, usize),
    pub(super) merged_counts: (usize, usize, usize),
    pub(super) stats: AssemblyStats,
}

fn owner_is_dirty(key: &StaticRecordKey, owners: &OwnerPlan) -> bool {
    match key {
        StaticRecordKey::Mesh { id: mesh } => mesh_owner_is_dirty(mesh, owners),
        StaticRecordKey::Merged { cell_x, cell_y, .. } => cell_owner_is_dirty(*cell_x, *cell_y, owners),
    }
}

/// [`owner_is_dirty`] for a raw `DistantStatics` map key, without parsing it into an owned key.
///
/// Classification here is by the same rule [`StaticRecordKey::parse`] applies, so a key spelled
/// like the synthetic merged form is treated as merged. This is identical to parsing it first.
pub(super) fn owner_key_is_dirty(key: &str, owners: &OwnerPlan) -> bool {
    match parse_merged(key) {
        Some((cell_x, cell_y, _)) => cell_owner_is_dirty(cell_x, cell_y, owners),
        None => mesh_owner_is_dirty(key, owners),
    }
}

fn mesh_owner_is_dirty(mesh: &str, owners: &OwnerPlan) -> bool {
    owners.dirty_meshes.contains(mesh) || owners.removed_meshes.contains(mesh)
}

fn cell_owner_is_dirty(cell_x: i32, cell_y: i32, owners: &OwnerPlan) -> bool {
    let cell = MergeUnitKey::new(cell_x, cell_y);
    owners.dirty_cells.contains(&cell) || owners.removed_cells.contains(&cell)
}

pub(super) fn build_static_assembly(
    owners: &OwnerPlan,
    distant_statics: &DistantStatics,
    merged_keys: &[StaticRecordKey],
    planned_merged_keys: &[StaticRecordKey],
    previous_shards: &[StaticShardState; STATIC_MESH_SHARD_COUNT],
) -> std::result::Result<[Vec<StaticRecordKey>; STATIC_MESH_SHARD_COUNT], StaticsFallbackReason> {
    // Both sets exist only for membership tests, and the merged keys that reach `assembly` from
    // `previous` carry no heap data, so neither needs an owned copy of every mesh path.
    let previous: HashSet<&StaticRecordKey> = previous_shards.iter().flat_map(|shard| shard.records.iter()).collect();
    let planned: HashSet<&StaticRecordKey> = planned_merged_keys.iter().collect();
    // `distant_statics` holds ordinary records only: merged geometry is packed and released as it
    // is built, so the merged keys this pass emitted arrive separately.
    let mut current: HashSet<StaticRecordKey> = distant_statics
        .iter()
        .filter(|(_, distant_static)| !distant_static.subsets.is_empty())
        .map(|(key, _)| StaticRecordKey::parse(key))
        .collect();
    current.extend(merged_keys.iter().cloned());

    for key in current.iter().filter(|key| matches!(key, StaticRecordKey::Mesh { .. })) {
        if !owner_is_dirty(key, owners) && !previous.contains(key) {
            return Err(StaticsFallbackReason::CleanOwnerMismatch);
        }
    }
    for key in previous.iter().filter(|key| matches!(key, StaticRecordKey::Mesh { .. })) {
        if !owner_is_dirty(key, owners) && !current.contains(*key) {
            return Err(StaticsFallbackReason::CleanOwnerMismatch);
        }
    }

    let mut assembly: [Vec<StaticRecordKey>; STATIC_MESH_SHARD_COUNT] = std::array::from_fn(|_| Vec::new());
    for key in previous.iter().filter(|key| matches!(key, StaticRecordKey::Merged { .. })) {
        if owner_is_dirty(key, owners) {
            continue;
        }
        if !planned.contains(*key) {
            return Err(StaticsFallbackReason::CleanCellGroupMismatch);
        }
        assembly[key.shard_id()].push((*key).clone());
    }
    // Consumes `current`: both membership checks are complete above, and every shard is sorted
    // and deduped below, so the ordinary and merged keys no longer need separate passes.
    for key in current {
        if matches!(key, StaticRecordKey::Merged { .. }) && (!owner_is_dirty(&key, owners) || !planned.contains(&key)) {
            return Err(StaticsFallbackReason::AssemblyMismatch);
        }
        assembly[key.shard_id()].push(key);
    }
    for shard in &mut assembly {
        shard.sort();
        shard.dedup();
    }
    Ok(assembly)
}

fn assembly_stats(
    owners: &OwnerPlan,
    assembly: &[Vec<StaticRecordKey>; STATIC_MESH_SHARD_COUNT],
    previous_shards: &[StaticShardState; STATIC_MESH_SHARD_COUNT],
) -> AssemblyStats {
    let current: HashSet<_> = assembly.iter().flat_map(|shard| shard.iter()).collect();
    let previous: HashSet<_> = previous_shards.iter().flat_map(|shard| shard.records.iter()).collect();
    let mut stats = AssemblyStats::default();
    for key in &current {
        let existed = previous.contains(key);
        match key {
            StaticRecordKey::Mesh { .. } => {
                if owner_is_dirty(key, owners) {
                    stats.ordinary_rebuilt += 1;
                } else {
                    stats.ordinary_carried += 1;
                }
                stats.ordinary_added += usize::from(!existed);
            }
            StaticRecordKey::Merged { .. } => {
                if owner_is_dirty(key, owners) {
                    stats.merged_rebuilt += 1;
                } else {
                    stats.merged_carried += 1;
                }
                stats.merged_added += usize::from(!existed);
            }
        }
    }
    for key in previous.difference(&current) {
        match key {
            StaticRecordKey::Mesh { .. } => stats.ordinary_removed += 1,
            StaticRecordKey::Merged { .. } => stats.merged_removed += 1,
        }
    }
    stats
}

pub(super) fn splice_static_shards(
    owners: &OwnerPlan,
    assembly: [Vec<StaticRecordKey>; STATIC_MESH_SHARD_COUNT],
    mut decoded: DecodedShards,
    fresh_records: crate::PackedDistantStatics,
    previous_shards: &[StaticShardState; STATIC_MESH_SHARD_COUNT],
    base_shards: &[Option<&RequiredArtifact>; STATIC_MESH_SHARD_COUNT],
) -> std::result::Result<SpliceResult, StaticsFallbackReason> {
    let stats = assembly_stats(owners, &assembly, previous_shards);
    let mut fresh_by_shard: [crate::PackedDistantStatics; STATIC_MESH_SHARD_COUNT] =
        std::array::from_fn(|_| crate::PackedDistantStatics::default());
    for (key, record) in fresh_records {
        fresh_by_shard[static_mesh_shard_id(&key)].insert(key, record);
    }

    let mut states: [StaticShardState; STATIC_MESH_SHARD_COUNT] = std::array::from_fn(|_| StaticShardState::default());
    let mut shard_plans = Vec::with_capacity(STATIC_MESH_SHARD_COUNT);
    for shard_id in 0..STATIC_MESH_SHARD_COUNT {
        if !owners.dirty_shards.contains(&shard_id) {
            if assembly[shard_id] != previous_shards[shard_id].records {
                return Err(StaticsFallbackReason::AssemblyMismatch);
            }
            states[shard_id] = previous_shards[shard_id].clone();
            shard_plans.push(StaticShardPlan::Carry(
                base_shards[shard_id]
                    .ok_or(StaticsFallbackReason::BaseArtifactMissing)?
                    .clone(),
            ));
            continue;
        }

        let mut packed = decoded.shards[shard_id]
            .take()
            .ok_or(StaticsFallbackReason::ShardCountMismatch)?;
        // `build_static_assembly` leaves every shard sorted and deduped, so membership is a binary
        // search rather than a scan of the whole shard per surviving record.
        packed.retain(|key, _| {
            let key = StaticRecordKey::parse(key);
            !owner_is_dirty(&key, owners) && assembly[shard_id].binary_search(&key).is_ok()
        });
        for (key, record) in std::mem::take(&mut fresh_by_shard[shard_id]) {
            packed.insert(key, record);
        }
        packed.sort_unstable_by(|left, _, right, _| left.as_bytes().cmp(right.as_bytes()));
        // Both sides are now in the same order, so the spliced shard can be checked against the
        // assembly in place instead of collecting a second parsed key vector.
        let matches_assembly = packed.len() == assembly[shard_id].len()
            && packed
                .keys()
                .zip(&assembly[shard_id])
                .all(|(key, expected)| StaticRecordKey::parse(key) == *expected);
        if !matches_assembly {
            return Err(StaticsFallbackReason::AssemblyMismatch);
        }
        let (subset_count, vertex_count, triangle_count) = summarize_distant_statics(&packed);
        let (merged_record_count, merged_vertex_count, merged_triangle_count) = summarize_merged_distant_statics(&packed);
        let state = StaticShardState {
            input_digest: fingerprint_static_mesh_shard_inputs(shard_id, &packed),
            record_count: u32::try_from(packed.len()).map_err(|_| StaticsFallbackReason::AssemblyMismatch)?,
            subset_count: subset_count as u64,
            vertex_count: vertex_count as u64,
            triangle_count: triangle_count as u64,
            merged_record_count: merged_record_count as u64,
            merged_vertex_count: merged_vertex_count as u64,
            merged_triangle_count: merged_triangle_count as u64,
            records: assembly[shard_id].clone(),
        };
        let carry = state == previous_shards[shard_id] && base_shards[shard_id].is_some();
        states[shard_id] = state;
        shard_plans.push(if carry {
            StaticShardPlan::Carry(base_shards[shard_id].expect("checked base shard").clone())
        } else {
            StaticShardPlan::Fresh(packed)
        });
    }

    let ordered_keys = assembly
        .iter()
        .flat_map(|shard| shard.iter().map(StaticRecordKey::render))
        .collect_vec();
    let ordinals = crate::usage::StaticOrdinalView::from_ordered_keys(ordered_keys);
    let final_counts = states.iter().fold((0usize, 0usize, 0usize), |counts, state| {
        (
            counts.0 + state.subset_count as usize,
            counts.1 + state.vertex_count as usize,
            counts.2 + state.triangle_count as usize,
        )
    });
    let merged_counts = states.iter().fold((0usize, 0usize, 0usize), |counts, state| {
        (
            counts.0 + state.merged_record_count as usize,
            counts.1 + state.merged_vertex_count as usize,
            counts.2 + state.merged_triangle_count as usize,
        )
    });
    Ok(SpliceResult {
        static_mesh_shards: states,
        shards: shard_plans,
        ordinals,
        final_counts,
        merged_counts,
        stats,
    })
}
