//! Batches compatible exterior references into synthetic static meshes.

use std::time::Duration;

use glam::{Affine3A, Vec3};
use hashbrown::{HashMap, HashSet};
use obvhs::BvhBuildParams;
use obvhs::aabb::Aabb;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tracing::info_span;

use crate::DistantStatics;
use crate::mge_xe::distant_statics::StaticType;
use crate::model::{
    DistantStatic, StaticMeshContext, StaticMeshSimplifierConfig, Subset, SubterrainCull, SubterrainCuller,
    update_bounds_with_context,
};
use crate::usage::{DistantReference, SourceId, StableRefKey, UsageInfo};

/// Log2 spacing between adjacent LOD-ladder error levels (0.5 = a ratio of sqrt(2)).
const LOD_LADDER_LOG2_STEP: f32 = 0.5;
/// Sentinel for an exact zero-error cache request.
const ZERO_ERROR_BUCKET: i32 = i32::MIN;

/// One planned merge group: the member references in a single exterior cell that will be
/// batched into one synthetic static.
///
/// Purely geometric: the group's simplification budget (`group_error`) depends on the mesh
/// simplifier configuration and is derived on demand in [`build_merge_geometry`], not stored here.
#[derive(Clone)]
struct MergeGroup<'a> {
    cell_x: i32,
    cell_y: i32,
    group_idx: usize,
    members: Vec<(StableRefKey, DistantReference<'a>)>,
    group_extent: f32,
}

impl MergeGroup<'_> {
    /// Renders the synthetic static id for this group.
    fn synthetic_id(&self) -> String {
        distantland_foundation::record_key::StaticRecordKey::Merged {
            cell_x: self.cell_x,
            cell_y: self.cell_y,
            group_idx: u32::try_from(self.group_idx).expect("merge group index exceeds u32"),
        }
        .render()
    }

    fn cell(&self) -> (i32, i32) {
        (self.cell_x, self.cell_y)
    }
}

/// Four-point summary of a merge-simplification value distribution.
#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct MergeValueDistribution {
    /// Minimum observed value.
    pub min: f32,
    /// Median observed value.
    pub p50: f32,
    /// 95th-percentile observed value.
    pub p95: f32,
    /// Maximum observed value.
    pub max: f32,
}

impl MergeValueDistribution {
    fn from_values(mut values: Vec<f32>) -> Self {
        values.retain(|value| value.is_finite());
        if values.is_empty() {
            return Self::default();
        }
        values.sort_unstable_by(f32::total_cmp);
        let percentile = |percent: usize| values[(values.len() - 1) * percent / 100];
        Self {
            min: values[0],
            p50: percentile(50),
            p95: percentile(95),
            max: values[values.len() - 1],
        }
    }
}

