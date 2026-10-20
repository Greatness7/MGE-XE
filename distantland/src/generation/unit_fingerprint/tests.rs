use super::*;
use crate::generation::identity::ContentIdentity;
use crate::generation::units::{MergeUnitKey, TerrainCellUnitKey, TerrainChunkUnitKey};
use crate::statics::atlas::sizing::TextureAxisCaps;

#[test]
fn missing_resolution_and_content_markers_are_distinct() {
    assert_eq!(absent_mesh_status(true, true), 2);
    assert_eq!(absent_mesh_status(true, false), 3);
    assert_eq!(absent_mesh_status(false, false), 4);

    let fingerprints = [b"first".as_slice(), b"second".as_slice()].map(|bytes| {
        let identity = ContentIdentity::from_bytes(bytes);
        let mut writer = CanonicalWriter::new();
        write_content_identity(&mut writer, Some(&identity));
        writer.finish()
    });
    assert_ne!(fingerprints[0], fingerprints[1]);
}

#[test]
fn statics_domain_digest_is_sensitive_and_deterministic() {
    fn state(mesh: &[(&str, u8)], merge: &[(i32, i32, u8)], global: u8) -> GenerationState {
        GenerationState {
            units: UnitFingerprintTables {
                mesh: FingerprintTable::from_entries(mesh.iter().map(|(key, value)| ((*key).to_owned(), [*value; 32]))),
                merge: FingerprintTable::from_entries(
                    merge.iter().map(|(x, y, value)| (MergeUnitKey::new(*x, *y), [*value; 32])),
                ),
                ..UnitFingerprintTables::default()
            },
            statics_global: [global; 32],
            ..GenerationState::default()
        }
    }

    let binding = [4; 32];
    let baseline = state(&[("b", 2), ("a", 1)], &[(1, 0, 3)], 5);
    let permuted = state(&[("a", 1), ("b", 2)], &[(1, 0, 3)], 5);
    let digest = statics_domain_digest(&baseline, &binding);
    assert_eq!(digest, statics_domain_digest(&permuted, &binding));
    assert_ne!(
        digest,
        statics_domain_digest(&state(&[("a", 9), ("b", 2)], &[(1, 0, 3)], 5), &binding)
    );
    assert_ne!(
        digest,
        statics_domain_digest(&state(&[("a", 1), ("b", 2)], &[(1, 0, 9)], 5), &binding)
    );
    assert_ne!(
        digest,
        statics_domain_digest(&state(&[("a", 1), ("b", 2)], &[(1, 0, 3)], 9), &binding)
    );
    assert_ne!(digest, statics_domain_digest(&baseline, &[9; 32]));
    assert_ne!(
        digest,
        statics_domain_digest_with_serializer(&baseline, &binding, STATICS_RECIPE_VERSION + 1)
    );

    // The record-production recipe digest is folded into the domain digest, so a recipe bump busts
    // whole-bundle carry.
    let mut record_recipe_changed = baseline.clone();
    record_recipe_changed.statics_record_global_digest = [9; 32];
    assert_ne!(digest, statics_domain_digest(&record_recipe_changed, &binding));
}

#[test]
fn statics_record_global_digest_is_sensitive_to_the_recipe_version() {
    let baseline = statics_record_global_digest_with(STATICS_RECIPE_VERSION);

    // The public entry point uses the real recipe constant.
    assert_eq!(baseline, statics_record_global_digest());
    assert_ne!(baseline, statics_record_global_digest_with(STATICS_RECIPE_VERSION + 1));
}

fn sample_terrain_gate_inputs() -> TerrainGateInputs {
    TerrainGateInputs {
        recipe_version: 1,
        terrain_file_version: 2,
        land_cell_size: 3.0,
        default_patch_size: 4.0,
        pattern_tile_size: 5,
        pattern_gutter_size: 6,
        origin_cell: [11, 12],
        cell_size_xy: [13, 14],
        material_size_xy: [15, 16],
        atlas_spec: [17, 18, 19, 20, 21, 22],
        mesh_chunk_cells_per_side: 23,
        mesh_target_error: 24.0,
        mesh_weights: [25.0, 26.0],
        mesh_option_bits: 29,
        ordered_texture_paths: vec!["a.dds".into(), "b.dds".into()],
        default_texture_key: "default.dds".into(),
        default_texture_hash: [30; 32],
    }
}

