//! Static-bundle decision and version-16 sharded publication.

use std::thread::JoinHandle;
use std::time::Instant;

use anyhow::Result;
use hashbrown::HashSet;
use itertools::Itertools;
use rayon::prelude::*;
use tracing::{Span, field, info_span};

use crate::{
    AtlasTextureSet, CellFilter, DistantStatics, StaticOverrides, SubterrainCull, UsageInfo, Vfs, apply_merge_usage,
    build_merge_geometry, plan_exterior_merge_groups,
};

use super::cache::{fingerprint_static_mesh_shard_inputs, static_mesh_shard_id};
use super::metrics::{CacheMetadata, GenerationMetrics, StaticReuseMode, summarize_distant_statics};
use super::output::{STATIC_MESH_SHARD_COUNT, static_mesh_shard_relative_path};
use super::record_key::{StaticRecordKey, parse_merged};
use super::state_db::{GenerationDiff, GenerationState, OptimizedMeshBounds, StaticShardState};
use super::storage::durable::SyncClass;
use super::storage::state::{ArtifactKind, RequiredArtifact, artifact_from_written};
use super::{
    GenerationStage, OutputWriteDecision, ProgressReporter, PublicationWrites, StageContext, record_usize, run_stage,
};

mod decode;
mod plan;
mod splice;

use decode::decode_dirty_shards;
use plan::{OwnerDirt, OwnerPlanOutcome, StaticsFallbackReason, close_owner_dirt, plan_static_owners};
use splice::{build_static_assembly, owner_key_is_dirty, splice_static_shards};

/// Prefer a full meshopt pass when at least this fraction of ordinary statics need optimization.
const SELECTIVE_OPTIMIZE_FULL_NUMERATOR: usize = 3;
const SELECTIVE_OPTIMIZE_FULL_DENOMINATOR: usize = 4;

pub(super) type StaticMeshesWrite = JoinHandle<Result<Vec<RequiredArtifact>>>;

enum StaticShardPlan {
    Carry(RequiredArtifact),
    Fresh(crate::PackedDistantStatics),
}

enum StaticsPublishMode {
    Reuse {
        usage: RequiredArtifact,
        shards: Vec<RequiredArtifact>,
    },
    Rebuild {
        usage_bytes: Vec<u8>,
        previous_usage: Option<RequiredArtifact>,
        shards: Vec<StaticShardPlan>,
        final_counts: (usize, usize, usize),
    },
}

/// Complete static-bundle decision and prepared miss inputs.
pub(super) struct StaticsStagePlan {
    static_mesh_shards: [StaticShardState; STATIC_MESH_SHARD_COUNT],
    optimized_mesh_bounds: Vec<(String, OptimizedMeshBounds)>,
    mode: StaticsPublishMode,
}

/// Synchronous publication results plus an optional background payload writer.
pub(super) struct StaticsPublishResult {
    pub(super) usage: RequiredArtifact,
    pub(super) shards: Vec<RequiredArtifact>,
    pub(super) writer: Option<StaticMeshesWrite>,
}

fn optimized_bounds_for<'a>(bounds: &'a [(String, OptimizedMeshBounds)], mesh_key: &str) -> Option<&'a OptimizedMeshBounds> {
    bounds
        .binary_search_by(|(key, _)| key.as_str().cmp(mesh_key))
        .ok()
        .map(|index| &bounds[index].1)
}

fn collect_dirty_cell_partners(
    usage_info: &UsageInfo<'_>,
    distant_statics: &DistantStatics,
    dirty_cells: &HashSet<super::units::MergeUnitKey>,
) -> HashSet<String> {
    let mut partners = HashSet::new();
    let Some(references) = usage_info.exterior_references() else {
        return partners;
    };
    for reference in references.values() {
        if reference.vis_index != 0
            || !dirty_cells.contains(&super::units::MergeUnitKey::new(
                reference.cell_coords().0,
                reference.cell_coords().1,
            ))
        {
            continue;
        }
        let Some(distant_static) = distant_statics.get(reference.id.as_ref()) else {
            continue;
        };
        if distant_static.static_type != crate::mge_xe::distant_statics::StaticType::StaticGrass
            && distant_static.bounding_sphere.radius * reference.scale >= 32.0
        {
            partners.insert(reference.id.to_string());
        }
    }
    partners
}

fn restore_optimized_bounds(
    distant_statics: &mut DistantStatics,
    optimized_meshes: &HashSet<String>,
    previous_bounds: &[(String, OptimizedMeshBounds)],
) {
    for (mesh_key, distant_static) in distant_statics {
        if optimized_meshes.contains(mesh_key) {
            continue;
        }
        let bounds = optimized_bounds_for(previous_bounds, mesh_key)
            .expect("selective optimize set includes every mesh missing cached bounds");
        distant_static.bounding_sphere.center = glam::Vec3::from_array(bounds.center);
        distant_static.bounding_sphere.radius = bounds.radius;
        distant_static.bounding_box.min = glam::Vec3::from_array(bounds.box_min);
        distant_static.bounding_box.max = glam::Vec3::from_array(bounds.box_max);
    }
}

fn capture_optimized_bounds(distant_statics: &DistantStatics) -> Vec<(String, OptimizedMeshBounds)> {
    distant_statics
        .iter()
        .filter(|(key, _)| parse_merged(key).is_none())
        .map(|(key, distant_static)| {
            (
                key.clone(),
                OptimizedMeshBounds {
                    center: distant_static.bounding_sphere.center.to_array(),
                    radius: distant_static.bounding_sphere.radius,
                    box_min: distant_static.bounding_box.min.to_array(),
                    box_max: distant_static.bounding_box.max.to_array(),
                },
            )
        })
        .sorted_unstable_by(|left, right| left.0.cmp(&right.0))
        .collect_vec()
}

fn remaining_unoptimized_meshes(distant_statics: &DistantStatics, optimized_meshes: &HashSet<String>) -> HashSet<String> {
    distant_statics
        .keys()
        .filter(|key| parse_merged(key).is_none() && !optimized_meshes.contains(*key))
        .cloned()
        .collect()
}