/// Diagnostics for the exterior-reference merge simplification policy.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct MergeSimplificationMetrics {
    /// Number of multi-reference merge groups processed.
    pub group_count: usize,
    /// Number of references included across all merge groups.
    pub member_count: usize,
    /// Number of source subsets included across all merge-group members.
    pub member_subset_count: usize,
    /// Number of member LOD requests that required an additional simplification pass.
    pub lod_cache_request_count: usize,
    /// Number of unique `(static, error bucket)` entries materialized in the LOD cache.
    pub lod_cache_entry_count: usize,
    /// Number of LOD requests served by reusing an existing cache entry.
    pub lod_cache_reuse_count: usize,
    /// Number of member subsets simplified again after the initial per-static pass.
    pub second_pass_subset_count: usize,
    /// Number of member subsets whose requested relative target was reduced by the cap.
    pub capped_subset_count: usize,
    /// Number of member subsets whose uncapped bucketed relative target exceeded `1.0`.
    pub requested_relative_target_over_one_subset_count: usize,
    /// Source triangle count belonging to member subsets whose uncapped relative target exceeded `1.0`.
    pub requested_relative_target_over_one_triangle_count: usize,
    /// Instanced source triangle count before any merge-stage second pass.
    pub member_triangle_count_before_second_pass: usize,
    /// Instanced triangle count after selecting cached or unchanged member subsets.
    pub member_triangle_count_after_second_pass: usize,
    /// Number of non-empty synthetic merged statics emitted by the active merge policy.
    pub emitted_merged_static_count: usize,
    /// Number of subsets across synthetic merged statics emitted by the active merge policy.
    pub emitted_merged_subset_count: usize,
    /// Triangle count in the final synthetic merged statics.
    pub merged_triangle_count: usize,
    /// Triangles removed by the below-terrain cull, additional to `merged_triangle_count`.
    pub subterrain_culled_triangle_count: usize,
    /// Vertices removed alongside the below-terrain culled triangles.
    pub subterrain_culled_vertex_count: usize,
    /// Distribution of maximum XYZ group AABB extents in world units.
    pub group_extent: MergeValueDistribution,
    /// Distribution of group extent divided by each decimation-eligible member subset's world-space extent.
    pub group_to_member_extent_ratio: MergeValueDistribution,
    /// Distribution of uncapped bucketed relative targets requested for decimation-eligible member subsets.
    pub requested_relative_target: MergeValueDistribution,
    /// Distribution of relative targets after applying the configured cap for decimation-eligible member subsets.
    pub effective_relative_target: MergeValueDistribution,
}

/// Quantizes an absolute error budget (mesh-local units) down onto the LOD ladder, so cached
/// LODs never exceed the budget of any group that selects them.
fn error_bucket(local_error: f32) -> i32 {
    if local_error == 0.0 {
        return ZERO_ERROR_BUCKET;
    }
    (local_error.log2() / LOD_LADDER_LOG2_STEP).floor() as i32
}

/// Returns the absolute error (mesh-local units) for an LOD-ladder bucket.
fn bucket_error(bucket: i32) -> f32 {
    if bucket == ZERO_ERROR_BUCKET {
        0.0
    } else {
        (bucket as f32 * LOD_LADDER_LOG2_STEP).exp2()
    }
}

/// Returns the LOD-cache key for one group member: the static's stable map key and the error
/// ladder bucket after converting the group's world-space budget into mesh-local space.
///
/// The stable mesh key keeps cache identity valid when owner-partial builds retain only a subset
/// of statics; the borrowed id also avoids allocating during repeated scans.
fn member_lod_key<'a>(reference: &'a DistantReference<'_>, group_error: f32) -> (&'a str, i32) {
    (reference.id.as_ref(), error_bucket(group_error / reference.scale.max(1e-6)))
}

fn subset_extent(subset: &Subset) -> f32 {
    (subset.bounding_box.max - subset.bounding_box.min).max_element()
}

/// Recursively partitions a BVH into spatially bounded merge groups.
///
/// When a node's horizontal half-diagonal is within `max_radius`, or when the node is a leaf,
/// all primitive indices under it are collected into one group. Otherwise the node is split and
/// both children are examined recursively.
fn collect_merge_groups(bvh: &obvhs::bvh2::Bvh2, node_index: usize, max_radius: f32, groups: &mut Vec<Vec<usize>>) {
    let node = &bvh.nodes[node_index];
    let aabb = node.aabb();

    // Merge groups are spatially limited in the horizontal plane so tall objects can still
    // merge when they occupy the same exterior footprint.
    let diagonal = (aabb.max.truncate() - aabb.min.truncate()).length();
    let radius = diagonal * 0.5;

    if radius <= max_radius || node.is_leaf() {
        let mut group = vec![];
        collect_indices(bvh, node_index, &mut group);
        if !group.is_empty() {
            groups.push(group);
        }
    } else {
        collect_merge_groups(bvh, node.first_index as usize, max_radius, groups);
        collect_merge_groups(bvh, node.first_index as usize + 1, max_radius, groups);
    }
}

