use super::*;
use crate::{GenerationSettings, StaticTextureSizingMode, TerrainDetail, TextureDedupeMode};

fn digest(value: &impl CanonicalWrite) -> [u8; 32] {
    let mut writer = CanonicalWriter::new();
    value.write_canonical(&mut writer);
    writer.finish()
}

#[test]
fn mesh_settings_fingerprint_tracks_every_declared_field() {
    let base = UnitSettingsPartitions::from_settings(&GenerationSettings::default()).mesh;
    macro_rules! assert_field_changes_digest {
        ($field:ident, $value:expr) => {{
            let mut changed = base;
            changed.$field = $value;
            assert_ne!(digest(&changed), digest(&base), stringify!($field));
        }};
    }
    assert_field_changes_digest!(min_static_size, base.min_static_size + 1.0);
    assert_field_changes_digest!(door_size_multiplier, base.door_size_multiplier + 1.0);
    assert_field_changes_digest!(target_error, base.target_error + 1.0);
    assert_field_changes_digest!(normal_weight, base.normal_weight + 1.0);
    assert_field_changes_digest!(color_weight, base.color_weight + 1.0);
    assert_field_changes_digest!(merge_error_multiplier, base.merge_error_multiplier + 1.0);
}

#[test]
fn merge_settings_fingerprint_tracks_every_declared_field() {
    let base = UnitSettingsPartitions::from_settings(&GenerationSettings::default()).merge;
    macro_rules! assert_field_changes_digest {
        ($field:ident) => {{
            let mut changed = base;
            changed.$field += 1.0;
            assert_ne!(digest(&changed), digest(&base), stringify!($field));
        }};
    }
    assert_field_changes_digest!(group_radius);
    assert_field_changes_digest!(target_error);
    assert_field_changes_digest!(normal_weight);
    assert_field_changes_digest!(color_weight);
    assert_field_changes_digest!(merge_error_multiplier);
}

#[test]
fn texture_settings_fingerprint_tracks_every_declared_field() {
    let base = UnitSettingsPartitions::from_settings(&GenerationSettings::default()).texture;
    macro_rules! assert_changed {
        ($field:ident, $value:expr) => {{
            let mut changed = base;
            changed.$field = $value;
            assert_ne!(digest(&changed), digest(&base), stringify!($field));
        }};
    }
    assert_changed!(max_atlas_size, base.max_atlas_size + 1);
    assert_changed!(sizing_mode, StaticTextureSizingMode::Report);
    assert_changed!(protected_density, base.protected_density + 1.0);
    assert_changed!(min_texture_size, base.min_texture_size + 1);
    assert_changed!(max_mip_reduction, base.max_mip_reduction + 1);
    assert_changed!(dedupe_mode, TextureDedupeMode::Off);
}

#[test]
fn terrain_settings_fingerprints_track_every_declared_field() {
    let partitions = UnitSettingsPartitions::from_settings(&GenerationSettings::default());
    let cell = partitions.terrain_cell;
    for changed in [
        TerrainCellUnitSettings {
            max_texture_size: cell.max_texture_size + 1,
            ..cell
        },
        TerrainCellUnitSettings {
            max_atlas_size: cell.max_atlas_size + 1,
            ..cell
        },
        TerrainCellUnitSettings {
            dedupe_mode: TextureDedupeMode::Off,
            ..cell
        },
    ] {
        assert_ne!(digest(&changed), digest(&cell));
    }

    let chunk = partitions.terrain_chunk;
    macro_rules! assert_chunk_changed {
        ($field:ident, $value:expr) => {{
            let mut changed = chunk;
            changed.$field = $value;
            assert_ne!(digest(&changed), digest(&chunk), stringify!($field));
        }};
    }
    assert_chunk_changed!(detail, TerrainDetail::Low);
    assert_chunk_changed!(target_error, chunk.target_error + 1.0);
    assert_chunk_changed!(smoothed_normal_weight, chunk.smoothed_normal_weight + 1.0);
    assert_chunk_changed!(color_weight, chunk.color_weight + 1.0);
}

#[test]
fn execution_and_domain_global_settings_do_not_enter_unit_partitions() {
    let base = GenerationSettings::default();
    let expected = UnitSettingsPartitions::from_settings(&base);
    let mut changed = base;
    changed.force_rebuild = !changed.force_rebuild;
    changed.generate_terrain = !changed.generate_terrain;
    changed.grass_density = 0.25;
    changed.include_misc = !changed.include_misc;
    changed.max_terrain_control_texture_size += 1;
    changed.max_terrain_control_texture_bytes += 1;
    assert_eq!(UnitSettingsPartitions::from_settings(&changed), expected);
}
