//! Per-unit projections of generation settings.

use crate::generation::job::{static_texture_sizing_mode_tag, terrain_detail_tag, texture_dedupe_mode_tag};
use crate::generation::units::{CanonicalWrite, CanonicalWriter};
use crate::{GenerationSettings, StaticTextureSizingMode, TerrainDetail, TextureDedupeMode};

/// Settings that directly affect static mesh unit products.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MeshUnitSettings {
    pub(crate) min_static_size: f32,
    pub(crate) door_size_multiplier: f32,
    pub(crate) target_error: f32,
    pub(crate) normal_weight: f32,
    pub(crate) color_weight: f32,
    pub(crate) merge_error_multiplier: f32,
}

/// Settings that directly affect an exterior-cell merge unit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MergeUnitSettings {
    pub(crate) group_radius: f32,
    pub(crate) target_error: f32,
    pub(crate) normal_weight: f32,
    pub(crate) color_weight: f32,
    pub(crate) merge_error_multiplier: f32,
    pub(crate) terrain_detail: TerrainDetail,
    pub(crate) subterrain_margin: f32,
}

/// Settings that affect static texture sizing, deduplication, and page packing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TextureUnitSettings {
    pub(crate) max_texture_long_axis: u32,
    pub(crate) max_texture_short_axis: u32,
    pub(crate) max_atlas_size: u32,
    pub(crate) sizing_mode: StaticTextureSizingMode,
    pub(crate) protected_density: f32,
    pub(crate) min_texture_size: u32,
    pub(crate) max_mip_reduction: u8,
    pub(crate) dedupe_mode: TextureDedupeMode,
}

/// Settings that affect terrain material-cell products.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TerrainCellUnitSettings {
    pub(crate) max_texture_size: u32,
    pub(crate) max_atlas_size: u32,
    pub(crate) dedupe_mode: TextureDedupeMode,
}

/// Settings that affect one absolute terrain mesh chunk.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TerrainChunkUnitSettings {
    pub(crate) detail: TerrainDetail,
    pub(crate) target_error: f32,
    pub(crate) smoothed_normal_weight: f32,
    pub(crate) color_weight: f32,
}

/// Audited settings partitions for every unit kind.
///
/// Projection/filter settings such as inclusion toggles and grass density belong to
/// statics-domain globals. Terrain control-map guards and `generate_terrain` likewise
/// belong to terrain-domain globals or execution policy. They are intentionally absent
/// here because no individual unit builder consumes them as product identity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct UnitSettingsPartitions {
    pub(crate) mesh: MeshUnitSettings,
    pub(crate) merge: MergeUnitSettings,
    pub(crate) texture: TextureUnitSettings,
    pub(crate) terrain_cell: TerrainCellUnitSettings,
    pub(crate) terrain_chunk: TerrainChunkUnitSettings,
}

