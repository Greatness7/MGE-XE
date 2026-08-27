//! Manifest metrics describing cache reuse decisions and generated output sizes.
//!
//! Every field written here is serialized verbatim into `distantland\generation_report.toml`
//! so field names are part of the advisory report schema.

use serde::{Deserialize, Serialize};

use crate::statics::atlas::AtlasFamilyMetrics;

use crate::generation::storage::state::{ArtifactKind, RequiredArtifact};
use crate::generation::units::TerrainMeshWorkKey;

/// How a particular output file was produced during the current run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputWriteDecision {
    /// The file was freshly written by this run.
    #[default]
    Written,
    /// The file was left untouched because identical output already existed.
    SkippedUnchanged,
}

/// Per-output write decisions recorded in the generation report.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct OutputWriteMetadata {
    /// Write outcome for `distantland\statics\usage.data`.
    #[serde(rename = "usage.data", skip_serializing_if = "Option::is_none")]
    pub usage_data: Option<OutputWriteDecision>,
    /// Aggregate write outcome for the fixed static-mesh shard set.
    #[serde(rename = "static_meshes", skip_serializing_if = "Option::is_none")]
    pub static_meshes: Option<OutputWriteDecision>,
    /// Write outcome for `distantland\terrain.bin`.
    #[serde(rename = "terrain.bin", skip_serializing_if = "Option::is_none")]
    pub terrain_bin: Option<OutputWriteDecision>,
    /// Write outcome for `distantland\terrain_occlusion.bin`.
    #[serde(rename = "terrain_occlusion.bin", skip_serializing_if = "Option::is_none")]
    pub terrain_occlusion: Option<OutputWriteDecision>,
    /// Write outcome for `distantland\terrain_atlas.dds`.
    #[serde(rename = "terrain_atlas.dds", skip_serializing_if = "Option::is_none")]
    pub terrain_atlas: Option<OutputWriteDecision>,
    /// Write outcome for `distantland\terrain_material.dds`.
    #[serde(rename = "terrain_material.dds", skip_serializing_if = "Option::is_none")]
    pub terrain_material: Option<OutputWriteDecision>,
    /// Write outcome for `distantland\terrain_material_flags.dds`.
    #[serde(rename = "terrain_material_flags.dds", skip_serializing_if = "Option::is_none")]
    pub terrain_material_flags: Option<OutputWriteDecision>,
    /// Write outcome for `distantland\terrain_patch_albedo.dds`.
    #[serde(rename = "terrain_patch_albedo.dds", skip_serializing_if = "Option::is_none")]
    pub terrain_patch_albedo: Option<OutputWriteDecision>,
    /// Write outcome for `distantland\terrain_blend_patterns.dds`.
    #[serde(rename = "terrain_blend_patterns.dds", skip_serializing_if = "Option::is_none")]
    pub terrain_blend_patterns: Option<OutputWriteDecision>,
}

/// Cache-hit summary for the current generation run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct CacheMetadata {
    /// Whether the static texture atlas was reused from cache.
    pub static_atlas_hit: bool,
    /// Whether the complete static mesh bundle (all shards plus `usage.data`) was reused from cache.
    pub static_bundle_hit: bool,
    /// Aggregate fixed-shard reuse and publication evidence.
    #[serde(default)]
    pub static_shards: StaticShardMetrics,
    /// Per-file write decisions for runtime outputs.
    pub writes: OutputWriteMetadata,
}

/// High-level static-shard reuse path selected for a generation run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StaticReuseMode {
    /// The coarse statics gate carried usage and all shards before expensive CPU work.
    CarriedBundle,
    /// A coarse miss rebuilt only dirty owners and spliced their affected shards.
    OwnerPartial,
    /// Owner-partial reuse was unavailable, so the complete statics pipeline ran.
    #[default]
    FullRebuild,
    /// The user explicitly requested a complete statics rebuild.
    ForcedFull,
}