fn collect_indices(bvh: &obvhs::bvh2::Bvh2, node_index: usize, indices: &mut Vec<usize>) {
    let node = &bvh.nodes[node_index];
    if node.is_leaf() {
        for i in 0..node.prim_count {
            indices.push(bvh.primitive_indices[(node.first_index + i) as usize] as usize);
        }
    } else {
        collect_indices(bvh, node.first_index as usize, indices);
        collect_indices(bvh, node.first_index as usize + 1, indices);
    }
}

/// Appends subsets from `source_subsets` into `merged_subsets`, applying `transform` and
/// recentering vertices around `center`. Only subsets whose alpha flag matches `opaque` are
/// processed so callers can separate opaque and alpha passes.
///
/// When `culler` is present, geometry below the terrain surface is dropped during the append, so
/// each component range still tiles the triangle buffer it is measured against.
#[allow(clippy::too_many_arguments)]
fn merge_subsets(
    merged_subsets: &mut Vec<Subset>,
    source_subsets: &[Subset],
    transform: Affine3A,
    center: Vec3,
    opaque: bool,
    component_center: Vec3,
    component_radius: f32,
    classification: StaticType,
    mut culler: Option<&mut SubterrainCuller<'_>>,
) {
    for subset in source_subsets {
        if subset.is_opaque() != opaque {
            continue;
        }

        let merged_subset = match merged_subsets.last_mut() {
            Some(last) if subset.can_merge_with(last) => last,
            _ => merged_subsets.push_mut(Subset::default()),
        };

        let first_triangle = merged_subset.triangles.len() as u32;
        match culler.as_deref_mut() {
            Some(culler) => merged_subset.merge_transformed_culled(subset, transform, center, opaque, culler),
            None => merged_subset.merge_transformed(subset, transform, center, opaque),
        }
        let triangle_count = merged_subset.triangles.len() as u32 - first_triangle;
        merged_subset.push_component(
            first_triangle,
            triangle_count,
            component_center,
            component_radius,
            classification,
        );
    }
}

/// Selects which planned merge cells [`build_merge_geometry`] materializes geometry for.
pub enum CellFilter {
    /// Build every planned group; reproduces the full (non-incremental) merge.
    All,
    /// Build only groups whose exterior cell is in this set.
    Dirty(HashSet<(i32, i32)>),
}

impl CellFilter {
    fn includes(&self, cell: (i32, i32)) -> bool {
        match self {
            CellFilter::All => true,
            CellFilter::Dirty(cells) => cells.contains(&cell),
        }
    }
}

/// Complete merge grouping in deterministic `(cell_x, cell_y, group_idx)` order.
///
/// Usage mutation and geometry construction can run separately for owner-partial builds.
pub struct MergePlan<'a> {
    groups: Vec<MergeGroup<'a>>,
}

impl MergePlan<'_> {
    /// Returns every planned synthetic record key in deterministic group order.
    pub fn merged_record_keys(&self) -> impl Iterator<Item = distantland_foundation::record_key::StaticRecordKey> + '_ {
        self.groups
            .iter()
            .map(|group| distantland_foundation::record_key::StaticRecordKey::Merged {
                cell_x: group.cell_x,
                cell_y: group.cell_y,
                group_idx: u32::try_from(group.group_idx).expect("merge group index exceeds u32"),
            })
    }

    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    /// Returns every planned member's source-mesh id, in deterministic group order.
    ///
    /// [`build_merge_geometry`] looks up all of these, so a caller that trims the statics map
    /// between planning and building can assert the meshes it still needs are present.
    pub fn member_ids(&self) -> impl Iterator<Item = &str> + '_ {
        self.groups
            .iter()
            .flat_map(|group| group.members.iter().map(|(_, reference)| reference.id.as_ref()))
    }
}

