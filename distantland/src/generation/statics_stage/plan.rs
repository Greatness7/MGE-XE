//! Static owner planner: dirty-owner closure and dirty-shard derivation for incremental static rebuilds.

use std::collections::BTreeSet;

use hashbrown::HashSet;

use crate::AtlasTextureSet;
use crate::generation::cache::static_mesh_shard_id;
use crate::generation::output::STATIC_MESH_SHARD_COUNT;
use crate::generation::record_key::StaticRecordKey;
use crate::generation::state_db::{GenerationDiff, MergeReverseIndex, PathReverseIndex, ReverseIndexes, StaticShardState};
use crate::generation::units::MergeUnitKey;
use crate::statics::atlas::BindingDelta;

/// Stable snake_case reasons an owner-partial static rebuild degrades to a full rebuild.
///
/// Every code is surfaced in `StaticsReuseMetrics::reuse_fallback_reason`. The variants span the
/// whole owner-partial flow: precondition checks (this module), dirty-shard readback, and
/// the environmental gates the stage itself applies before planning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StaticsFallbackReason {
    /// No comparable previous state (absent, corrupt, version mismatch, or forced).
    NoComparablePreviousState,
    /// The run forced a full rebuild.
    ForceRebuild,
    /// The cache is being treated as absent/incompatible (migration).
    Migration,
    /// The record-production recipe digest changed.
    RecordRecipeChanged,
    /// An atlas family binding delta was unavailable (no accepted prior evidence).
    BindingDeltaUnavailable,
    /// A required base shard or the base usage artifact was missing from the inventory.
    BaseArtifactMissing,
    /// A binding add/remove implied a mesh change that the diff did not report.
    BindingConsumerInvariantViolated,
    /// A dirty prior shard file could not be read.
    ShardUnreadable,
    /// A dirty prior shard failed its full content hash.
    ShardHashMismatch,
    /// A dirty prior shard failed structural/header validation.
    ShardHeaderInvalid,
    /// A dirty prior shard's record count disagreed with its persisted key list.
    ShardCountMismatch,
    /// A dirty prior shard could not be decoded.
    ShardDecodeFailed,
    /// A clean ordinary owner's current filter outcome disagreed with its persisted presence.
    CleanOwnerMismatch,
    /// A persisted merged key had no matching currently-planned group.
    CleanCellGroupMismatch,
    /// The spliced per-shard key vector disagreed with the authoritative assembly.
    AssemblyMismatch,
}

impl StaticsFallbackReason {
    /// Returns the stable snake_case metrics code for this reason.
    pub(super) fn code(self) -> &'static str {
        match self {
            Self::NoComparablePreviousState => "no_comparable_previous_state",
            Self::ForceRebuild => "force_rebuild",
            Self::Migration => "migration",
            Self::RecordRecipeChanged => "record_recipe_changed",
            Self::BindingDeltaUnavailable => "binding_delta_unavailable",
            Self::BaseArtifactMissing => "base_artifact_missing",
            Self::BindingConsumerInvariantViolated => "binding_consumer_invariant_violated",
            Self::ShardUnreadable => "shard_unreadable",
            Self::ShardHashMismatch => "shard_hash_mismatch",
            Self::ShardHeaderInvalid => "shard_header_invalid",
            Self::ShardCountMismatch => "shard_count_mismatch",
            Self::ShardDecodeFailed => "shard_decode_failed",
            Self::CleanOwnerMismatch => "clean_owner_mismatch",
            Self::CleanCellGroupMismatch => "clean_cell_group_mismatch",
            Self::AssemblyMismatch => "assembly_mismatch",
        }
    }
}