/// Aggregate observability for the fixed static shard set.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct StaticShardMetrics {
    /// Reuse path selected by the statics stage.
    pub reuse_mode: StaticReuseMode,
    /// Stable reason owner-partial reuse was unavailable or rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reuse_fallback_reason: Option<String>,
    /// Number of shards in the output contract.
    pub shard_count: usize,
    /// Number of shards carried from committed state.
    pub carried_shard_count: usize,
    /// Number of shards durably written by this run.
    pub written_shard_count: usize,
    /// Shard ids selected for fresh publication.
    pub written_shard_ids: Vec<u8>,
    /// Ordinary owners selected by the complete dirty-owner closure.
    pub dirty_mesh_count: usize,
    /// Merge cells selected by the complete dirty-owner closure.
    pub dirty_cell_count: usize,
    /// Ordinary statics that ran the meshopt pipeline during this statics miss.
    pub optimized_static_count: usize,
    /// Ordinary statics whose prior post-optimize bounds allowed meshopt to be skipped.
    pub skipped_optimize_static_count: usize,
    /// Whether this miss began with a selective rather than full meshopt pass.
    pub selective_optimize: bool,
    /// Geometry-independent merge groups planned globally.
    pub merge_groups_planned: usize,
    /// Merge groups whose geometry was built on the selected path.
    pub merge_groups_geometry_built: usize,
    /// Ordinary records carried without repacking.
    pub ordinary_records_carried: usize,
    /// Ordinary records freshly packed.
    pub ordinary_records_rebuilt: usize,
    /// Ordinary records newly added to the assembly.
    pub ordinary_records_added: usize,
    /// Ordinary records removed from the assembly.
    pub ordinary_records_removed: usize,
    /// Merged records carried without rebuilding geometry.
    pub merged_records_carried: usize,
    /// Merged records freshly built and packed.
    pub merged_records_rebuilt: usize,
    /// Merged records newly added to the assembly.
    pub merged_records_added: usize,
    /// Merged records removed from the assembly.
    pub merged_records_removed: usize,
    /// Dirty shards whose committed bytes passed length, hash, and decode validation.
    pub validated_shard_count: usize,
    /// Dirty shards decoded for positional key recovery and splicing.
    pub decoded_shard_count: usize,
    /// Distinct merge LOD cache entries built by this run.
    pub lod_cache_entries_built: usize,
}

/// Metrics captured while scanning plugins and usage data.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct UsageMetrics {
    /// Number of object records indexed from the load order.
    pub object_count: usize,
    /// Number of distinct mesh-scale entries captured from references.
    pub mesh_scale_count: usize,
    /// Number of landscape records available for static/terrain processing.
    pub landscape_cell_count: usize,
    /// Number of terrain cells that contribute to atlas generation.
    pub terrain_cell_count: usize,
    /// Number of interior cells retained in interior metadata.
    pub interior_cell_count: usize,
    /// Total reference count immediately after parsing plugins.
    pub total_reference_count_after_parse: usize,
    /// Exterior reference count immediately after parsing plugins.
    pub exterior_reference_count_after_parse: usize,
    /// Total reference count after pruning unused or hidden references.
    pub total_reference_count_after_prune: usize,
    /// Exterior reference count after pruning unused or hidden references.
    pub exterior_reference_count_after_prune: usize,
    /// Total emitted placement count after merge processing and final ordinal reconciliation.
    pub total_reference_count_after_merge: usize,
    /// Exterior emitted placement count after merge processing and final ordinal reconciliation.
    pub exterior_reference_count_after_merge: usize,
    /// Outcome and work counters from buried-reference culling.
    #[serde(default)]
    pub burial: crate::usage::BurialStats,
    /// What the mwscript disable rules excluded, and the Rule B exclusion list.
    #[serde(default)]
    pub script_disable: crate::usage::ScriptDisableReport,
}

/// Metrics captured while building static meshes.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct StaticMetrics {
    /// Number of source meshes requested from usage analysis.
    pub requested_mesh_count: usize,
    /// Number of intermediate statics produced before filtering.
    pub generated_static_count: usize,
    /// Number of statics that survived the minimum-size filter (`0` on a static bundle cache hit).
    pub after_size_filter_static_count: usize,
    /// Number of placed statics that survived merge processing (`0` on a static bundle cache hit).
    pub after_merge_static_count: usize,
    /// Exterior-reference merge simplification diagnostics (empty on a static bundle cache hit).
    #[serde(default)]
    pub merge_simplification: crate::MergeSimplificationMetrics,
    /// Number of final serialized statics (`0` on a static bundle cache hit).
    pub final_static_count: usize,
    /// Total subset count across final serialized statics (`0` on a static bundle cache hit).
    pub final_subset_count: usize,
    /// Total vertex count across final serialized statics (`0` on a static bundle cache hit).
    pub final_vertex_count: usize,
    /// Total triangle count across final serialized statics (`0` on a static bundle cache hit).
    pub final_triangle_count: usize,
    /// Combined serialized byte size of all fixed static-mesh shards.
    pub static_meshes_serialized_size_bytes: u64,
}