fn record_optimize_metrics(
    cache: &mut CacheMetadata,
    initial_static_count: usize,
    optimized_meshes: &HashSet<String>,
    selective_optimize: bool,
) {
    cache.static_shards.optimized_static_count = optimized_meshes.len();
    cache.static_shards.skipped_optimize_static_count = initial_static_count.saturating_sub(optimized_meshes.len());
    cache.static_shards.selective_optimize = selective_optimize;
}

/// Removes statics that no surviving placement can select, returning the removed entries.
///
/// This must run only after merged geometry has consumed its original member meshes.
///
/// The removed entries are handed back rather than dropped because the owner-partial attempt can
/// still degrade to the full rebuild after calling this, and that rebuild's merge pass looks up
/// every group member's source mesh. A merge swallows all of those meshes out of the usage table.
fn retain_referenced_statics(distant_statics: &mut DistantStatics, usage_info: &UsageInfo<'_>) -> DistantStatics {
    let referenced_ids: HashSet<&str> = usage_info
        .cells
        .values()
        .flat_map(|references| references.values())
        .map(|reference| reference.id.as_ref())
        .collect();
    distant_statics
        .extract_if(.., |key, _| !referenced_ids.contains(key.as_str()))
        .collect()
}

fn reconcile_usage_with_ordinals(
    usage_info: &mut UsageInfo<'_>,
    ordinals: &crate::usage::StaticOrdinalView,
    metrics: &mut GenerationMetrics,
) {
    usage_info.discard_unused_references(|key| ordinals.get_index_of(key).is_some());
    metrics.usage.total_reference_count_after_merge = usage_info.total_references_count();
    metrics.usage.exterior_reference_count_after_merge = usage_info.exterior_references_count();
}

/// Runs the `ConvertStatics` stage, packing `statics` and recording the result's totals.
///
/// Both publication paths convert through here. The owner-partial path handles its dirty records
/// only, while the full path handles everything, so their stage span and its four recorded counts cannot
/// drift apart.
fn convert_statics_stage(
    statics: DistantStatics,
    vfs: &Vfs,
    door_size_multiplier: f32,
    reporter: &mut dyn ProgressReporter,
) -> Result<crate::PackedDistantStatics> {
    run_stage(GenerationStage::ConvertStatics, reporter, || {
        let stage_span = info_span!(
            "stage.convert_statics",
            report = true,
            generated_static_count = field::Empty,
            subset_count = field::Empty,
            vertex_count = field::Empty,
            triangle_count = field::Empty,
        );
        let _stage_guard = stage_span.enter();
        let packed = finalize_distant_statics(statics, vfs, door_size_multiplier);
        let (subset_count, vertex_count, triangle_count) = summarize_distant_statics(&packed);
        record_usize(&stage_span, "generated_static_count", packed.len());
        record_usize(&stage_span, "subset_count", subset_count);
        record_usize(&stage_span, "vertex_count", vertex_count);
        record_usize(&stage_span, "triangle_count", triangle_count);
        Ok(packed)
    })
}

impl StaticsStagePlan {
    pub(super) fn static_mesh_shards(&self) -> [StaticShardState; STATIC_MESH_SHARD_COUNT] {
        self.static_mesh_shards.clone()
    }

    pub(super) fn optimized_mesh_bounds(&self) -> Vec<(String, OptimizedMeshBounds)> {
        self.optimized_mesh_bounds.clone()
    }

    pub(super) fn is_hit(&self) -> bool {
        matches!(self.mode, StaticsPublishMode::Reuse { .. })
    }

