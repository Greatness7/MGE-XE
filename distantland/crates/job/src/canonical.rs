//! Canonical identity-stream encoding of generation settings.
//!
//! The byte vocabulary here is a generation-identity interlock consumed by fingerprinting, not
//! ordinary model behavior: tag values and field order must stay stable across builds.

use distantland_foundation::units::CanonicalWriter;
use distantland_statics::{StaticTextureSizingMode, StaticTextureSizingSettings, TextureDedupeMode};

use crate::job::{GenerationSettings, TerrainDetail};

/// Canonical digest tag for [`crate::job::TerrainDetail`].
///
/// The identity streams write an explicit tag rather than casting the discriminant, so reordering
/// or inserting a variant cannot silently repoint every cached unit at another preset's bytes.
pub const fn terrain_detail_tag(detail: TerrainDetail) -> u8 {
    match detail {
        TerrainDetail::UltraHigh => 0,
        TerrainDetail::VeryHigh => 1,
        TerrainDetail::High => 2,
        TerrainDetail::Medium => 3,
        TerrainDetail::Low => 4,
    }
}

/// Canonical digest tag for [`TextureDedupeMode`]; see [`terrain_detail_tag`].
pub const fn texture_dedupe_mode_tag(mode: TextureDedupeMode) -> u8 {
    match mode {
        TextureDedupeMode::Off => 0,
        TextureDedupeMode::Exact => 1,
    }
}

/// Canonical digest tag for [`StaticTextureSizingMode`]; see [`terrain_detail_tag`].
pub const fn static_texture_sizing_mode_tag(mode: StaticTextureSizingMode) -> u8 {
    match mode {
        StaticTextureSizingMode::Off => 0,
        StaticTextureSizingMode::Report => 1,
        StaticTextureSizingMode::DownscaleOpaque => 2,
        StaticTextureSizingMode::Downscale => 3,
    }
}

/// Writes every generation setting into a canonical identity stream.
///
/// The exhaustive destructuring is intentional: adding a setting must fail compilation until
/// its identity behavior is selected.
pub fn write_generation_settings_canonical(writer: &mut CanonicalWriter, settings: &GenerationSettings) {
    let GenerationSettings {
        min_static_size,
        max_static_texture_long_axis,
        max_static_texture_short_axis,
        max_static_atlas_size,
        grass_density,
        force_rebuild: _,
        use_override_list,
        override_files,
        use_plugin_metadata,
        include_activators,
        include_misc,
        include_behaves_like_exterior,
        include_interiors_with_water,
        include_large_interiors,
        exclude_script_disable_targets,
        generate_terrain,
        max_terrain_texture_size,
        max_terrain_atlas_size,
        max_terrain_control_texture_size,
        max_terrain_control_texture_bytes,
        terrain_detail,
        terrain_mesh_smoothed_normal_weight,
        terrain_mesh_color_weight,
        static_mesh_target_error,
        static_mesh_normal_weight,
        static_mesh_color_weight,
        static_mesh_merge_error_multiplier,
        door_size_multiplier,
        merge_group_radius,
        texture_dedupe_mode,
        deep_water_static_cull_depth,
        static_texture_sizing,
    } = settings;
    let StaticTextureSizingSettings {
        mode,
        protected_density,
        min_texture_size,
        max_mip_reduction,
    } = static_texture_sizing;

    writer.write_f32(*min_static_size);
    writer.write_u32(*max_static_texture_long_axis);
    writer.write_u32(*max_static_texture_short_axis);
    writer.write_u32(*max_static_atlas_size);
    writer.write_f32(*grass_density);
    writer.write_bool(false);
    writer.write_bool(*use_override_list);
    writer.write_u64(override_files.len() as u64);
    for path in override_files {
        writer.write_str(&path.to_string_lossy());
    }
    writer.write_bool(*use_plugin_metadata);
    writer.write_bool(*include_activators);
    writer.write_bool(*include_misc);
    writer.write_bool(*include_behaves_like_exterior);
    writer.write_bool(*include_interiors_with_water);
    writer.write_bool(*include_large_interiors);
    writer.write_bool(*exclude_script_disable_targets);
    writer.write_bool(*generate_terrain);
    writer.write_u32(*max_terrain_texture_size);
    writer.write_u32(*max_terrain_atlas_size);
    writer.write_u32(*max_terrain_control_texture_size);
    writer.write_u64(*max_terrain_control_texture_bytes);
    writer.write_u8(terrain_detail_tag(*terrain_detail));
    writer.write_f32(*terrain_mesh_smoothed_normal_weight);
    writer.write_f32(*terrain_mesh_color_weight);
    writer.write_f32(*static_mesh_target_error);
    writer.write_f32(*static_mesh_normal_weight);
    writer.write_f32(*static_mesh_color_weight);
    writer.write_f32(*static_mesh_merge_error_multiplier);
    writer.write_f32(*door_size_multiplier);
    writer.write_f32(*merge_group_radius);
    writer.write_u8(texture_dedupe_mode_tag(*texture_dedupe_mode));
    writer.write_f32(*deep_water_static_cull_depth);
    writer.write_u8(static_texture_sizing_mode_tag(*mode));
    writer.write_f32(*protected_density);
    writer.write_u32(*min_texture_size);
    writer.write_u8(*max_mip_reduction);
}