/// Metrics captured while building static texture atlases.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct AtlasMetrics {
    /// Number of opaque source textures considered for atlas packing.
    pub opaque_texture_count: usize,
    /// Number of alpha-tested/translucent source textures considered for atlas packing.
    pub alpha_texture_count: usize,
    /// Cached opaque atlas page count when atlas reuse was possible.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_opaque_page_count: Option<u32>,
    /// Cached alpha atlas page count when atlas reuse was possible.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_alpha_page_count: Option<u32>,
    /// Whether the opaque family reused its committed rectangle layout.
    pub opaque_layout_hit: bool,
    /// Whether the alpha family reused its committed rectangle layout.
    pub alpha_layout_hit: bool,
    /// Number of source textures actually decoded while deciding atlas work.
    pub decoded_texture_count: usize,
    /// Number of opaque pages carried from committed state.
    pub carried_opaque_page_count: usize,
    /// Number of alpha pages carried from committed state.
    pub carried_alpha_page_count: usize,
    /// Number of opaque pages selected for fresh publication.
    pub built_opaque_page_count: usize,
    /// Number of alpha pages selected for fresh publication.
    pub built_alpha_page_count: usize,
    /// Number of pages selected for fresh publication or staged cache reuse.
    pub dirty_page_count: usize,
    /// Detailed opaque-family allocator and binding metrics.
    #[serde(default)]
    pub opaque_family: AtlasFamilyMetrics,
    /// Detailed alpha-family allocator and binding metrics.
    #[serde(default)]
    pub alpha_family: AtlasFamilyMetrics,
    /// Number of reconciled families that changed evidence without writing an atlas page.
    pub zero_page_write_reconciliation_count: usize,
    /// Conservative upper estimate for bytes selected by atlas publication.
    pub publication_bytes_estimate: u64,
}

/// Metrics captured while building terrain outputs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct TerrainMetrics {
    /// Number of packed terrain atlas regions.
    pub atlas_region_count: usize,
    /// Horizontal cell span of the terrain atlas.
    pub atlas_span_x: i32,
    /// Vertical cell span of the terrain atlas.
    pub atlas_span_y: i32,
    /// Number of terrain cells covered by the atlas layout.
    pub atlas_covered_cell_count: usize,
    /// Number of terrain textures loaded into the texture cache.
    pub terrain_texture_count: usize,
    /// Side length in pixels of `terrain_atlas.dds`.
    pub atlas_size: u32,
    /// Logical source tile size in pixels inside `terrain_atlas.dds`.
    pub logical_tile_size: u32,
    /// Physical tile size in pixels, including gutters.
    pub physical_tile_size: u32,
    /// Number of atlas tiles per row.
    pub tiles_per_row: u32,
    /// Maximum source-atlas mip index physically present in `terrain_atlas.dds`.
    pub atlas_max_lod: u32,
    /// Material/control texture size in texels on X and Y.
    pub material_size_xy: [u32; 2],
    /// Estimated control-texture memory footprint used by the rectangular guard.
    pub estimated_control_texture_bytes: u64,
    /// Estimated source-atlas memory footprint used by the rectangular guard report.
    pub estimated_source_atlas_bytes: u64,
    /// Number of serialized terrain meshes in `terrain.bin`.
    pub terrain_mesh_count: usize,
    /// Total vertex count across serialized terrain meshes.
    pub terrain_mesh_vertex_count: usize,
    /// Total triangle count across serialized terrain meshes.
    pub terrain_mesh_triangle_count: usize,
    /// Serialized byte size of `terrain.bin`.
    pub terrain_bin_size_bytes: u64,
    /// How the terrain mesh records for this run were produced.
    pub reuse_mode: TerrainReuseMode,
    /// Stable reason record reuse was unavailable, when it was rejected or bypassed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reuse_fallback_reason: Option<String>,
    /// Total mesh work items implied by the current layout.
    pub mesh_work_items: usize,
    /// Work items rebuilt because a declared chunk dependency was dirty.
    pub rebuilt_work_items: usize,
    /// Clean work items whose committed record was carried.
    pub reused_records: usize,
    /// Clean work items that emitted no record before and still emit none.
    pub reused_absent_work_items: usize,
    /// Rebuilt work keys, bounded for readability.
    pub rebuilt_keys: Vec<TerrainMeshWorkKey>,
    /// Whether `rebuilt_keys` omitted entries.
    pub rebuilt_keys_truncated: bool,
    /// Whether the five height-independent DDS products were carried rather than rebuilt.
    pub dds_carried: bool,
    /// Whether the coarse whole-domain occlusion builder ran.
    ///
    /// Occlusion remains explicitly coarse: this records that its builder scanned
    /// the complete height domain, independently of the write decision its content gate reached.
    pub occlusion_built_coarse: bool,
}