/// Groups nearby exterior references into synthetic cell-local merge candidates.
///
/// This is the geometry-independent first half of the merge: it filters mergeable exterior
/// references, spatially groups them per cell, and records each group's members and world extent.
/// No mesh simplification or map mutation happens here.
///
/// `max_group_radius` is the maximum half-diagonal (in game units, horizontal plane) of a BVH
/// node whose references may be batched into a single merge group; wider nodes are split further.
///
/// # Panics
///
/// Panics if any candidate reference's static ID cannot be found in `distant_statics`.
pub fn plan_exterior_merge_groups<'a>(
    distant_statics: &DistantStatics,
    usage_info: &UsageInfo<'a>,
    max_group_radius: f32,
) -> MergePlan<'a> {
    let ref_count = usage_info.exterior_references_count();
    let _grouping_guard = info_span!("statics.merge_grouping", report = true).entered();

    // Exterior entries must remain borrowed for the whole planning pass: per-cell bags hold
    // `&DistantReference` for AABB/BVH work and only clone when materializing multi-member groups.
    let Some(exterior) = usage_info.exterior_references() else {
        return MergePlan { groups: Vec::new() };
    };

    let mut cells: HashMap<(i32, i32), Vec<(StableRefKey, &DistantReference<'a>)>> =
        HashMap::with_capacity((ref_count / 8).max(16));

    for (key, reference) in exterior {
        if reference.vis_index != 0 {
            continue;
        }
        let (cell_x, cell_y) = reference.cell_coords();

        if let Some(ds) = distant_statics.get(reference.id.as_ref())
            && ds.static_type != StaticType::StaticGrass
            && (ds.bounding_sphere.radius * reference.scale) >= 32.0
        {
            cells.entry((cell_x, cell_y)).or_insert_with(Vec::new).push((*key, reference));
        }
    }

    cells.retain(|_, references| references.len() > 1);

    // Keep planning independent of simplifier settings; derive the absolute budget during build.
    let mut all_groups: Vec<_> = {
        cells
            .into_par_iter()
            .flat_map_iter(|((cell_x, cell_y), references)| {
                let mut aabbs = Vec::with_capacity(references.len());
                for (_, reference) in &references {
                    let ds = distant_statics.get(reference.id.as_ref()).unwrap();
                    let (min, max) = reference.world_aabb(&ds.bounding_box);
                    aabbs.push(Aabb::new(min.into(), max.into()));
                }

                let bvh =
                    obvhs::bvh2::builder::build_bvh2(&aabbs, BvhBuildParams::fastest_build(), &mut Duration::default());

                let mut groups = vec![];
                if !bvh.nodes.is_empty() {
                    collect_merge_groups(&bvh, 0, max_group_radius, &mut groups);
                }

                groups
                    .into_iter()
                    .filter(|g| g.len() > 1)
                    .enumerate()
                    .map(move |(group_idx, mut group)| {
                        group.sort_unstable_by_key(|&index| references[index].0);

                        let mut min = glam::Vec3A::MAX;
                        let mut max = glam::Vec3A::MIN;
                        for &i in &group {
                            min = min.min(aabbs[i].min);
                            max = max.max(aabbs[i].max);
                        }
                        let group_extent = (max - min).max_element();

                        let members: Vec<_> = group
                            .iter()
                            .map(|&i| {
                                let (key, reference) = references[i];
                                (key, DistantReference::clone(reference))
                            })
                            .collect();
                        MergeGroup {
                            cell_x,
                            cell_y,
                            group_idx,
                            members,
                            group_extent,
                        }
                    })
            })
            .collect()
    };
    all_groups.sort_unstable_by_key(|group| (group.cell_x, group.cell_y, group.group_idx));

    MergePlan { groups: all_groups }
}