#[test]
fn terrain_global_v2_is_sensitive_to_every_prepared_fact() {
    let settings = GenerationSettings::default();
    let layout = TerrainAtlasLayout::default();
    let baseline = sample_terrain_gate_inputs();
    let digest = fingerprint_terrain_global(&settings, &layout, &baseline);
    let assert_changed = |changed: TerrainGateInputs| {
        assert_ne!(digest, fingerprint_terrain_global(&settings, &layout, &changed));
    };

    macro_rules! changed {
        ($field:ident, $value:expr) => {{
            let mut changed = baseline.clone();
            changed.$field = $value;
            assert_changed(changed);
        }};
    }
    changed!(recipe_version, 31);
    changed!(terrain_file_version, 31);
    changed!(land_cell_size, 31.0);
    changed!(default_patch_size, 31.0);
    changed!(pattern_tile_size, 31);
    changed!(pattern_gutter_size, 31);
    changed!(origin_cell, [31, 12]);
    changed!(cell_size_xy, [31, 14]);
    changed!(material_size_xy, [31, 16]);
    changed!(atlas_spec, [31, 18, 19, 20, 21, 22]);
    changed!(mesh_chunk_cells_per_side, 31);
    changed!(mesh_target_error, 31.0);
    changed!(mesh_weights, [31.0, 26.0]);
    changed!(mesh_option_bits, 31);
    changed!(ordered_texture_paths, vec!["b.dds".into(), "a.dds".into()]);
    changed!(default_texture_key, "other.dds".into());
    changed!(default_texture_hash, [31; 32]);
}

#[test]
fn settings_identity_ignores_force_rebuild() {
    let mut forced = GenerationSettings::default();
    forced.force_rebuild = true;
    let mut cached = forced.clone();
    cached.force_rebuild = false;
    assert_eq!(
        fingerprint_settings(&forced),
        fingerprint_settings(&cached),
        "force_rebuild must not enter the committed settings identity used by no-op"
    );

    cached.min_static_size = 151.0;
    assert_ne!(
        fingerprint_settings(&forced),
        fingerprint_settings(&cached),
        "content-affecting settings must still change the identity"
    );
}

#[test]
fn terrain_domain_digest_is_sensitive_and_deterministic() {
    let state = GenerationState {
        units: UnitFingerprintTables {
            terrain_cell: FingerprintTable::from_entries([
                (TerrainCellUnitKey::new(1, 0), [1; 32]),
                (TerrainCellUnitKey::new(0, 0), [2; 32]),
            ]),
            terrain_chunk: FingerprintTable::from_entries([(TerrainChunkUnitKey::new(0, 0), [3; 32])]),
            ..UnitFingerprintTables::default()
        },
        terrain_global: [4; 32],
        ..GenerationState::default()
    };
    let mut changed = state.clone();
    changed.units.terrain_cell.entries[0].1 = [5; 32];
    assert_ne!(terrain_domain_digest(&state), terrain_domain_digest(&changed));
    changed = state.clone();
    changed.units.terrain_chunk.entries[0].1 = [5; 32];
    assert_ne!(terrain_domain_digest(&state), terrain_domain_digest(&changed));
    changed = state.clone();
    changed.terrain_global = [5; 32];
    assert_ne!(terrain_domain_digest(&state), terrain_domain_digest(&changed));
}

#[test]
fn coordinate_table_digest_matches_string_key_table_bytes() {
    let typed = FingerprintTable::from_entries([
        (MergeUnitKey::new(i32::MIN, i32::MIN), [0; 32]),
        (MergeUnitKey::new(10, -1), [1; 32]),
        (MergeUnitKey::new(2, 0), [2; 32]),
        (MergeUnitKey::new(-1, 4), [3; 32]),
        (MergeUnitKey::new(i32::MAX, i32::MAX), [4; 32]),
    ]);
    let mut typed_writer = CanonicalWriter::new();
    write_coordinate_fingerprint_table(&mut typed_writer, &typed);

    // The two writers must emit the same digest bytes for equivalent content, so that a table's
    // key type is a storage choice rather than a cache-invalidating one. Entries are ordered by
    // hand because String Ord alone would place "10,-1" before "2,0"; typed Ord sorts numerically,
    // which is the order the string table has to be given to match.
    let string_keyed = FingerprintTable {
        entries: vec![
            ("-2147483648,-2147483648".to_owned(), [0; 32]),
            ("-1,4".to_owned(), [3; 32]),
            ("2,0".to_owned(), [2; 32]),
            ("10,-1".to_owned(), [1; 32]),
            ("2147483647,2147483647".to_owned(), [4; 32]),
        ],
    };
    let mut string_writer = CanonicalWriter::new();
    write_path_fingerprint_table(&mut string_writer, &string_keyed);
    assert_eq!(typed_writer.into_bytes(), string_writer.into_bytes());
}