/// How one run produced its terrain mesh records.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerrainReuseMode {
    /// Terrain was disabled or no decision was reached.
    #[default]
    NotApplicable,
    /// Every terrain artifact was unchanged and the committed package was carried whole.
    CarriedPackage,
    /// Only work items with a dirty dependency were rebuilt; the rest were carried.
    PartialRebuild,
    /// Every work item was rebuilt.
    FullRebuild,
}

/// Estimated GPU-resident bytes for the generated distant-land set.
///
/// These are the exact payload sizes of the published artifacts that MGE XE uploads to the
/// GPU. They are a proxy for runtime residency, not a measurement of it: file headers and
/// record metadata are included, and render targets, the game's own resources, and driver
/// allocation overhead are not.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct DistantLandGpuMemoryEstimate {
    /// Static vertex/index data stored in all fixed static shards.
    pub static_geometry_bytes: u64,
    /// Compressed static-atlas DDS payloads.
    pub static_texture_bytes: u64,
    /// Terrain vertex/index data stored in `terrain.bin`.
    pub terrain_geometry_bytes: u64,
    /// Terrain source atlas, control maps, patch albedo, and blend-pattern DDS files.
    pub terrain_texture_bytes: u64,
}

impl DistantLandGpuMemoryEstimate {
    /// Sum of the four categories.
    ///
    /// Saturating because this is reporting code and must not panic on a corrupt report.
    pub const fn total_bytes(self) -> u64 {
        self.static_geometry_bytes
            .saturating_add(self.static_texture_bytes)
            .saturating_add(self.terrain_geometry_bytes)
            .saturating_add(self.terrain_texture_bytes)
    }
}

/// Sums the published artifact inventory into GPU-memory categories.
///
/// Counts the final committed set, so carried atlas pages and reused static shards are
/// included alongside freshly written ones. Host-only artifacts (version, usage, occlusion)
/// are never uploaded and are excluded.
pub(super) fn summarize_gpu_memory(artifacts: &[RequiredArtifact]) -> DistantLandGpuMemoryEstimate {
    let mut estimate = DistantLandGpuMemoryEstimate::default();
    for artifact in artifacts {
        let field = match artifact.kind {
            ArtifactKind::StaticShard => &mut estimate.static_geometry_bytes,
            ArtifactKind::AtlasDds => &mut estimate.static_texture_bytes,
            ArtifactKind::TerrainPayload => &mut estimate.terrain_geometry_bytes,
            ArtifactKind::TerrainDds => &mut estimate.terrain_texture_bytes,
            ArtifactKind::Version | ArtifactKind::Usage | ArtifactKind::TerrainOcclusion => {
                continue;
            }
        };
        *field = field.saturating_add(artifact.byte_length);
    }
    estimate
}

/// Aggregate metrics written into the generation report.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct GenerationMetrics {
    /// Plugin-usage metrics.
    pub usage: UsageMetrics,
    /// Static-mesh metrics.
    pub statics: StaticMetrics,
    /// Static-atlas metrics.
    pub atlas: AtlasMetrics,
    /// Terrain-generation metrics.
    pub terrain: TerrainMetrics,
    /// Estimated GPU-resident bytes of the published distant-land set.
    pub gpu_memory: DistantLandGpuMemoryEstimate,
}