/// Applies a merge plan's effect on the usage table: removes each group's member references and
/// inserts one synthetic reference per group, in the plan's deterministic order.
///
/// This runs for *every* planned group, including groups whose geometry later proves empty
/// and whose synthetic reference then dangles and is skipped during usage serialization, exactly
/// reproducing the monolithic merge's usage mutation and its synthetic reference indices.
pub fn apply_merge_usage(plan: &MergePlan<'_>, usage_info: &mut UsageInfo<'_>) {
    let mut removals = HashSet::new();
    let mut insertions = Vec::with_capacity(plan.groups.len());
    for group in &plan.groups {
        // The synthetic reference uses the same mean as geometry recentering.
        let center =
            group.members.iter().map(|(_, reference)| reference.translation).sum::<Vec3>() / group.members.len() as f32;
        for (key, _) in &group.members {
            removals.insert(*key);
        }
        insertions.push(DistantReference {
            id: group.synthetic_id().into(),
            deleted: false,
            persistent: false,
            translation: center,
            rotation: Vec3::ZERO,
            scale: 1.0,
            vis_index: 0,
        });
    }

    usage_info.exterior_references_mut().retain(|key, _| !removals.contains(key));

    let synthetic_source = SourceId::synthetic();
    let mut next_index = usage_info.next_reference_index(synthetic_source);
    for insert in insertions {
        usage_info
            .exterior_references_mut()
            .insert(StableRefKey::synthetic(next_index), insert);
        next_index += 1;
    }
}

/// Number of merge groups whose geometry is built before it is handed to `emit` and released.
///
/// Two separate costs are at work. Packing each group as it is built, instead of retaining the
/// unpacked form for a later conversion pass, is what removes the bulk: an unpacked `Vertex` is
/// 64 bytes against `PackedVertex`'s 28 and nothing pre-sizes the merged buffers, so the whole
/// run's retained intermediate cost roughly 12 GiB on the reference job against 3.7 GiB of packed
/// output. Batching then bounds the transient on top of that - at most this many groups' unpacked
/// geometry is alive at once. Whole-process peak moved under 3% between 64 and 256 on the
/// reference job, which says some other allocation dominated that run, not that the bound is
/// inert. The value is deliberately a fixed constant rather than a function of the thread count:
/// a thread-count-dependent batch would make the reported metrics vary by machine for no memory
/// benefit.
const MERGE_GEOMETRY_BATCH_GROUPS: usize = 256;

/// Builds merged geometry for the selected cells of a plan, packing each synthetic static as soon
/// as its geometry is complete, and returns the simplification metrics with the packed records.
///
/// [`CellFilter::All`] reproduces the monolithic geometry pass exactly. A narrower filter builds
/// only the requested cells' groups; the returned metrics then describe just those groups.
///
/// Merged records are packed here rather than in the caller's conversion stage because the unpacked
/// form is 2.3x the size of the packed one and nothing downstream ever reads it: packing is a pure
/// per-record function, and the final map is re-sorted into shard-major key order afterwards.
/// Groups whose geometry came out empty are dropped rather than packed.
///
/// # Panics
///
/// Panics if a member's static ID is missing from `distant_statics`.
/// `subterrain_cull`, when present, removes merged triangles that sit entirely below the terrain
/// surface. It applies to merged geometry only; ordinary instanced records are untouched.
pub fn build_merge_geometry(
    plan: &MergePlan<'_>,
    cells: &CellFilter,
    distant_statics: &DistantStatics,
    config: StaticMeshSimplifierConfig,
    subterrain_cull: Option<SubterrainCull<'_>>,
    vfs: &crate::Vfs,
    door_size_multiplier: f32,
) -> (MergeSimplificationMetrics, crate::PackedDistantStatics) {
    let mut packed = crate::PackedDistantStatics::default();
    let metrics = build_merge_batches(plan, cells, distant_statics, config, subterrain_cull, |batch| {
        // Packing in parallel keeps the stage at wall-time parity; a sequential pack cost +7.6 s.
        let batch: Vec<_> = batch
            .into_par_iter()
            .map(|(id, merged_static)| (id, merged_static.into_distant_static(vfs, door_size_multiplier)))
            .collect();
        packed.extend(batch);
    });
    (metrics, packed)
}