/// Two merge-eligible references in cell `(0, 0)`, the minimum that forms a merge unit.
fn merge_unit_world() -> (DistantStatics, UsageInfo<'static>) {
    let mut distant_statics: DistantStatics = Default::default();
    let mut usage: UsageInfo<'static> = UsageInfo::default();
    for (index, id) in ["a.nif", "b.nif"].into_iter().enumerate() {
        let mut distant_static = crate::statics::DistantStatic::default();
        distant_static.bounding_sphere = tes3::nif::NiBound {
            center: glam::Vec3::ZERO,
            radius: 64.0,
        };
        distant_statics.insert(id.to_string(), distant_static);
        usage.exterior_references_mut().insert(
            crate::usage::StableRefKey::test(index as u32 + 1),
            crate::usage::DistantReference {
                id: std::borrow::Cow::Borrowed(id),
                deleted: false,
                persistent: false,
                translation: glam::Vec3::new(1000.0 + index as f32 * 100.0, 1000.0, 0.0),
                rotation: glam::Vec3::ZERO,
                scale: 1.0,
                vis_index: 0,
            },
        );
    }
    (distant_statics, usage)
}

/// Fingerprints cell `(0, 0)`'s merge unit with the given per-cell height hashes.
fn merge_unit_fingerprint(settings: &GenerationSettings, heights: &[((i32, i32), u8)]) -> [u8; 32] {
    let (distant_statics, usage) = merge_unit_world();
    let mut projections = Projections::capture(&UsageInfo::default(), settings, &crate::StaticOverrides::default());
    for &(cell, fill) in heights {
        projections.landscapes.insert(
            cell,
            LandscapeCellProjection {
                height_hash: Some([fill; 32]),
                ..LandscapeCellProjection::default()
            },
        );
    }

    let partitions = UnitSettingsPartitions::from_settings(settings);
    let (merge, _) = build_merge_units(&usage, &distant_statics, &HashMap::new(), &partitions, &projections);
    merge
        .entries
        .iter()
        .find(|(key, _)| *key == MergeUnitKey::new(0, 0))
        .expect("the two references form one merge unit")
        .1
}

#[test]
fn merge_units_follow_the_heights_their_cull_can_sample() {
    let settings = GenerationSettings::default();
    let base = merge_unit_fingerprint(&settings, &[((0, 0), 1)]);

    assert_ne!(
        base,
        merge_unit_fingerprint(&settings, &[((0, 0), 2)]),
        "editing the unit's own LAND heights must dirty it"
    );
    assert_ne!(
        base,
        merge_unit_fingerprint(&settings, &[((0, 0), 1), ((1, 0), 2)]),
        "editing a neighbouring cell's heights must dirty it, since members can overhang"
    );
    assert_eq!(
        base,
        merge_unit_fingerprint(&settings, &[((0, 0), 1), ((2, 0), 2)]),
        "cells beyond the halo are out of reach and must not dirty it"
    );
}

#[test]
fn merge_units_follow_the_terrain_detail_that_sets_their_cull_margin() {
    let mut settings = GenerationSettings::default();
    settings.terrain_detail = crate::TerrainDetail::High;
    let base = merge_unit_fingerprint(&settings, &[((0, 0), 1)]);

    settings.terrain_detail = crate::TerrainDetail::Low;
    assert_ne!(
        base,
        merge_unit_fingerprint(&settings, &[((0, 0), 1)]),
        "the detail preset sets the cull margin, so it changes merged geometry"
    );
}