/// Returns `(subset_count, vertex_count, triangle_count)` for a set of distant statics.
pub(super) fn summarize_distant_statics(distant_statics: &crate::PackedDistantStatics) -> (usize, usize, usize) {
    distant_statics
        .values()
        .flat_map(|static_mesh| static_mesh.subsets.iter())
        .fold((0, 0, 0), |(subsets, vertices, triangles), subset| {
            (
                subsets + 1,
                vertices + subset.vertices.len(),
                triangles + subset.triangles.len(),
            )
        })
}

/// Returns `(mesh_count, vertex_count, triangle_count)` for a serialized terrain file.
pub(super) fn summarize_terrain_file(terrain_file: &crate::mge_xe::distant_terrain::TerrainFile) -> (usize, usize, usize) {
    terrain_file
        .meshes
        .iter()
        .fold((0, 0, 0), |(meshes, vertices, triangles), mesh| {
            (meshes + 1, vertices + mesh.vertices.len(), triangles + mesh.triangles.len())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(kind: ArtifactKind, byte_length: u64) -> RequiredArtifact {
        RequiredArtifact {
            kind,
            relative_path: format!("{kind:?}-{byte_length}"),
            byte_length,
            content_blake3: [0u8; 32],
        }
    }

    #[test]
    fn each_uploaded_kind_lands_in_its_own_category() {
        let estimate = summarize_gpu_memory(&[
            artifact(ArtifactKind::StaticShard, 11),
            artifact(ArtifactKind::AtlasDds, 22),
            artifact(ArtifactKind::TerrainPayload, 33),
            artifact(ArtifactKind::TerrainDds, 44),
        ]);

        assert_eq!(
            estimate,
            DistantLandGpuMemoryEstimate {
                static_geometry_bytes: 11,
                static_texture_bytes: 22,
                terrain_geometry_bytes: 33,
                terrain_texture_bytes: 44,
            }
        );
        assert_eq!(estimate.total_bytes(), 110);
    }

    #[test]
    fn artifacts_of_one_kind_are_summed() {
        let estimate = summarize_gpu_memory(&[
            artifact(ArtifactKind::StaticShard, 5),
            artifact(ArtifactKind::StaticShard, 7),
            artifact(ArtifactKind::AtlasDds, 1),
            artifact(ArtifactKind::AtlasDds, 2),
            artifact(ArtifactKind::AtlasDds, 3),
        ]);

        assert_eq!(estimate.static_geometry_bytes, 12);
        assert_eq!(estimate.static_texture_bytes, 6);
        assert_eq!(estimate.terrain_geometry_bytes, 0);
        assert_eq!(estimate.terrain_texture_bytes, 0);
    }

    #[test]
    fn host_only_kinds_are_excluded() {
        let estimate = summarize_gpu_memory(&[
            artifact(ArtifactKind::Version, 100),
            artifact(ArtifactKind::Usage, 200),
            artifact(ArtifactKind::TerrainOcclusion, 400),
            artifact(ArtifactKind::StaticShard, 8),
        ]);

        assert_eq!(estimate.total_bytes(), 8);
        assert_eq!(estimate.static_geometry_bytes, 8);
    }

    #[test]
    fn empty_inventory_is_all_zero() {
        let estimate = summarize_gpu_memory(&[]);

        assert_eq!(estimate, DistantLandGpuMemoryEstimate::default());
        assert_eq!(estimate.total_bytes(), 0);
    }

    #[test]
    fn accumulation_and_total_saturate_instead_of_panicking() {
        let estimate = summarize_gpu_memory(&[
            artifact(ArtifactKind::StaticShard, u64::MAX),
            artifact(ArtifactKind::StaticShard, 1),
            artifact(ArtifactKind::AtlasDds, u64::MAX),
        ]);

        assert_eq!(estimate.static_geometry_bytes, u64::MAX);
        assert_eq!(estimate.static_texture_bytes, u64::MAX);
        assert_eq!(estimate.total_bytes(), u64::MAX);
    }
}