impl UnitSettingsPartitions {
    /// Projects a generation job's settings into the five unit-specific partitions.
    ///
    /// The exhaustive destructure below is the enforcement point of the settings input audit: any
    /// field added to [`GenerationSettings`] fails to compile until it is explicitly classified
    /// here. Fields not owned by a static or terrain unit partition are bound to `_` and their
    /// ownership is documented in the settings-audit table in `docs/architecture/statics.md`.
    pub(crate) fn from_settings(settings: &GenerationSettings) -> Self {
        let GenerationSettings {
            // --- Static mesh + merge unit inputs (affect packed record bytes / merge grouping) ---
            min_static_size,
            door_size_multiplier,
            static_mesh_target_error,
            static_mesh_normal_weight,
            static_mesh_color_weight,
            static_mesh_merge_error_multiplier,
            merge_group_radius,
            // --- Static texture unit inputs ---
            max_static_texture_long_axis,
            max_static_texture_short_axis,
            max_static_atlas_size,
            texture_dedupe_mode,
            static_texture_sizing,
            // --- Terrain unit inputs ---
            max_terrain_texture_size,
            max_terrain_atlas_size,
            terrain_detail,
            terrain_mesh_smoothed_normal_weight,
            terrain_mesh_color_weight,
            // --- Not owned by any unit partition (see docs/architecture/statics.md audit table) ---
            // Statics-domain globals via projections: they decide which references and
            // grass placements exist (membership), caught by unit add/remove and clean-owner checks.
            //
            // Ignoring one here is only half the job: it still has to be classified in
            // `UsageSettingsProjection` (`projection.rs`), which is how a setting reaches
            // `statics_global` and so `statics_domain_digest`. That destructure is exhaustive too,
            // so the compiler enforces the second half. Adding the field to the struct and to
            // `write_canonical` when it belongs there is still a manual step.
            grass_density: _,
            use_override_list: _,
            override_files: _,
            use_plugin_metadata: _,
            include_activators: _,
            include_misc: _,
            include_behaves_like_exterior: _,
            include_interiors_with_water: _,
            include_large_interiors: _,
            exclude_script_disable_targets: _,
            deep_water_static_cull_depth: _,
            // Terrain-domain globals and control-map guards (not statics).
            generate_terrain: _,
            max_terrain_control_texture_size: _,
            max_terrain_control_texture_bytes: _,
            // Execution policy (excluded from `settings_identity`).
            force_rebuild: _,
        } = settings;

        let sizing = static_texture_sizing;
        Self {
            mesh: MeshUnitSettings {
                min_static_size: *min_static_size,
                door_size_multiplier: *door_size_multiplier,
                target_error: *static_mesh_target_error,
                normal_weight: *static_mesh_normal_weight,
                color_weight: *static_mesh_color_weight,
                merge_error_multiplier: *static_mesh_merge_error_multiplier,
            },
            merge: MergeUnitSettings {
                group_radius: *merge_group_radius,
                target_error: *static_mesh_target_error,
                normal_weight: *static_mesh_normal_weight,
                color_weight: *static_mesh_color_weight,
                merge_error_multiplier: *static_mesh_merge_error_multiplier,
                // Merged geometry is trimmed against the terrain, and the terrain simplifier's
                // absolute budget is the slack that trim must leave, so the preset is a merge input.
                terrain_detail: *terrain_detail,
                subterrain_margin: terrain_detail.target_error(),
            },
            texture: TextureUnitSettings {
                max_texture_long_axis: *max_static_texture_long_axis,
                max_texture_short_axis: *max_static_texture_short_axis,
                max_atlas_size: *max_static_atlas_size,
                sizing_mode: sizing.mode,
                protected_density: sizing.protected_density,
                min_texture_size: sizing.min_texture_size,
                max_mip_reduction: sizing.max_mip_reduction,
                dedupe_mode: *texture_dedupe_mode,
            },
            terrain_cell: TerrainCellUnitSettings {
                max_texture_size: *max_terrain_texture_size,
                max_atlas_size: *max_terrain_atlas_size,
                dedupe_mode: *texture_dedupe_mode,
            },
            terrain_chunk: TerrainChunkUnitSettings {
                detail: *terrain_detail,
                target_error: settings.terrain_mesh_target_error(),
                smoothed_normal_weight: *terrain_mesh_smoothed_normal_weight,
                color_weight: *terrain_mesh_color_weight,
            },
        }
    }
}

impl CanonicalWrite for MeshUnitSettings {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.write_f32(self.min_static_size);
        writer.write_f32(self.door_size_multiplier);
        writer.write_f32(self.target_error);
        writer.write_f32(self.normal_weight);
        writer.write_f32(self.color_weight);
        writer.write_f32(self.merge_error_multiplier);
    }
}

impl CanonicalWrite for MergeUnitSettings {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.write_f32(self.group_radius);
        writer.write_f32(self.target_error);
        writer.write_f32(self.normal_weight);
        writer.write_f32(self.color_weight);
        writer.write_f32(self.merge_error_multiplier);
        writer.write_u8(terrain_detail_tag(self.terrain_detail));
        writer.write_f32(self.subterrain_margin);
    }
}

impl CanonicalWrite for TextureUnitSettings {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.write_u32(self.max_texture_long_axis);
        writer.write_u32(self.max_texture_short_axis);
        writer.write_u32(self.max_atlas_size);
        writer.write_u8(static_texture_sizing_mode_tag(self.sizing_mode));
        writer.write_f32(self.protected_density);
        writer.write_u32(self.min_texture_size);
        writer.write_u8(self.max_mip_reduction);
        writer.write_u8(texture_dedupe_mode_tag(self.dedupe_mode));
    }
}

impl CanonicalWrite for TerrainCellUnitSettings {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.write_u32(self.max_texture_size);
        writer.write_u32(self.max_atlas_size);
        writer.write_u8(texture_dedupe_mode_tag(self.dedupe_mode));
    }
}

impl CanonicalWrite for TerrainChunkUnitSettings {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.write_u8(terrain_detail_tag(self.detail));
        writer.write_f32(self.target_error);
        writer.write_f32(self.smoothed_normal_weight);
        writer.write_f32(self.color_weight);
    }
}