/// A mesh with two shapes, `PoolA` and `PoolB`, that differ in the colour of their material
/// alone: the bounds, the texture and the flags of the two are the same.
fn build_two_pool_nif() -> Vec<u8> {
    use glam::{Vec2, Vec3};
    use tes3::nif::*;

    let mut stream = NiStream::new();
    let texture = stream.insert(NiSourceTexture {
        source: TextureSource::External("pool.dds".into()),
        ..NiSourceTexture::default()
    });
    let mut texture_map = Map::default();
    texture_map.texture = texture;
    let texturing = stream.insert(NiTexturingProperty {
        texture_maps: vec![Some(TextureMap::Map(texture_map))],
        ..NiTexturingProperty::default()
    });

    let mut root = NiNode::default();
    for (name, emissive_color) in [("PoolA", Vec3::X), ("PoolB", Vec3::Y)] {
        let material = stream.insert(NiMaterialProperty {
            emissive_color,
            ..NiMaterialProperty::default()
        });
        let mut data = NiTriShapeData::default();
        data.vertices = vec![Vec3::ZERO, Vec3::X, Vec3::Y];
        data.normals = vec![Vec3::Z; 3];
        data.uv_sets = vec![Vec2::ZERO, Vec2::X, Vec2::Y];
        data.triangles = vec![[0, 1, 2]];
        data.update_center_radius();
        let data = stream.insert(data);

        let mut shape = NiTriShape::default();
        shape.name = name.into();
        shape.geometry_data = data.cast();
        shape.properties.push(texturing.cast());
        shape.properties.push(material.cast());
        root.children.push(stream.insert(shape).cast());
    }
    let root = stream.insert(root);
    stream.roots.push(root.cast());
    stream.save_bytes().expect("serialize test nif")
}

#[test]
fn water_names_that_choose_another_shape_change_the_fingerprint_of_a_water_mesh() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::create_dir_all(root.join("meshes")).unwrap();
    std::fs::create_dir_all(root.join("textures")).unwrap();
    std::fs::write(root.join("meshes").join("pools.nif"), build_two_pool_nif()).unwrap();
    std::fs::write(root.join("textures").join("pool.dds"), b"dds").unwrap();
    let vfs = Vfs {
        ini_path: root.join("Morrowind.ini"),
        data_dirs: vec![root.to_path_buf()],
        active_plugins: vec![],
        archives: vec![],
        maps: crate::vfs::directory_map::build_directory_map(&[root.to_path_buf()]).unwrap(),
    };

    let settings = GenerationSettings::default();
    let usage = UsageInfo::default();
    // The static of the mesh and its fingerprint, with this body name in the water names.
    let mesh_unit = |body_name: Option<&str>| {
        let mut overrides = crate::StaticOverrides::default();
        overrides.water_names.body = body_name.into_iter().map(str::to_owned).collect();
        let distant_static =
            crate::DistantStatic::from_nif_with_identity("pools.nif", &vfs, 1.0, 0.0, false, 1.0, false, &overrides)
                .distant_static
                .expect("static of the mesh");
        let mut distant_statics: DistantStatics = Default::default();
        distant_statics.insert("pools.nif".to_owned(), distant_static.clone());

        let source_info = HashMap::new();
        let state = build_static_state(
            &settings,
            &Projections::capture(&usage, &settings, &overrides),
            &ContentIdentityCollector::default(),
            &usage,
            &distant_statics,
            &vfs,
            &AtlasTextureSet::default(),
            &source_info,
            &SizingPlan::baseline(TextureAxisCaps::uniform(1024), &source_info),
        );
        assert_eq!(state.units.mesh.entries.len(), 1);
        (distant_static, state.units.mesh.entries[0].1)
    };

    // Each body name leaves one shape out, and the other shape is the water.
    let (without_a, fingerprint_without_a) = mesh_unit(Some("poola"));
    let (without_b, fingerprint_without_b) = mesh_unit(Some("poolb"));
    for water in [&without_a, &without_b] {
        assert_eq!(water.subsets.len(), 1);
        assert!(water.has_water());
    }

    // Everything that the fingerprint took from the static before is the same for the two.
    assert_eq!(without_a.bounding_sphere.center, without_b.bounding_sphere.center);
    assert_eq!(without_a.bounding_sphere.radius, without_b.bounding_sphere.radius);
    assert_eq!(without_a.bounding_box.min, without_b.bounding_box.min);
    assert_eq!(without_a.bounding_box.max, without_b.bounding_box.max);
    let (subset_a, subset_b) = (&without_a.subsets[0], &without_b.subsets[0]);
    assert_eq!(subset_a.has_alpha, subset_b.has_alpha);
    assert_eq!(subset_a.has_uv_controller, subset_b.has_uv_controller);
    assert_eq!(subset_a.water, subset_b.water);
    assert_eq!(subset_a.texture, subset_b.texture);
    // The output is not the same: the two shapes have water of different colours.
    assert_ne!(subset_a.vertices[0].color, subset_b.vertices[0].color);

    assert_ne!(fingerprint_without_a, fingerprint_without_b);
    assert_eq!(fingerprint_without_a, mesh_unit(Some("poola")).1);

    // The names do not enter the fingerprint of a mesh that they do not make water.
    let (ordinary, fingerprint_ordinary) = mesh_unit(None);
    assert_eq!(ordinary.subsets.len(), 2);
    assert!(!ordinary.has_water());
    assert_eq!(fingerprint_ordinary, mesh_unit(Some("lake")).1);
}