/// The dirty-owner closure and dirty-shard set for an owner-partial static rebuild.
///
/// The four owner sets are unordered: every consumer either membership-tests them or iterates
/// them into another set, so no traversal order reaches output. `dirty_shards` stays ordered
/// because it is small, bounded by [`STATIC_MESH_SHARD_COUNT`], and read as a shard sequence.
pub(super) struct OwnerPlan {
    /// Ordinary owners whose records must be repacked (content-dirty ∪ binding-dirty consumers).
    pub(super) dirty_meshes: HashSet<String>,
    /// Ordinary owners removed since the previous state.
    pub(super) removed_meshes: HashSet<String>,
    /// Exterior cells whose merged geometry must be rebuilt.
    pub(super) dirty_cells: HashSet<MergeUnitKey>,
    /// Exterior cells whose merge units were removed since the previous state.
    pub(super) removed_cells: HashSet<MergeUnitKey>,
    /// Shard indices that must be opened, spliced, and rewritten. Everything else is carried.
    pub(super) dirty_shards: BTreeSet<usize>,
}

/// Either a complete owner-partial plan or a fail-closed reason to fall back to a full rebuild.
pub(super) enum OwnerPlanOutcome {
    /// A safe owner-partial plan.
    Plan(OwnerPlan),
    /// Fall back to a full rebuild for this reason.
    Fallback(StaticsFallbackReason),
}

/// Dirty ordinary-mesh and merge-cell closure shared by selective optimization and owner planning.
///
/// Unordered for the same reason as [`OwnerPlan`]: these sets are only membership-tested or
/// folded into other sets.
pub(super) struct OwnerDirt {
    pub(super) dirty_meshes: HashSet<String>,
    pub(super) removed_meshes: HashSet<String>,
    pub(super) dirty_cells: HashSet<MergeUnitKey>,
    pub(super) removed_cells: HashSet<MergeUnitKey>,
}

/// Closes mesh, atlas-binding, and merge-cell dirt without inspecting shard records.
pub(super) fn close_owner_dirt(
    diff: &GenerationDiff,
    binding_deltas: &AtlasTextureSet<Option<BindingDelta>>,
    current_reverse: &ReverseIndexes,
    previous_reverse: &ReverseIndexes,
) -> Result<OwnerDirt, StaticsFallbackReason> {
    if !diff.comparable {
        return Err(StaticsFallbackReason::NoComparablePreviousState);
    }
    let (Some(opaque), Some(alpha)) = (binding_deltas.opaque.as_ref(), binding_deltas.alpha.as_ref()) else {
        return Err(StaticsFallbackReason::BindingDeltaUnavailable);
    };

    let mut dirty_meshes: HashSet<String> = diff.mesh_added.iter().chain(&diff.mesh_changed).cloned().collect();
    for key in opaque.changed.iter().chain(&alpha.changed) {
        dirty_meshes.extend(path_consumers_of(&current_reverse.texture_to_mesh, key).iter().cloned());
    }
    let removed_meshes: HashSet<String> = diff.mesh_removed.iter().cloned().collect();

    for key in opaque.added.iter().chain(&alpha.added) {
        if path_consumers_of(&current_reverse.texture_to_mesh, key)
            .iter()
            .any(|consumer| !dirty_meshes.contains(consumer))
        {
            return Err(StaticsFallbackReason::BindingConsumerInvariantViolated);
        }
    }
    for key in opaque.removed.iter().chain(&alpha.removed) {
        if path_consumers_of(&previous_reverse.texture_to_mesh, key)
            .iter()
            .any(|consumer| !dirty_meshes.contains(consumer) && !removed_meshes.contains(consumer))
        {
            return Err(StaticsFallbackReason::BindingConsumerInvariantViolated);
        }
    }

    let mut dirty_cells: HashSet<MergeUnitKey> = diff.merge_added.iter().chain(&diff.merge_changed).copied().collect();
    for mesh in &dirty_meshes {
        dirty_cells.extend(merge_consumers_of(&current_reverse.mesh_to_merge, mesh).iter().copied());
    }

    Ok(OwnerDirt {
        dirty_meshes,
        removed_meshes,
        dirty_cells,
        removed_cells: diff.merge_removed.iter().copied().collect(),
    })
}