/// [`build_merge_geometry`] retaining the unpacked merged records in `distant_statics`.
///
/// Packing discards what the merge tests assert on - f32 vertex positions, exact bounding spheres,
/// `horizon_footprint_eligible`, component tiling - so they build through this instead.
#[cfg(test)]
pub(crate) fn build_merge_geometry_unpacked(
    plan: &MergePlan<'_>,
    cells: &CellFilter,
    distant_statics: &mut DistantStatics,
    config: StaticMeshSimplifierConfig,
    subterrain_cull: Option<SubterrainCull<'_>>,
) -> MergeSimplificationMetrics {
    let mut built = Vec::new();
    let metrics = build_merge_batches(plan, cells, distant_statics, config, subterrain_cull, |batch| {
        built.extend(batch);
    });
    distant_statics.extend(built);
    metrics
}

/// Builds the selected groups' merged geometry in batches, handing each batch's non-empty records
/// to `emit` before the next batch is built.
fn build_merge_batches(
    plan: &MergePlan<'_>,
    cells: &CellFilter,
    distant_statics: &DistantStatics,
    config: StaticMeshSimplifierConfig,
    subterrain_cull: Option<SubterrainCull<'_>>,
    mut emit: impl FnMut(Vec<(String, DistantStatic)>),
) -> MergeSimplificationMetrics {
    let work_groups: Vec<&MergeGroup> = plan.groups.iter().filter(|group| cells.includes(group.cell())).collect();

    let mut metrics = MergeSimplificationMetrics {
        group_count: work_groups.len(),
        ..MergeSimplificationMetrics::default()
    };
    let mut group_extents = Vec::with_capacity(work_groups.len());
    let mut extent_ratios = Vec::new();
    let mut requested_targets = Vec::new();
    let mut effective_targets = Vec::new();
    let mut needed = HashSet::new();

    // Collect distinct `(static, error-bucket)` requests while accumulating metrics.
    for group in &work_groups {
        let group_error = config.target_error * group.group_extent;
        group_extents.push(group.group_extent);
        for (_, reference) in &group.members {
            metrics.member_count += 1;
            let key = member_lod_key(reference, group_error);
            let absolute_error = bucket_error(key.1);
            let ds = distant_statics.get(key.0).unwrap();
            let mut member_needs_lod = false;

            for subset in &ds.subsets {
                metrics.member_subset_count += 1;
                metrics.member_triangle_count_before_second_pass += subset.triangles.len();

                if !subset.allows_simplification() {
                    continue;
                }

                let extent = subset_extent(subset);
                if extent <= 0.0 || !extent.is_finite() {
                    continue;
                }

                let target = subset.absolute_simplification_target_with_extent(config, absolute_error, extent);
                extent_ratios.push(group.group_extent / (extent * reference.scale.max(1e-6)));
                requested_targets.push(target.requested);
                effective_targets.push(target.effective);

                if target.requested > 1.0 {
                    metrics.requested_relative_target_over_one_subset_count += 1;
                    metrics.requested_relative_target_over_one_triangle_count += subset.triangles.len();
                }
                if target.capped {
                    metrics.capped_subset_count += 1;
                }
                if target.should_simplify {
                    metrics.second_pass_subset_count += 1;
                    member_needs_lod = true;
                }
            }

            if member_needs_lod {
                metrics.lod_cache_request_count += 1;
                needed.insert(key);
            }
        }
    }

    metrics.group_extent = MergeValueDistribution::from_values(group_extents);
    metrics.group_to_member_extent_ratio = MergeValueDistribution::from_values(extent_ratios);
    metrics.requested_relative_target = MergeValueDistribution::from_values(requested_targets);
    metrics.effective_relative_target = MergeValueDistribution::from_values(effective_targets);
    metrics.lod_cache_entry_count = needed.len();
    metrics.lod_cache_reuse_count = metrics.lod_cache_request_count - metrics.lod_cache_entry_count;

    // Build each requested `(static, error-bucket)` pair once and reuse it across groups.
    let lod_guard = info_span!("statics.merge_lod", report = true).entered();
    let lod_cache: HashMap<(&str, i32), Vec<Subset>> = needed
        .into_par_iter()
        .map_init(StaticMeshContext::default, |context, (mesh_key, bucket)| {
            let ds = distant_statics.get(mesh_key).unwrap();
            let absolute_error = bucket_error(bucket);
            let subsets = ds
                .subsets
                .iter()
                .enumerate()
                .map(|(subset_index, subset)| {
                    Subset::build_merge_lod_from(subset, config, absolute_error, context, mesh_key, subset_index)
                })
                .collect();
            ((mesh_key, bucket), subsets)
        })
        .collect();
    drop(lod_guard);

    for group in &work_groups {
        let group_error = config.target_error * group.group_extent;
        for (_, reference) in &group.members {
            let key = member_lod_key(reference, group_error);
            let source = &distant_statics.get(key.0).unwrap().subsets;
            let subsets = lod_cache.get(&key).unwrap_or(source);
            metrics.member_triangle_count_after_second_pass +=
                subsets.iter().map(|subset| subset.triangles.len()).sum::<usize>();
        }
    }

    // LOD simplification is cached; this pass transforms, merges, and recomputes bounds. Groups are
    // built a batch at a time so only one batch of unpacked merged geometry is ever resident. The
    // span stays entered across `emit`, so production packing is reported as merge work - it used
    // to be timed under `stage.convert_statics`, which merged records no longer pass through.
    let geometry_guard = info_span!("statics.merge_geometry", report = true).entered();
    for work_batch in work_groups.chunks(MERGE_GEOMETRY_BATCH_GROUPS) {
        let results: Vec<_> = {
            let lod_cache = &lod_cache;
            work_batch
                .par_iter()
                .map_init(StaticMeshContext::default, |context, group| {
                    let group_error = config.target_error * group.group_extent;
                    let group_references: Vec<_> = group.members.iter().map(|(_, reference)| reference).collect();
                    let mut culler = subterrain_cull.map(SubterrainCuller::new);

                    let center =
                        group_references.iter().map(|r| r.translation).sum::<Vec3>() / group_references.len() as f32;

                    let mut merged_static = DistantStatic::default();
                    merged_static.static_type = StaticType::StaticAuto;
                    merged_static.max_scale = 1.0;
                    merged_static.horizon_footprint_eligible = true;

                    for opaque in [true, false] {
                        for reference in &group_references {
                            let key = member_lod_key(reference, group_error);
                            let source_static = distant_statics.get(key.0).unwrap();
                            let source = &source_static.subsets;
                            let subsets = lod_cache.get(&key).unwrap_or(source);
                            let transform = reference.get_transform();
                            let component_center = transform.transform_point3(source_static.bounding_sphere.center) - center;
                            merge_subsets(
                                &mut merged_static.subsets,
                                subsets,
                                transform,
                                center,
                                opaque,
                                component_center,
                                source_static.bounding_sphere.radius * reference.scale,
                                source_static.static_type,
                                culler.as_mut(),
                            );
                        }
                    }

                    // Inputs are already welded and compacted; only final merge/bounds work remains.
                    // Empty subsets are discarded before packing.
                    merged_static.merge_subsets();
                    update_bounds_with_context(&mut merged_static, context);

                    let cull_tally = culler.map(|culler| culler.tally()).unwrap_or_default();
                    (group.synthetic_id(), merged_static, cull_tally)
                })
                .collect()
        };

        // Tally sequentially so the metrics stay independent of worker scheduling.
        let mut built = Vec::with_capacity(results.len());
        for (id, merged_static, cull_tally) in results {
            metrics.subterrain_culled_triangle_count += cull_tally.triangles;
            metrics.subterrain_culled_vertex_count += cull_tally.vertices;
            metrics.merged_triangle_count += merged_static
                .subsets
                .iter()
                .map(|subset| subset.triangles.len())
                .sum::<usize>();
            metrics.emitted_merged_subset_count += merged_static.subsets.len();

            if merged_static.subsets.is_empty() {
                continue;
            }
            metrics.emitted_merged_static_count += 1;
            built.push((id, merged_static));
        }
        emit(built);
    }
    drop(geometry_guard);

    metrics
}

#[cfg(test)]
mod tests;