#[test]
fn the_dry_space_names_and_the_dry_space_key_change_the_fingerprint_of_a_mesh() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::create_dir_all(root.join("meshes")).unwrap();
    std::fs::create_dir_all(root.join("textures")).unwrap();
    std::fs::write(root.join("meshes").join("pools.nif"), build_two_pool_nif()).unwrap();
    std::fs::write(root.join("textures").join("pool.dds"), b"dds").unwrap();
    let vfs = Vfs {
        ini_path: root.join("Morrowind.ini"),
        data_dirs: vec![root.to_path_buf()],
        active_plugins: vec![],
        archives: vec![],
        maps: crate::vfs::directory_map::build_directory_map(&[root.to_path_buf()]).unwrap(),
    };

    let settings = GenerationSettings::default();
    let usage = UsageInfo::default();
    // The static of the mesh and its fingerprint, with this mask name in the water names and
    // this dry space key in the mesh entry.
    let mesh_unit = |mask_name: Option<&str>, dry_space: Option<bool>| {
        let mut overrides = crate::StaticOverrides::default();
        overrides.water_names.mask = mask_name.into_iter().map(str::to_owned).collect();
        if dry_space.is_some() {
            overrides.mesh_overrides.insert(
                "pools.nif".to_owned(),
                crate::StaticOverride {
                    dry_space,
                    ..Default::default()
                },
            );
        }
        let distant_static =
            crate::DistantStatic::from_nif_with_identity("pools.nif", &vfs, 1.0, 0.0, false, 1.0, false, &overrides)
                .distant_static
                .expect("static of the mesh");
        let mut distant_statics: DistantStatics = Default::default();
        distant_statics.insert("pools.nif".to_owned(), distant_static.clone());

        let source_info = HashMap::new();
        let state = build_static_state(
            &settings,
            &Projections::capture(&usage, &settings, &overrides),
            &ContentIdentityCollector::default(),
            &usage,
            &distant_statics,
            &vfs,
            &AtlasTextureSet::default(),
            &source_info,
            &SizingPlan::baseline(TextureAxisCaps::uniform(1024), &source_info),
        );
        assert_eq!(state.units.mesh.entries.len(), 1);
        (distant_static, state.units.mesh.entries[0].1)
    };

    // Without a mask name the mesh is two ordinary shapes.
    let (ordinary, fingerprint_ordinary) = mesh_unit(None, None);
    assert_eq!(ordinary.subsets.len(), 2);
    assert!(!ordinary.stays_alone());

    // With the name one shape is a dry space, and the static stays out of the merges.
    let (with_b, fingerprint_with_b) = mesh_unit(Some("poolb"), None);
    assert_eq!(with_b.subsets.len(), 2);
    assert!(with_b.stays_alone() && !with_b.has_water());
    assert_ne!(fingerprint_ordinary, fingerprint_with_b);
    assert_eq!(fingerprint_with_b, mesh_unit(Some("poolb"), None).1);

    // Another name chooses another shape.
    assert_ne!(fingerprint_with_b, mesh_unit(Some("poola"), None).1);

    // The key of the mesh entry leaves the dry space out.
    let (refused, fingerprint_refused) = mesh_unit(Some("poolb"), Some(false));
    assert_eq!(refused.subsets.len(), 1);
    assert!(!refused.stays_alone());
    assert_ne!(fingerprint_refused, fingerprint_with_b);
    assert_ne!(fingerprint_refused, fingerprint_ordinary);
}