/// Computes the dirty-owner closure, the fail-closed binding invariants, and the dirty-shard set.
///
/// `current_merged_keys` are the [`StaticRecordKey::Merged`] keys of every group in the current
/// merge plan; they finalize the dirty-shard set for cells whose group structure changed. The
/// caller is responsible for the environmental gates (force/migration/previous present/base
/// artifacts present) before calling this function.
pub(super) fn plan_static_owners(
    diff: &GenerationDiff,
    binding_deltas: &AtlasTextureSet<Option<BindingDelta>>,
    current_reverse: &ReverseIndexes,
    previous_reverse: &ReverseIndexes,
    previous_shards: &[StaticShardState; STATIC_MESH_SHARD_COUNT],
    current_merged_keys: &[StaticRecordKey],
    record_global_current: &[u8; 32],
    record_global_previous: &[u8; 32],
) -> OwnerPlanOutcome {
    use OwnerPlanOutcome::Fallback;

    if !diff.comparable {
        return Fallback(StaticsFallbackReason::NoComparablePreviousState);
    }
    if record_global_current != record_global_previous {
        return Fallback(StaticsFallbackReason::RecordRecipeChanged);
    }
    let OwnerDirt {
        dirty_meshes,
        removed_meshes,
        dirty_cells,
        removed_cells,
    } = match close_owner_dirt(diff, binding_deltas, current_reverse, previous_reverse) {
        Ok(dirt) => dirt,
        Err(reason) => return Fallback(reason),
    };

    // Dirty shards: (a) prior records owned by dirty/removed owners, (b) dirty mesh owners' current
    // shard targets (covering added owners with no prior record), and (c) current merged keys of
    // dirty cells (covering cells whose group structure changed).
    let mut dirty_shards = BTreeSet::new();
    for (shard_index, shard) in previous_shards.iter().enumerate() {
        let owner_dirty = shard
            .records
            .iter()
            .any(|record| record_owner_is_dirty(record, &dirty_meshes, &removed_meshes, &dirty_cells, &removed_cells));
        if owner_dirty {
            dirty_shards.insert(shard_index);
        }
    }
    for mesh in &dirty_meshes {
        dirty_shards.insert(static_mesh_shard_id(mesh));
    }
    for key in current_merged_keys {
        if let StaticRecordKey::Merged { cell_x, cell_y, .. } = key
            && dirty_cells.contains(&MergeUnitKey::new(*cell_x, *cell_y))
        {
            dirty_shards.insert(key.shard_id());
        }
    }

    OwnerPlanOutcome::Plan(OwnerPlan {
        dirty_meshes,
        removed_meshes,
        dirty_cells,
        removed_cells,
        dirty_shards,
    })
}

/// Whether a persisted record's owner is in the dirty or removed set for its owner class.
fn record_owner_is_dirty(
    record: &StaticRecordKey,
    dirty_meshes: &HashSet<String>,
    removed_meshes: &HashSet<String>,
    dirty_cells: &HashSet<MergeUnitKey>,
    removed_cells: &HashSet<MergeUnitKey>,
) -> bool {
    match record {
        StaticRecordKey::Mesh { id: mesh } => dirty_meshes.contains(mesh) || removed_meshes.contains(mesh),
        StaticRecordKey::Merged { cell_x, cell_y, .. } => {
            let cell = MergeUnitKey::new(*cell_x, *cell_y);
            dirty_cells.contains(&cell) || removed_cells.contains(&cell)
        }
    }
}

/// Binary-searches a sorted path reverse index for `key`'s consumers, returning an empty slice if absent.
///
/// Reverse indexes are stored sorted ascending by outer key (`validate_reverse_index`), so the
/// search comparator matches the persisted order.
fn path_consumers_of<'a>(index: &'a PathReverseIndex, key: &str) -> &'a [String] {
    match index.binary_search_by(|(entry_key, _)| entry_key.as_str().cmp(key)) {
        Ok(position) => &index[position].1,
        Err(_) => &[],
    }
}

/// Binary-searches a sorted mesh→merge reverse index for typed merge-cell consumers.
fn merge_consumers_of<'a>(index: &'a MergeReverseIndex, key: &str) -> &'a [MergeUnitKey] {
    match index.binary_search_by(|(entry_key, _)| entry_key.as_str().cmp(key)) {
        Ok(position) => &index[position].1,
        Err(_) => &[],
    }
}

#[cfg(test)]
mod tests;