    pub(super) fn publication_bytes_estimate(&self, previous_size: u64) -> u64 {
        match &self.mode {
            StaticsPublishMode::Reuse { .. } => 0,
            StaticsPublishMode::Rebuild { usage_bytes, shards, .. } => {
                let payload = if shards.iter().any(|shard| matches!(shard, StaticShardPlan::Fresh(_))) {
                    previous_size.saturating_mul(5).div_ceil(4).max(512 * 1024 * 1024)
                } else {
                    0
                };
                payload.saturating_add(usage_bytes.len() as u64)
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_statics_bundle(
    ctx: &StageContext<'_>,
    generation_span: &Span,
    static_bundle_fingerprint: [u8; 32],
    current_state: &GenerationState,
    previous_state: Option<&GenerationState>,
    diff: &GenerationDiff,
    binding_deltas: &AtlasTextureSet<Option<crate::statics::atlas::BindingDelta>>,
    base_shards: &[Option<&RequiredArtifact>; STATIC_MESH_SHARD_COUNT],
    base_usage: Option<&RequiredArtifact>,
    overrides: &StaticOverrides,
    usage_info: &mut UsageInfo<'_>,
    mut distant_statics: DistantStatics,
    metrics: &mut GenerationMetrics,
    cache: &mut CacheMetadata,
    reporter: &mut dyn ProgressReporter,
) -> Result<StaticsStagePlan> {
    let StageContext { job, vfs, migration, .. } = *ctx;
    let force_rebuild = ctx.force_rebuild();
    let static_bundle_hit = !migration
        && !force_rebuild
        && previous_state.is_some_and(|state| state.statics_domain_digest == static_bundle_fingerprint)
        && base_shards.iter().all(Option::is_some)
        && base_usage.is_some();
    generation_span.record("static_bundle_hit", static_bundle_hit);
    cache.static_bundle_hit = static_bundle_hit;
    if static_bundle_hit {
        let state = previous_state.expect("state hit has generation state");
        cache.static_shards.reuse_mode = StaticReuseMode::CarriedBundle;
        cache.static_shards.shard_count = STATIC_MESH_SHARD_COUNT;
        cache.static_shards.carried_shard_count = STATIC_MESH_SHARD_COUNT;
        return Ok(StaticsStagePlan {
            static_mesh_shards: state.static_mesh_shards.clone(),
            optimized_mesh_bounds: state.optimized_mesh_bounds.clone(),
            mode: StaticsPublishMode::Reuse {
                usage: base_usage.expect("state hit has usage inventory").clone(),
                shards: base_shards
                    .iter()
                    .map(|entry| entry.expect("state hit has shard inventory").clone())
                    .collect(),
            },
        });
    }

    let environmental_fallback = if force_rebuild {
        Some(StaticsFallbackReason::ForceRebuild)
    } else if migration {
        Some(StaticsFallbackReason::Migration)
    } else if previous_state.is_none() || !diff.comparable {
        Some(StaticsFallbackReason::NoComparablePreviousState)
    } else if base_usage.is_none() || base_shards.iter().any(Option::is_none) {
        Some(StaticsFallbackReason::BaseArtifactMissing)
    } else {
        None
    };

    let cfg = job.settings.static_mesh_simplifier_config();
    let initial_static_count = distant_statics.len();
    let previous_bounds = previous_state.map_or(&[][..], |state| state.optimized_mesh_bounds.as_slice());
    let selective_keys = environmental_fallback.is_none().then(|| {
        let previous = previous_state.expect("selective optimize requires previous state");
        if current_state.statics_record_global_digest != previous.statics_record_global_digest {
            return None;
        }
        let OwnerDirt {
            dirty_meshes,
            dirty_cells,
            ..
        } = close_owner_dirt(diff, binding_deltas, &current_state.reverse, &previous.reverse).ok()?;
        let mut keys = dirty_meshes;
        keys.retain(|key| distant_statics.contains_key(key));
        keys.extend(collect_dirty_cell_partners(usage_info, &distant_statics, &dirty_cells));
        for key in distant_statics.keys() {
            if !keys.contains(key) && optimized_bounds_for(previous_bounds, key).is_none() {
                keys.insert(key.clone());
            }
        }
        Some(keys)
    });
    let selective_keys = selective_keys.flatten().filter(|keys| {
        initial_static_count != 0
            && keys.len().saturating_mul(SELECTIVE_OPTIMIZE_FULL_DENOMINATOR)
                < initial_static_count.saturating_mul(SELECTIVE_OPTIMIZE_FULL_NUMERATOR)
    });
    let selective_optimize = selective_keys.is_some();
    let mut optimized_meshes = selective_keys
        .clone()
        .unwrap_or_else(|| distant_statics.keys().cloned().collect());
    run_stage(GenerationStage::OptimizeMeshes, reporter, || {
        let _stage_guard = info_span!(
            "stage.optimize_meshes",
            report = true,
            selective = selective_optimize,
            optimized_count = optimized_meshes.len() as u64,
            skipped_count = initial_static_count.saturating_sub(optimized_meshes.len()) as u64,
        )
        .entered();
        if selective_optimize {
            crate::statics::model::optimize_statics_keys(&mut distant_statics, &optimized_meshes, cfg);
            restore_optimized_bounds(&mut distant_statics, &optimized_meshes, previous_bounds);
        } else {
            crate::statics::model::optimize_statics(&mut distant_statics, cfg);
        }
        Ok(())
    })?;
    {
        let span = info_span!(
            "statics.size_filter",
            report = true,
            requested_mesh_count = distant_statics.len() as u64,
            generated_static_count = field::Empty,
        );
        let _guard = span.enter();
        distant_statics.retain(|_, distant_static| {
            crate::passes_static_min_radius(
                distant_static,
                job.settings.min_static_size,
                job.settings.door_size_multiplier,
            )
        });
        record_usize(&span, "generated_static_count", distant_statics.len());
    }
    metrics.statics.after_size_filter_static_count = distant_statics.len();
    let mut optimized_mesh_bounds = capture_optimized_bounds(&distant_statics);
    // Merged geometry below the rendered ground is never visible, so it is trimmed during the
    // merge. The terrain mesh is simplified against an absolute error budget, so that budget is
    // the slack the cull must leave below the sampled LAND heights.
    let subterrain_margin = job.settings.terrain_detail.target_error();
    let merge_plan = plan_exterior_merge_groups(&distant_statics, usage_info, job.settings.merge_group_radius);
    let planned_merged_keys = merge_plan.merged_record_keys().collect_vec();
    cache.static_shards.merge_groups_planned = merge_plan.group_count();
    apply_merge_usage(&merge_plan, usage_info);
    usage_info.sort_for_deterministic_output();

    let owner_outcome = environmental_fallback.map_or_else(
        || {
            let previous = previous_state.expect("owner planning requires previous state");
            plan_static_owners(
                diff,
                binding_deltas,
                &current_state.reverse,
                &previous.reverse,
                &previous.static_mesh_shards,
                &planned_merged_keys,
                &current_state.statics_record_global_digest,
                &previous.statics_record_global_digest,
            )
        },
        OwnerPlanOutcome::Fallback,
    );

    // The owner-partial attempt either returns a finished plan or yields the reason it degraded to
    // the full rebuild below. A `break 'partial` is a degradation; `?` is a genuine failure.
    let mut merged_away_statics = DistantStatics::default();
    let fallback_reason: Option<StaticsFallbackReason> = match owner_outcome {
        OwnerPlanOutcome::Fallback(reason) => Some(reason),
        OwnerPlanOutcome::Plan(owners) => 'partial: {
            // Reaching a plan at all establishes that a previous state exists.
            let previous = previous_state.expect("owner plan requires previous state");
            cache.static_shards.dirty_mesh_count = owners.dirty_meshes.len();
            cache.static_shards.dirty_cell_count = owners.dirty_cells.len();
            let decode_span = info_span!(
                "statics.shard_decode_splice",
                report = true,
                dirty_shard_count = owners.dirty_shards.len() as u64,
                prior_bytes_read = field::Empty,
            );
            let decode_guard = decode_span.enter();
            let decoded = match decode_dirty_shards(
                &ctx.output_paths.static_mesh_shard_paths,
                base_shards,
                &owners.dirty_shards,
                &previous.static_mesh_shards,
            ) {
                Ok(decoded) => decoded,
                Err(reason) => break 'partial Some(reason),
            };
            decode_span.record("prior_bytes_read", decoded.bytes_read);
            cache.static_shards.validated_shard_count = owners.dirty_shards.len();
            cache.static_shards.decoded_shard_count = owners.dirty_shards.len();
            drop(decode_guard);

            let dirty_cells = owners.dirty_cells.iter().map(|cell| (cell.x, cell.y)).collect();
            let merge_span = merge_geometry_span();
            let (partial_merge_metrics, merged_statics) = {
                let _guard = merge_span.enter();
                let built = build_merge_geometry(
                    &merge_plan,
                    &CellFilter::Dirty(dirty_cells),
                    &distant_statics,
                    cfg,
                    Some(SubterrainCull::new(&usage_info.terrain_cells, subterrain_margin)),
                    vfs,
                    job.settings.door_size_multiplier,
                );
                record_merged_pack_metrics(&merge_span, &built.1);
                built
            };
            cache.static_shards.merge_groups_geometry_built = partial_merge_metrics.group_count;
            cache.static_shards.lod_cache_entries_built = partial_merge_metrics.lod_cache_entry_count;
            merged_away_statics = retain_referenced_statics(&mut distant_statics, usage_info);

            // The merged records this pass built are already packed, so the assembly is told which
            // keys they were instead of reading them back out of `distant_statics`.
            let merged_keys = merged_statics.keys().map(|key| StaticRecordKey::parse(key)).collect_vec();
            let assembly = match build_static_assembly(
                &owners,
                &distant_statics,
                &merged_keys,
                &planned_merged_keys,
                &previous.static_mesh_shards,
            ) {
                Ok(assembly) => assembly,
                Err(reason) => break 'partial Some(reason),
            };

            // This must stay a clone, not an `extract_if` move: `splice_static_shards` below can
            // still `break 'partial` into the full rebuild, which re-runs `build_merge_geometry`
            // under `CellFilter::All` and looks every planned member up in `distant_statics`
            // (`debug_assert!` then `.unwrap()`). The only restore on that path,
            // `distant_statics.extend(merged_away_statics)`, puts back records trimmed by
            // `retain_referenced_statics` - not dirty surviving records. Moving these out would
            // panic the fallback in release.
            let dirty_statics: DistantStatics = distant_statics
                .iter()
                .filter(|(key, distant_static)| !distant_static.subsets.is_empty() && owner_key_is_dirty(key, &owners))
                .map(|(key, distant_static)| (key.clone(), distant_static.clone()))
                .collect();
            let mut fresh_records = convert_statics_stage(dirty_statics, vfs, job.settings.door_size_multiplier, reporter)?;
            // Every group built above sits in a dirty cell by construction, so all of these records
            // are dirty by `owner_key_is_dirty` - the same set the clone used to carry. Order does
            // not matter: `splice_static_shards` re-sorts each shard by key bytes.
            fresh_records.extend(merged_statics);

            let spliced = match splice_static_shards(
                &owners,
                assembly,
                decoded,
                fresh_records,
                &previous.static_mesh_shards,
                base_shards,
            ) {
                Ok(spliced) => spliced,
                Err(reason) => break 'partial Some(reason),
            };

            reconcile_usage_with_ordinals(usage_info, &spliced.ordinals, metrics);
            let usage_bytes = crate::usage::serialize_usage_data(
                usage_info,
                &spliced.ordinals,
                &overrides.dynamic_vis,
                job.settings.min_static_size,
            )?;
            let written_shard_ids = spliced
                .shards
                .iter()
                .enumerate()
                .filter_map(|(id, shard)| matches!(shard, StaticShardPlan::Fresh(_)).then_some(id as u8))
                .collect_vec();
            cache.static_shards.reuse_mode = StaticReuseMode::OwnerPartial;
            cache.static_shards.shard_count = STATIC_MESH_SHARD_COUNT;
            cache.static_shards.carried_shard_count = STATIC_MESH_SHARD_COUNT - written_shard_ids.len();
            cache.static_shards.written_shard_count = written_shard_ids.len();
            cache.static_shards.written_shard_ids = written_shard_ids;
            cache.static_shards.ordinary_records_carried = spliced.stats.ordinary_carried;
            cache.static_shards.ordinary_records_rebuilt = spliced.stats.ordinary_rebuilt;
            cache.static_shards.ordinary_records_added = spliced.stats.ordinary_added;
            cache.static_shards.ordinary_records_removed = spliced.stats.ordinary_removed;
            cache.static_shards.merged_records_carried = spliced.stats.merged_carried;
            cache.static_shards.merged_records_rebuilt = spliced.stats.merged_rebuilt;
            cache.static_shards.merged_records_added = spliced.stats.merged_added;
            cache.static_shards.merged_records_removed = spliced.stats.merged_removed;
            metrics.statics.merge_simplification = partial_merge_metrics;
            metrics.statics.after_merge_static_count = spliced.ordinals.len();
            metrics.statics.final_static_count = spliced.ordinals.len();
            metrics.statics.final_subset_count = spliced.final_counts.0;
            metrics.statics.final_vertex_count = spliced.final_counts.1;
            metrics.statics.final_triangle_count = spliced.final_counts.2;
            record_optimize_metrics(cache, initial_static_count, &optimized_meshes, selective_optimize);
            return Ok(StaticsStagePlan {
                static_mesh_shards: spliced.static_mesh_shards,
                optimized_mesh_bounds,
                mode: StaticsPublishMode::Rebuild {
                    usage_bytes,
                    previous_usage: base_usage.cloned(),
                    shards: spliced.shards,
                    final_counts: spliced.final_counts,
                },
            });
        }
    };

    // A degraded owner-partial attempt already trimmed the meshes its merge swallowed, so put them
    // back before the full merge pass below looks them up again. Map order does not matter here:
    // `finalize_distant_statics` sorts records into shard-major key order, and `retain_referenced_statics`
    // drops these same entries again after the rebuild's own merge.
    distant_statics.extend(merged_away_statics);

    let prior_shard_bytes_are_trusted = !matches!(
        fallback_reason,
        Some(
            StaticsFallbackReason::ShardUnreadable
                | StaticsFallbackReason::ShardHashMismatch
                | StaticsFallbackReason::ShardHeaderInvalid
                | StaticsFallbackReason::ShardCountMismatch
                | StaticsFallbackReason::ShardDecodeFailed
        )
    );
    cache.static_shards.reuse_fallback_reason = fallback_reason.map(|reason| reason.code().to_owned());
    cache.static_shards.reuse_mode = if force_rebuild {
        StaticReuseMode::ForcedFull
    } else {
        StaticReuseMode::FullRebuild
    };
    if selective_optimize {
        let remaining_meshes = remaining_unoptimized_meshes(&distant_statics, &optimized_meshes);
        if !remaining_meshes.is_empty() {
            let _guard = info_span!(
                "statics.optimize_remaining",
                report = true,
                optimized_count = remaining_meshes.len() as u64,
            )
            .entered();
            crate::statics::model::optimize_statics_keys(&mut distant_statics, &remaining_meshes, cfg);
            optimized_meshes.extend(remaining_meshes);
        }
        optimized_mesh_bounds = capture_optimized_bounds(&distant_statics);
    }
    record_optimize_metrics(cache, initial_static_count, &optimized_meshes, selective_optimize);
    // Merge geometry runs outside every `run_stage` call, so without its own `stage.*`
    // span the merge LOD builds, subterrain culling, and packing never reach the reported timeline.
    let merge_span = merge_geometry_span();
    let (merge_metrics, merged_statics) = {
        let _guard = merge_span.enter();
        // Every planned member's source mesh must still be present: a degraded owner-partial
        // attempt trims the meshes its merge swallowed, and the restore above is what keeps this
        // pass's lookups total.
        debug_assert!(
            merge_plan.member_ids().all(|id| distant_statics.contains_key(id)),
            "merge plan member missing from statics map"
        );
        let built = build_merge_geometry(
            &merge_plan,
            &CellFilter::All,
            &distant_statics,
            cfg,
            Some(SubterrainCull::new(&usage_info.terrain_cells, subterrain_margin)),
            vfs,
            job.settings.door_size_multiplier,
        );
        record_merged_pack_metrics(&merge_span, &built.1);
        built
    };
    metrics.statics.merge_simplification = merge_metrics;
    cache.static_shards.merge_groups_geometry_built = metrics.statics.merge_simplification.group_count;
    cache.static_shards.lod_cache_entries_built = metrics.statics.merge_simplification.lod_cache_entry_count;
    distant_statics.retain(|_, distant_static| !distant_static.subsets.is_empty());
    retain_referenced_statics(&mut distant_statics, usage_info);
    // Merged records left the unpacked map when they were packed, so the count spans both halves.
    metrics.statics.after_merge_static_count = distant_statics.len() + merged_statics.len();

    let mut distant_statics = convert_statics_stage(distant_statics, vfs, job.settings.door_size_multiplier, reporter)?;
    distant_statics.extend(merged_statics);
    sort_packed_statics(&mut distant_statics);
    let final_counts = summarize_distant_statics(&distant_statics);
    metrics.statics.final_static_count = distant_statics.len();
    metrics.statics.final_subset_count = final_counts.0;
    metrics.statics.final_vertex_count = final_counts.1;
    metrics.statics.final_triangle_count = final_counts.2;

    let current_record_keys: HashSet<_> = distant_statics.keys().map(|key| StaticRecordKey::parse(key)).collect();
    let previous_record_keys: HashSet<_> = previous_state
        .into_iter()
        .flat_map(|state| state.static_mesh_shards.iter())
        .flat_map(|shard| shard.records.iter().cloned())
        .collect();
    cache.static_shards.ordinary_records_rebuilt = current_record_keys
        .iter()
        .filter(|key| matches!(key, StaticRecordKey::Mesh { .. }))
        .count();
    cache.static_shards.merged_records_rebuilt = current_record_keys
        .iter()
        .filter(|key| matches!(key, StaticRecordKey::Merged { .. }))
        .count();
    cache.static_shards.ordinary_records_added = current_record_keys
        .difference(&previous_record_keys)
        .filter(|key| matches!(key, StaticRecordKey::Mesh { .. }))
        .count();
    cache.static_shards.merged_records_added = current_record_keys
        .difference(&previous_record_keys)
        .filter(|key| matches!(key, StaticRecordKey::Merged { .. }))
        .count();
    cache.static_shards.ordinary_records_removed = previous_record_keys
        .difference(&current_record_keys)
        .filter(|key| matches!(key, StaticRecordKey::Mesh { .. }))
        .count();
    cache.static_shards.merged_records_removed = previous_record_keys
        .difference(&current_record_keys)
        .filter(|key| matches!(key, StaticRecordKey::Merged { .. }))
        .count();

    let ordinals = crate::usage::StaticOrdinalView::from_packed(&distant_statics);
    reconcile_usage_with_ordinals(usage_info, &ordinals, metrics);
    let usage_bytes =
        crate::usage::serialize_usage_data(usage_info, &ordinals, &overrides.dynamic_vis, job.settings.min_static_size)?;
    let mut packed_shards: [crate::PackedDistantStatics; STATIC_MESH_SHARD_COUNT] =
        std::array::from_fn(|_| crate::PackedDistantStatics::default());
    for (key, distant_static) in distant_statics {
        packed_shards[static_mesh_shard_id(&key)].insert(key, distant_static);
    }

    let mut static_mesh_shards: [StaticShardState; STATIC_MESH_SHARD_COUNT] =
        std::array::from_fn(|_| StaticShardState::default());
    let mut shards = Vec::with_capacity(STATIC_MESH_SHARD_COUNT);
    let can_carry_shards = !force_rebuild && !migration && prior_shard_bytes_are_trusted;
    for (shard_id, packed) in packed_shards.into_iter().enumerate() {
        let (subset_count, vertex_count, triangle_count) = summarize_distant_statics(&packed);
        let shard_state = StaticShardState {
            input_digest: fingerprint_static_mesh_shard_inputs(shard_id, &packed),
            record_count: u32::try_from(packed.len())?,
            subset_count: subset_count as u64,
            vertex_count: vertex_count as u64,
            triangle_count: triangle_count as u64,
            records: packed.keys().map(|key| StaticRecordKey::parse(key)).collect(),
        };
        // Compare (and thus borrow) `shard_state` before moving it into the shard-state array.
        let carried = can_carry_shards
            && previous_state.is_some_and(|state| state.static_mesh_shards[shard_id] == shard_state)
            && base_shards[shard_id].is_some();
        shards.push(if carried {
            StaticShardPlan::Carry(base_shards[shard_id].expect("checked shard inventory").clone())
        } else {
            StaticShardPlan::Fresh(packed)
        });
        static_mesh_shards[shard_id] = shard_state;
    }

    let written_shard_ids = shards
        .iter()
        .enumerate()
        .filter_map(|(shard_id, shard)| matches!(shard, StaticShardPlan::Fresh(_)).then_some(shard_id as u8))
        .collect_vec();
    cache.static_shards.shard_count = STATIC_MESH_SHARD_COUNT;
    cache.static_shards.carried_shard_count = STATIC_MESH_SHARD_COUNT - written_shard_ids.len();
    cache.static_shards.written_shard_count = written_shard_ids.len();
    cache.static_shards.written_shard_ids = written_shard_ids;

    Ok(StaticsStagePlan {
        static_mesh_shards,
        optimized_mesh_bounds,
        mode: StaticsPublishMode::Rebuild {
            usage_bytes,
            previous_usage: base_usage.cloned(),
            shards,
            final_counts,
        },
    })
}

pub(super) fn publish_statics_bundle(
    plan: StaticsStagePlan,
    ctx: &StageContext<'_>,
    writes: &PublicationWrites,
    cache: &mut CacheMetadata,
    reporter: &mut dyn ProgressReporter,
) -> Result<StaticsPublishResult> {
    let output = ctx.output_paths;
    match plan.mode {
        StaticsPublishMode::Reuse { usage, shards } => {
            cache.writes.usage_data = Some(OutputWriteDecision::SkippedUnchanged);
            cache.writes.static_meshes = Some(OutputWriteDecision::SkippedUnchanged);
            Ok(StaticsPublishResult {
                usage,
                shards,
                writer: None,
            })
        }
        StaticsPublishMode::Rebuild {
            usage_bytes,
            previous_usage,
            shards,
            final_counts,
        } => {
            let usage_hash = *blake3::hash(&usage_bytes).as_bytes();
            let usage = if !ctx.force_rebuild()
                && previous_usage
                    .as_ref()
                    .is_some_and(|entry| entry.byte_length == usage_bytes.len() as u64 && entry.content_blake3 == usage_hash)
            {
                cache.writes.usage_data = Some(OutputWriteDecision::SkippedUnchanged);
                previous_usage.expect("checked previous usage")
            } else {
                let written = run_stage(GenerationStage::WriteUsageData, reporter, || {
                    let _stage_guard = info_span!("stage.write_usage_data", report = true).entered();
                    Ok(writes.write_durable(&output.usage_data_path, &usage_bytes, SyncClass::SmallArtifact)?)
                })?;
                cache.writes.usage_data = Some(OutputWriteDecision::Written);
                artifact_from_written(
                    ArtifactKind::Usage,
                    "statics\\usage.data",
                    written.byte_length,
                    written.content_blake3,
                )
            };

            let mut carried = Vec::with_capacity(STATIC_MESH_SHARD_COUNT);
            let mut fresh = Vec::new();
            for (shard_id, shard) in shards.into_iter().enumerate() {
                match shard {
                    StaticShardPlan::Carry(entry) => carried.push(entry),
                    StaticShardPlan::Fresh(packed) => {
                        fresh.push((shard_id, output.static_mesh_shard_paths[shard_id].clone(), packed));
                    }
                }
            }

            if fresh.is_empty() {
                cache.writes.static_meshes = Some(OutputWriteDecision::SkippedUnchanged);
                return Ok(StaticsPublishResult {
                    usage,
                    shards: carried,
                    writer: None,
                });
            }

            let writes = writes.clone();
            let handle = run_stage(GenerationStage::WriteStaticMeshes, reporter, || {
                Ok(std::thread::spawn(move || {
                    let outer_span = info_span!(
                        "io.write_static_meshes_async",
                        report = true,
                        written_shard_count = fresh.len() as u64,
                        subset_count = final_counts.0 as u64,
                        vertex_count = final_counts.1 as u64,
                        triangle_count = final_counts.2 as u64,
                    );
                    let _outer_guard = outer_span.enter();

                    // A rendezvous channel keeps only the shard being written and the next shard
                    // being serialized resident while still overlapping the two operations.
                    let (serialized_tx, serialized_rx) = std::sync::mpsc::sync_channel(0);
                    let serializer_span = info_span!(
                        parent: &outer_span,
                        "io.serialize_static_meshes_pipeline",
                        report = true,
                    );
                    let serializer = std::thread::spawn(move || -> Result<()> {
                        let _guard = serializer_span.enter();
                        for (shard_id, path, packed) in fresh {
                            let serialize_started = Instant::now();
                            let bytes = crate::statics::serialize_static_meshes(&packed)?;
                            let serialization_us =
                                u64::try_from(serialize_started.elapsed().as_micros()).unwrap_or(u64::MAX);
                            serialized_tx.send((shard_id, path, bytes, serialization_us)).map_err(|_| {
                                anyhow::anyhow!("static shard writer stopped before serialization completed")
                            })?;
                        }
                        Ok(())
                    });

                    let write_result: Result<Vec<RequiredArtifact>> = (|| {
                        let mut artifacts = Vec::new();
                        for (shard_id, path, bytes, serialization_us) in serialized_rx {
                            let span = info_span!(
                                "io.write_static_mesh_shard",
                                report = true,
                                shard_id = shard_id as u64,
                                path = field::display(path.display()),
                                serialization_us,
                            );
                            let _guard = span.enter();
                            let written = writes.write_durable(&path, bytes, SyncClass::Payload)?;
                            artifacts.push(artifact_from_written(
                                ArtifactKind::StaticShard,
                                static_mesh_shard_relative_path(shard_id),
                                written.byte_length,
                                written.content_blake3,
                            ));
                        }
                        Ok(artifacts)
                    })();

                    let serializer_result = serializer
                        .join()
                        .map_err(|_| anyhow::anyhow!("static shard serializer thread panicked"))
                        .and_then(|result| result);
                    match (write_result, serializer_result) {
                        (Err(error), _) => Err(error),
                        (Ok(_), Err(error)) => Err(error),
                        (Ok(artifacts), Ok(())) => Ok(artifacts),
                    }
                }))
            })?;
            Ok(StaticsPublishResult {
                usage,
                shards: carried,
                writer: Some(handle),
            })
        }
    }
}

/// Opens the merge-geometry stage span, including the fields describing what it packed.
fn merge_geometry_span() -> Span {
    info_span!(
        "stage.build_merge_geometry",
        report = true,
        packed_merged_static_count = field::Empty,
        packed_merged_subset_count = field::Empty,
        packed_merged_vertex_count = field::Empty,
        packed_merged_triangle_count = field::Empty,
    )
}

/// Records what [`build_merge_geometry`] packed onto its own stage span.
///
/// Merged records are packed inside the merge pass and never reach `stage.convert_statics`, so
/// that stage's counts describe ordinary statics alone. Reporting the merged half here keeps the
/// two spans summing to the whole rather than leaving merged geometry unaccounted for.
fn record_merged_pack_metrics(span: &Span, merged_statics: &crate::PackedDistantStatics) {
    let (subset_count, vertex_count, triangle_count) = summarize_distant_statics(merged_statics);
    record_usize(span, "packed_merged_static_count", merged_statics.len());
    record_usize(span, "packed_merged_subset_count", subset_count);
    record_usize(span, "packed_merged_vertex_count", vertex_count);
    record_usize(span, "packed_merged_triangle_count", triangle_count);
}

/// Orders packed statics shard-major, then by key bytes, for stable runtime ordinals.
///
/// The order is total, so a map assembled from several sources - the conversion stage plus the
/// records `build_merge_geometry` packed as it went - sorts into the same sequence a single pass
/// would have produced.
pub(super) fn sort_packed_statics(distant_statics: &mut crate::PackedDistantStatics) {
    distant_statics.sort_unstable_by(|left_key, _, right_key, _| {
        static_mesh_shard_id(left_key)
            .cmp(&static_mesh_shard_id(right_key))
            .then_with(|| left_key.as_bytes().cmp(right_key.as_bytes()))
    });
}

/// Converts intermediate statics into final shard-major/key order for stable runtime ordinals.
pub(super) fn finalize_distant_statics(
    distant_statics: DistantStatics,
    vfs: &Vfs,
    door_size_multiplier: f32,
) -> crate::PackedDistantStatics {
    let span = info_span!(
        "statics.pack_records",
        report = true,
        source_static_count = distant_statics.len() as u64,
        packed_static_count = field::Empty
    );
    let _guard = span.enter();
    let mut distant_statics: crate::PackedDistantStatics = distant_statics
        .into_par_iter()
        .map(|(key, value)| (key, value.into_distant_static(vfs, door_size_multiplier)))
        .collect();
    sort_packed_statics(&mut distant_statics);
    record_usize(&span, "packed_static_count", distant_statics.len());
    distant_statics
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;
    use crate::mge_xe::distant_statics::StaticType;
    use crate::{DistantReference, StableRefKey};
    use glam::Vec3;

    fn bounded_static(radius: f32, static_type: StaticType) -> crate::DistantStatic {
        crate::DistantStatic {
            bounding_sphere: tes3::nif::NiBound {
                center: Vec3::splat(radius),
                radius,
            },
            bounding_box: crate::mge_xe::distant_statics::BoundingBox {
                min: Vec3::splat(-radius),
                max: Vec3::splat(radius),
            },
            static_type,
            ..crate::DistantStatic::default()
        }
    }

    const DEFAULT_DOOR_SIZE_MULTIPLIER: f32 = 2.0;

    fn empty_vfs() -> crate::Vfs {
        crate::Vfs {
            ini_path: std::path::PathBuf::from("Morrowind.ini"),
            data_dirs: vec![],
            active_plugins: vec![],
            archives: vec![],
            maps: crate::vfs::directory_map::DirectoryMaps::default(),
        }
    }

    fn reference(id: &'static str) -> DistantReference<'static> {
        DistantReference {
            id: Cow::Borrowed(id),
            deleted: false,
            persistent: false,
            translation: Vec3::ZERO,
            rotation: Vec3::ZERO,
            scale: 1.0,
            vis_index: 0,
        }
    }

    #[test]
    fn dirty_cell_partner_collection_matches_pre_optimize_eligibility() {
        let mut usage = UsageInfo::default();
        for (index, id, translation, vis_index) in [
            (1, "partner.nif", Vec3::ZERO, 0),
            (2, "grass.nif", Vec3::ZERO, 0),
            (3, "tiny.nif", Vec3::ZERO, 0),
            (4, "dynamic.nif", Vec3::ZERO, 1),
            (5, "other_cell.nif", Vec3::new(8192.0, 0.0, 0.0), 0),
        ] {
            usage.exterior_references_mut().insert(
                StableRefKey::test(index),
                DistantReference {
                    id: Cow::Borrowed(id),
                    deleted: false,
                    persistent: false,
                    translation,
                    rotation: Vec3::ZERO,
                    scale: 1.0,
                    vis_index,
                },
            );
        }
        let distant_statics = DistantStatics::from_iter([
            ("partner.nif".to_owned(), bounded_static(32.0, StaticType::StaticAuto)),
            ("grass.nif".to_owned(), bounded_static(40.0, StaticType::StaticGrass)),
            ("tiny.nif".to_owned(), bounded_static(31.0, StaticType::StaticAuto)),
            ("dynamic.nif".to_owned(), bounded_static(40.0, StaticType::StaticAuto)),
            ("other_cell.nif".to_owned(), bounded_static(40.0, StaticType::StaticAuto)),
        ]);

        assert_eq!(
            collect_dirty_cell_partners(
                &usage,
                &distant_statics,
                &HashSet::from([super::super::units::MergeUnitKey::new(0, 0)]),
            ),
            HashSet::from(["partner.nif".to_owned()])
        );
    }

    #[test]
    fn retains_only_statics_selected_by_surviving_placements() {
        let mut usage = UsageInfo::default();
        usage
            .exterior_references_mut()
            .insert(StableRefKey::test(1), reference("exterior.nif"));
        usage.cells.insert(
            "Interior".to_owned(),
            [(StableRefKey::test(2), reference("interior.nif"))].into_iter().collect(),
        );
        let mut statics = DistantStatics::from_iter([
            ("exterior.nif".to_owned(), crate::DistantStatic::default()),
            ("interior.nif".to_owned(), crate::DistantStatic::default()),
            ("unplaced.nif".to_owned(), crate::DistantStatic::default()),
        ]);

        let removed = retain_referenced_statics(&mut statics, &usage);

        assert_eq!(
            statics.keys().map(String::as_str).collect_vec(),
            ["exterior.nif", "interior.nif"]
        );
        assert_eq!(removed.keys().map(String::as_str).collect_vec(), ["unplaced.nif"]);
    }

    /// A degraded owner-partial attempt leaves the trimmed member meshes restorable, so the full
    /// rebuild's merge pass can still resolve every group member it planned.
    #[test]
    fn restored_merge_members_survive_a_fallback_to_the_full_merge() {
        let mut usage = UsageInfo::default();
        for (index, id, translation) in [(1, "left.nif", Vec3::ZERO), (2, "right.nif", Vec3::new(256.0, 0.0, 0.0))] {
            usage.exterior_references_mut().insert(
                StableRefKey::test(index),
                DistantReference {
                    translation,
                    ..reference(id)
                },
            );
        }
        let mut statics = DistantStatics::from_iter([
            ("left.nif".to_owned(), bounded_static(40.0, StaticType::StaticAuto)),
            ("right.nif".to_owned(), bounded_static(40.0, StaticType::StaticAuto)),
        ]);

        let plan = plan_exterior_merge_groups(&statics, &usage, 8192.0);
        assert_eq!(plan.group_count(), 1);
        apply_merge_usage(&plan, &mut usage);

        // The merge swallowed both members' placements, so the partial path's trim empties the map.
        let removed = retain_referenced_statics(&mut statics, &usage);
        assert!(statics.is_empty());

        statics.extend(removed);
        // Without the restore this lookup of each planned member's source mesh panics. These
        // members carry no subsets, so the merged record is empty and never reaches the packed
        // map; the member tally is what proves both lookups resolved.
        let (metrics, _) = build_merge_geometry(
            &plan,
            &CellFilter::All,
            &statics,
            crate::StaticMeshSimplifierConfig::default(),
            None,
            &empty_vfs(),
            DEFAULT_DOOR_SIZE_MULTIPLIER,
        );
        assert_eq!(metrics.member_count, 2);
    }

    #[test]
    fn final_ordinal_reconciliation_removes_dangling_references() {
        let mut usage = UsageInfo::default();
        for (index, id) in [(1, "placed.nif"), (2, "missing.nif")] {
            usage
                .exterior_references_mut()
                .insert(StableRefKey::test(index), reference(id));
        }
        let ordinals = crate::usage::StaticOrdinalView::from_ordered_keys(["placed.nif".to_owned()]);
        let mut metrics = GenerationMetrics::default();

        reconcile_usage_with_ordinals(&mut usage, &ordinals, &mut metrics);

        assert_eq!(usage.exterior_references_count(), 1);
        assert_eq!(metrics.usage.total_reference_count_after_merge, 1);
        assert_eq!(metrics.usage.exterior_reference_count_after_merge, 1);
    }

    #[test]
    fn cached_bounds_restore_clean_meshes_and_capture_sorted_ordinary_entries() {
        let mut distant_statics = DistantStatics::from_iter([
            ("z_dirty.nif".to_owned(), bounded_static(2.0, StaticType::StaticAuto)),
            ("a_clean.nif".to_owned(), bounded_static(1.0, StaticType::StaticAuto)),
            (
                "CELL (0, 0) GROUP (0)".to_owned(),
                bounded_static(3.0, StaticType::StaticAuto),
            ),
        ]);
        let previous = vec![(
            "a_clean.nif".to_owned(),
            OptimizedMeshBounds {
                center: [10.0; 3],
                radius: 10.0,
                box_min: [-10.0; 3],
                box_max: [10.0; 3],
            },
        )];
        assert_eq!(
            remaining_unoptimized_meshes(&distant_statics, &HashSet::from(["z_dirty.nif".to_owned()])),
            HashSet::from(["a_clean.nif".to_owned()])
        );
        restore_optimized_bounds(
            &mut distant_statics,
            &HashSet::from(["z_dirty.nif".to_owned(), "CELL (0, 0) GROUP (0)".to_owned()]),
            &previous,
        );

        assert_eq!(distant_statics["a_clean.nif"].bounding_sphere.radius, 10.0);
        assert_eq!(distant_statics["z_dirty.nif"].bounding_sphere.radius, 2.0);
        assert_eq!(
            capture_optimized_bounds(&distant_statics)
                .into_iter()
                .map(|(key, _)| key)
                .collect_vec(),
            ["a_clean.nif", "z_dirty.nif"]
        );
    }
}
