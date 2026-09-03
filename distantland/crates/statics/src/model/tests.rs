use super::*;
use std::fs;
use std::path::{Path, PathBuf};

use bytemuck;
use itertools::Itertools;
use tempfile::tempdir;

fn pack_subset_normal_and_emissive(emissive: f32) -> [u8; 4] {
    let vfs = crate::Vfs {
        ini_path: PathBuf::from("Morrowind.ini"),
        data_dirs: vec![],
        active_plugins: vec![],
        archives: vec![],
        maps: crate::vfs::directory_map::DirectoryMaps::default(),
    };
    DistantStatic {
        static_type: StaticType::StaticAuto,
        subsets: vec![Subset {
            emissive,
            vertices: vec![Vertex {
                normal: Vec3::new(1.0, 0.0, 0.0),
                color: Vec4::ONE,
                ..Vertex::default()
            }],
            triangles: vec![[0, 0, 0]],
            ..Subset::default()
        }],
        ..DistantStatic::default()
    }
    .into_distant_static(&vfs, 1.0)
    .subsets[0]
        .vertices[0]
        .normal
}

#[test]
fn static_vertex_color_is_packed_as_d3dcolor_bytes() {
    let vfs = crate::Vfs {
        ini_path: PathBuf::from("Morrowind.ini"),
        data_dirs: vec![],
        active_plugins: vec![],
        archives: vec![],
        maps: crate::vfs::directory_map::DirectoryMaps::default(),
    };
    let packed = DistantStatic {
        subsets: vec![Subset {
            vertices: vec![Vertex {
                color: Vec4::new(0.0, 0.5, 1.0, 1.0),
                ..Vertex::default()
            }],
            triangles: vec![[0, 0, 0]],
            ..Subset::default()
        }],
        ..DistantStatic::default()
    }
    .into_distant_static(&vfs, 1.0);

    assert_eq!(packed.subsets[0].vertices[0].color, [255, 128, 0, 255]);
}

#[test]
fn building_statics_use_doubled_cutoff_radius() {
    assert!(!passes_min_radius(4.0, StaticType::StaticAuto, false, 1.0, 6.0, 1.0));
    assert!(passes_min_radius(4.0, StaticType::StaticBuilding, false, 1.0, 6.0, 1.0));
}

#[test]
fn door_statics_use_door_multiplier_cutoff_radius() {
    assert!(!passes_min_radius(4.0, StaticType::StaticAuto, true, 1.0, 6.0, 1.0));
    assert!(passes_min_radius(4.0, StaticType::StaticAuto, true, 1.0, 6.0, 2.0));
    assert!(!passes_min_radius(4.0, StaticType::StaticAuto, false, 1.0, 6.0, 2.0));
}

#[test]
fn grass_statics_bypass_min_radius_filter() {
    // Grass meshes are intentionally tiny; they must always pass regardless of min_static_size.
    assert!(passes_min_radius(0.1, StaticType::StaticGrass, false, 1.0, 150.0, 1.0));
    assert!(passes_min_radius(0.0, StaticType::StaticGrass, false, 1.0, 150.0, 1.0));
    assert!(!passes_min_radius(0.1, StaticType::StaticAuto, false, 1.0, 150.0, 1.0));
}

#[test]
fn explicit_distance_tiers_bypass_min_radius_filter() {
    // An `.ovr` entry of `= near|far|very_far` overrides the size cutoff, matching the legacy
    // generator. Distant Lights depends on this for lanterns and lit windows well under 150 units.
    for static_type in [StaticType::StaticNear, StaticType::StaticFar, StaticType::StaticVeryFar] {
        assert!(passes_min_radius(0.1, static_type, false, 1.0, 150.0, 1.0));
    }
    assert!(!passes_min_radius(0.1, StaticType::StaticTree, false, 1.0, 150.0, 1.0));
    assert!(!passes_min_radius(0.1, StaticType::StaticBuilding, false, 1.0, 150.0, 1.0));
}

#[test]
fn inferred_static_type_requires_directory_prefixes() {
    assert!(matches!(inferred_static_type("grass\\foo.nif"), StaticType::StaticGrass));
    assert!(matches!(inferred_static_type("trees\\foo.nif"), StaticType::StaticTree));
    assert!(matches!(inferred_static_type("x\\foo.nif"), StaticType::StaticBuilding));
    assert!(matches!(inferred_static_type("X\\foo.nif"), StaticType::StaticBuilding));
}

#[test]
fn inferred_static_type_ignores_non_directory_prefixes() {
    assert!(matches!(inferred_static_type("grassland\\foo.nif"), StaticType::StaticAuto));
    assert!(matches!(inferred_static_type("treeshouse\\foo.nif"), StaticType::StaticAuto));
    assert!(matches!(inferred_static_type("xtest\\foo.nif"), StaticType::StaticAuto));
    assert!(matches!(inferred_static_type("xfoo.nif"), StaticType::StaticAuto));
    assert!(matches!(inferred_static_type("x"), StaticType::StaticAuto));
}

#[test]
fn explicit_building_override_controls_static_type() {
    let overrides = crate::parse_overrides_texts(&["foo.nif = building"]).expect("override parse");
    let static_type = resolve_static_type("foo.nif", false, &overrides).expect("static type");
    assert!(matches!(static_type, StaticType::StaticBuilding));
}

#[test]
fn explicit_type_override_is_last_token_wins() {
    let overrides = crate::parse_overrides_texts(&["foo.nif = building far"]).expect("override parse");
    let static_type = resolve_static_type("foo.nif", false, &overrides).expect("static type");
    assert!(matches!(static_type, StaticType::StaticFar));
}

#[test]
fn static_min_radius_reads_the_building_cutoff_off_the_static() {
    let sized = |static_type| DistantStatic {
        bounding_sphere: NiBound {
            center: Vec3::ZERO,
            radius: 4.0,
        },
        static_type,
        max_scale: 1.0,
        ..DistantStatic::default()
    };

    assert!(!passes_static_min_radius(&sized(StaticType::StaticAuto), 6.0, 1.0));
    assert!(passes_static_min_radius(&sized(StaticType::StaticBuilding), 6.0, 1.0));
}

#[test]
fn door_static_inflates_only_written_top_level_sphere_radius() {
    let vfs = crate::Vfs {
        ini_path: PathBuf::from("Morrowind.ini"),
        data_dirs: vec![],
        active_plugins: vec![],
        archives: vec![],
        maps: crate::vfs::directory_map::DirectoryMaps::default(),
    };

    let make = |is_door: bool| DistantStatic {
        bounding_sphere: NiBound {
            center: Vec3::ZERO,
            radius: 100.0,
        },
        is_door,
        subsets: vec![Subset {
            bounding_sphere: NiBound {
                center: Vec3::ZERO,
                radius: 50.0,
            },
            vertices: vec![Vertex::default()],
            triangles: vec![[0, 0, 0]],
            ..Subset::default()
        }],
        ..DistantStatic::default()
    };

    // Only the written top-level sphere radius is multiplied.
    let door = make(true).into_distant_static(&vfs, 4.0);
    assert_eq!(door.bounding_sphere.radius, 400.0);
    assert_eq!(door.subsets[0].bounding_sphere.radius, 50.0);

    let stat = make(false).into_distant_static(&vfs, 4.0);
    assert_eq!(stat.bounding_sphere.radius, 100.0);
    assert_eq!(stat.subsets[0].bounding_sphere.radius, 50.0);
}

#[test]
fn horizon_footprint_requires_explicit_eligibility() {
    let vfs = crate::Vfs {
        ini_path: PathBuf::from("Morrowind.ini"),
        data_dirs: vec![],
        active_plugins: vec![],
        archives: vec![],
        maps: crate::vfs::directory_map::DirectoryMaps::default(),
    };
    let subset = Subset {
        bounding_box: BoundingBox {
            min: Vec3::new(0.0, 0.0, 0.0),
            max: Vec3::new(4.0, 4.0, 3.0),
        },
        vertices: vec![
            Vertex {
                position: Vec3::new(0.0, 0.0, 1.0),
                ..Vertex::default()
            },
            Vertex {
                position: Vec3::new(4.0, 0.0, 2.0),
                ..Vertex::default()
            },
            Vertex {
                position: Vec3::new(0.0, 4.0, 3.0),
                ..Vertex::default()
            },
        ],
        triangles: vec![[0, 1, 2]],
        texture: SubsetTexture::AtlasPage(0),
        ..Subset::default()
    };

    let ordinary = DistantStatic {
        subsets: vec![subset.clone()],
        ..DistantStatic::default()
    }
    .into_distant_static(&vfs, 1.0);
    assert_eq!(ordinary.subsets[0].horizon_footprint.vertex_count, 0);

    let eligible = DistantStatic {
        subsets: vec![subset],
        horizon_footprint_eligible: true,
        ..DistantStatic::default()
    }
    .into_distant_static(&vfs, 1.0);
    assert_eq!(eligible.subsets[0].horizon_footprint.vertex_count, 3);
    assert_eq!(eligible.subsets[0].horizon_footprint.max_z, 3.0);
}

fn test_vfs() -> crate::Vfs {
    crate::Vfs {
        ini_path: PathBuf::from("Morrowind.ini"),
        data_dirs: vec![],
        active_plugins: vec![],
        archives: vec![],
        maps: crate::vfs::directory_map::DirectoryMaps::default(),
    }
}

fn bound(min_x: f32) -> UvBound {
    UvBound {
        min_y: 0.0,
        max_x: min_x + 0.25,
        min_x,
        max_y: 1.0,
    }
}

#[test]
fn packing_builds_a_first_appearance_palette_and_writes_ordinals() {
    let a = bound(0.0);
    let b = bound(0.5);
    let vertex = |uv_bound| Vertex {
        uv_bound,
        ..Vertex::default()
    };

    let packed = DistantStatic {
        subsets: vec![Subset {
            // b appears first, a second, then b again: ordinals must be 0, 1, 0.
            vertices: vec![vertex(b), vertex(a), vertex(b)],
            triangles: vec![[0, 1, 2]],
            uv_bounds: vec![a, b],
            ..Subset::default()
        }],
        ..DistantStatic::default()
    }
    .into_distant_static(&test_vfs(), 1.0);

    let subset = &packed.subsets[0];
    assert_eq!(
        subset.palette,
        vec![
            UvBoundRecord {
                bound: [b.min_y, b.max_x, b.min_x, b.max_y],
            },
            UvBoundRecord {
                bound: [a.min_y, a.max_x, a.min_x, a.max_y],
            },
        ]
    );
    let ordinals: Vec<f32> = subset.vertices.iter().map(|vertex| vertex.position[3].to_f32()).collect();
    assert_eq!(ordinals, vec![0.0, 1.0, 0.0]);
}

#[test]
fn grass_packing_emits_no_palette_and_keeps_position_w_at_one() {
    let packed = DistantStatic {
        static_type: StaticType::StaticGrass,
        subsets: vec![Subset {
            vertices: vec![Vertex {
                uv_bound: bound(0.5),
                ..Vertex::default()
            }],
            triangles: vec![[0, 0, 0]],
            uv_bounds: vec![bound(0.5)],
            ..Subset::default()
        }],
        ..DistantStatic::default()
    }
    .into_distant_static(&test_vfs(), 1.0);

    assert!(packed.subsets[0].palette.is_empty());
    assert_eq!(packed.subsets[0].vertices[0].position[3], half::f16::ONE);
}

#[test]
fn emissive_material_is_packed_into_normal_w() {
    let normal = pack_subset_normal_and_emissive(0.4);
    assert_eq!(normal, [255, 127, 127, 102]);
}

#[test]
fn missing_emissive_defaults_to_zero_in_normal_w() {
    let normal = pack_subset_normal_and_emissive(0.0);
    assert_eq!(normal[3], 0);
}

#[test]
fn emissive_packing_uses_byte_quantization() {
    let normal = pack_subset_normal_and_emissive(0.5);
    assert_eq!(normal[3], 127);
}

fn make_test_vfs(dir: &Path) -> Vfs {
    use crate::vfs::directory_map::build_directory_map;

    let maps = build_directory_map(&[dir.to_path_buf()]).expect("directory map");
    Vfs {
        ini_path: dir.join("Morrowind.ini"),
        data_dirs: vec![dir.to_path_buf()],
        active_plugins: vec![],
        archives: vec![],
        maps,
    }
}

fn build_test_static_nif(has_controller: bool, has_scroll_tag: bool) -> Vec<u8> {
    let mut stream = NiStream::new();

    let texture_link = stream.insert(NiSourceTexture {
        source: TextureSource::External("uv_anim\\ghost.dds".into()),
        ..NiSourceTexture::default()
    });

    let mut texture_map = Map::default();
    texture_map.texture = texture_link;

    let texturing_property_link = stream.insert(NiTexturingProperty {
        texture_maps: vec![Some(TextureMap::Map(texture_map))],
        ..NiTexturingProperty::default()
    });

    let mut geometry_data = NiTriShapeData::default();
    geometry_data.vertices = vec![Vec3::ZERO, Vec3::X, Vec3::Y];
    geometry_data.normals = vec![Vec3::Z; 3];
    geometry_data.uv_sets = vec![Vec2::ZERO, Vec2::X, Vec2::Y];
    geometry_data.triangles = vec![[0, 1, 2]];
    geometry_data.update_center_radius();
    let geometry_data_link = stream.insert(geometry_data);

    let controller_link = has_controller.then(|| stream.insert(NiUVController::default()));
    let extra_data_link = has_scroll_tag.then(|| {
        stream.insert(NiStringExtraData {
            value: "mge.distant.scroll".into(),
            ..NiStringExtraData::default()
        })
    });

    let mut shape = NiTriShape::default();
    shape.geometry_data = geometry_data_link.cast();
    shape.properties.push(texturing_property_link.cast());
    if let Some(link) = controller_link {
        shape.controller = link.cast();
    }
    if let Some(link) = extra_data_link {
        shape.extra_data = link.cast();
    }

    let shape_link = stream.insert(shape);
    stream.roots.push(shape_link.cast());
    stream.save_bytes().expect("serialize test nif")
}

fn write_test_static_asset(root: &Path, mesh_name: &str, nif_bytes: &[u8]) {
    let mesh_path = root.join("meshes").join(mesh_name);
    let texture_path = root.join("textures").join("uv_anim").join("ghost.dds");
    fs::create_dir_all(mesh_path.parent().expect("mesh parent")).expect("mesh dir");
    fs::create_dir_all(texture_path.parent().expect("texture parent")).expect("texture dir");
    fs::write(mesh_path, nif_bytes).expect("write mesh");
    fs::write(texture_path, b"dds").expect("write texture");
}

fn write_test_mesh_asset(root: &Path, mesh_name: &str, nif_bytes: &[u8]) {
    let mesh_path = root.join("meshes").join(mesh_name);
    fs::create_dir_all(mesh_path.parent().expect("mesh parent")).expect("mesh dir");
    fs::write(mesh_path, nif_bytes).expect("write mesh");
}

#[test]
fn from_nif_marks_uv_animated_subsets_only_when_controller_and_tag_exist() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    write_test_static_asset(root, "animated.nif", &build_test_static_nif(true, true));
    write_test_static_asset(root, "missing_tag.nif", &build_test_static_nif(true, false));
    write_test_static_asset(root, "missing_controller.nif", &build_test_static_nif(false, true));
    let vfs = make_test_vfs(root);

    let animated = DistantStatic::from_nif_with_identity(
        "animated.nif",
        &vfs,
        1.0,
        0.0,
        false,
        1.0,
        false,
        &StaticOverrides::default(),
    )
    .distant_static
    .expect("animated static");
    let missing_tag = DistantStatic::from_nif_with_identity(
        "missing_tag.nif",
        &vfs,
        1.0,
        0.0,
        false,
        1.0,
        false,
        &StaticOverrides::default(),
    )
    .distant_static
    .expect("missing-tag static");
    let missing_controller = DistantStatic::from_nif_with_identity(
        "missing_controller.nif",
        &vfs,
        1.0,
        0.0,
        false,
        1.0,
        false,
        &StaticOverrides::default(),
    )
    .distant_static
    .expect("missing-controller static");

    assert_eq!(animated.subsets.len(), 1);
    assert!(animated.subsets[0].has_uv_controller);
    assert_eq!(missing_tag.subsets.len(), 1);
    assert!(!missing_tag.subsets[0].has_uv_controller);
    assert_eq!(missing_controller.subsets.len(), 1);
    assert!(!missing_controller.subsets[0].has_uv_controller);
}

#[test]
fn from_nif_remaps_missing_static_texture_to_error_texture() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    write_test_mesh_asset(root, "missing_texture.nif", &build_test_static_nif(false, false));
    let vfs = make_test_vfs(root);

    let static_mesh = DistantStatic::from_nif_with_identity(
        "missing_texture.nif",
        &vfs,
        1.0,
        0.0,
        false,
        1.0,
        false,
        &StaticOverrides::default(),
    )
    .distant_static
    .expect("static mesh");
    let sym = static_mesh.subsets[0].texture.source_sym().expect("source texture");

    assert_eq!(vfs.texture_key_for_sym(sym), Some(crate::vfs::STATIC_ERROR_TEXTURE_KEY));
}

fn triangle_subset(vertices: [Vec3; 3]) -> Subset {
    Subset {
        vertices: vertices
            .into_iter()
            .map(|position| Vertex {
                position,
                normal: Vec3::Z,
                color: Vec4::ONE,
                ..Vertex::default()
            })
            .collect(),
        triangles: vec![[0, 1, 2]],
        ..Subset::default()
    }
}

#[test]
fn merge_subsets_keeps_opaque_and_alpha_atlas_pages_separate() {
    let mut alpha_subset = triangle_subset([Vec3::ZERO, Vec3::X, Vec3::Y]);
    alpha_subset.has_alpha = true;
    alpha_subset.texture = SubsetTexture::AtlasPage(0);

    let mut opaque_subset = triangle_subset([Vec3::Z, Vec3::new(2.0, 0.0, 0.0), Vec3::new(0.0, 2.0, 0.0)]);
    opaque_subset.texture = SubsetTexture::AtlasPage(0);

    let mut distant_static = DistantStatic {
        subsets: vec![opaque_subset, alpha_subset],
        ..DistantStatic::default()
    };

    distant_static.merge_subsets();

    assert_eq!(distant_static.subsets.len(), 2);
    assert!(distant_static.subsets.iter().any(|subset| subset.has_alpha));
    assert!(distant_static.subsets.iter().any(|subset| subset.is_opaque()));
}

/// A subset on atlas page 0 carrying `count` bit-distinct bounds, all otherwise mergeable.
fn palette_subset(first_bound: u32, count: u32) -> Subset {
    let mut subset = triangle_subset([Vec3::ZERO, Vec3::X, Vec3::Y]);
    subset.texture = SubsetTexture::AtlasPage(0);
    subset.uv_bounds = (first_bound..first_bound + count)
        .map(|seed| bound(seed as f32 / 1024.0))
        .collect();
    subset
}

#[test]
fn merge_subsets_refuses_when_the_bound_union_would_exceed_the_palette_cap() {
    // Post-atlas, both subsets share atlas page 0 and every other identity field, so only the
    // palette cap can keep them apart.
    let mut distant_static = DistantStatic {
        subsets: vec![palette_subset(0, UV_BOUND_PALETTE_CAP), palette_subset(1000, 1)],
        ..DistantStatic::default()
    };
    distant_static.merge_subsets();

    assert_eq!(distant_static.subsets.len(), 2);
    assert_eq!(distant_static.subsets.iter().map(|s| s.triangles.len()).sum::<usize>(), 2);

    // One bound short of the cap, the same pair merges into a single subset with the union.
    let mut distant_static = DistantStatic {
        subsets: vec![palette_subset(0, UV_BOUND_PALETTE_CAP - 1), palette_subset(1000, 1)],
        ..DistantStatic::default()
    };
    distant_static.merge_subsets();

    assert_eq!(distant_static.subsets.len(), 1);
    assert_eq!(distant_static.subsets[0].triangles.len(), 2);
    assert_eq!(distant_static.subsets[0].uv_bounds.len(), UV_BOUND_PALETTE_CAP as usize);
}

#[test]
fn merge_subsets_unions_overlapping_bounds_without_double_counting() {
    // The two subsets share every bound but one, so the union is cap-sized and fits.
    let mut distant_static = DistantStatic {
        subsets: vec![palette_subset(0, UV_BOUND_PALETTE_CAP), palette_subset(0, 4)],
        ..DistantStatic::default()
    };
    distant_static.merge_subsets();

    assert_eq!(distant_static.subsets.len(), 1);
    assert_eq!(distant_static.subsets[0].uv_bounds.len(), UV_BOUND_PALETTE_CAP as usize);
}

#[test]
fn merge_subsets_reuses_earlier_compatible_subset_when_tail_cannot_accept() {
    // Subset 0 has 120 bounds (room for 8 more before cap).
    // Subset 1 has 128 bounds (at cap, cannot accept any new bounds).
    // Subset 2 has 4 bounds (overlapping subset 0's bounds).
    //
    // Under tail-only packing, subset 2 would be tested only against subset 1 (the tail),
    // which cannot accept it, needlessly creating a third subset.
    // With first-fit reuse, subset 2 reuses earlier subset 0 without exceeding the cap.
    let subset_0 = palette_subset(0, 120);
    let subset_1 = palette_subset(200, UV_BOUND_PALETTE_CAP);
    let subset_2 = palette_subset(0, 4);

    let mut distant_static = DistantStatic {
        subsets: vec![subset_0, subset_1, subset_2],
        ..DistantStatic::default()
    };
    distant_static.merge_subsets();

    assert_eq!(
        distant_static.subsets.len(),
        2,
        "subset 2 must reuse earlier subset 0 rather than creating a third subset"
    );
    // Explicit stable ordering expectations:
    // subsets[0] corresponds to bin 0 (containing subset 0 + subset 2 geometry).
    // subsets[1] corresponds to bin 1 (containing subset 1 geometry).
    assert_eq!(distant_static.subsets[0].triangles.len(), 2);
    assert_eq!(distant_static.subsets[0].uv_bounds.len(), 120);
    assert_eq!(distant_static.subsets[1].triangles.len(), 1);
    assert_eq!(distant_static.subsets[1].uv_bounds.len(), UV_BOUND_PALETTE_CAP as usize);
}

#[test]
fn update_bounds_recomputes_subset_and_static_bounds() {
    let mut distant_static = DistantStatic {
        subsets: vec![
            triangle_subset([
                Vec3::new(-4.0, 0.0, 0.0),
                Vec3::new(-2.0, 0.0, 0.0),
                Vec3::new(-4.0, 2.0, 0.0),
            ]),
            Subset::default(),
            triangle_subset([
                Vec3::new(10.0, 1.0, 0.0),
                Vec3::new(12.0, 1.0, 0.0),
                Vec3::new(10.0, 3.0, 0.0),
            ]),
        ],
        ..DistantStatic::default()
    };

    distant_static.update_bounds();

    assert_eq!(distant_static.subsets.len(), 2);
    assert_eq!(distant_static.subsets[0].bounding_box.min, Vec3::new(-4.0, 0.0, 0.0));
    assert_eq!(distant_static.subsets[0].bounding_box.max, Vec3::new(-2.0, 2.0, 0.0));
    assert_eq!(distant_static.subsets[1].bounding_box.min, Vec3::new(10.0, 1.0, 0.0));
    assert_eq!(distant_static.subsets[1].bounding_box.max, Vec3::new(12.0, 3.0, 0.0));
    assert_eq!(distant_static.bounding_box.min, Vec3::new(-4.0, 0.0, 0.0));
    assert_eq!(distant_static.bounding_box.max, Vec3::new(12.0, 3.0, 0.0));
    assert!(distant_static.bounding_sphere.radius > 0.0);
}

#[test]
fn merged_subset_bounds_use_component_spheres() {
    let mut subset = Subset {
        vertices: [
            Vec3::new(-0.25, 0.0, 0.0),
            Vec3::new(0.25, 0.0, 0.0),
            Vec3::new(0.0, 0.25, 0.0),
            Vec3::new(9.75, 0.0, 0.0),
            Vec3::new(10.25, 0.0, 0.0),
            Vec3::new(10.0, 0.25, 0.0),
        ]
        .into_iter()
        .map(|position| Vertex {
            position,
            normal: Vec3::Z,
            color: Vec4::ONE,
            ..Vertex::default()
        })
        .collect(),
        triangles: vec![[0, 1, 2], [3, 4, 5]],
        ..Subset::default()
    };
    subset.push_component(0, 1, Vec3::ZERO, 4.0, StaticType::StaticAuto);
    subset.push_component(1, 1, Vec3::new(10.0, 0.0, 0.0), 2.0, StaticType::StaticAuto);

    let mut scratch = vec![[123.0, 456.0, 789.0]];
    let mut sphere_scratch = BoundingSphereScratch::new();
    subset.update_bounds_with(&mut scratch, &mut sphere_scratch);

    assert!(scratch.is_empty());
    assert_eq!(subset.bounding_box.min, Vec3::new(-0.25, 0.0, 0.0));
    assert_eq!(subset.bounding_box.max, Vec3::new(10.25, 0.25, 0.0));
    assert!((subset.bounding_sphere.center - Vec3::new(4.0, 0.0, 0.0)).length() < 1e-5);
    assert!((subset.bounding_sphere.radius - 8.0).abs() < 1e-5);
}

#[test]
fn simplify_uses_config_target_error() {
    let positions: Vec<Vec3> = vec![
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(2.0, 0.0, 0.0),
        Vec3::new(0.0, 1.0, 0.0),
        Vec3::new(1.0, 1.0, 0.0),
        Vec3::new(2.0, 1.0, 0.0),
    ];
    let vertices: Vec<Vertex> = positions
        .iter()
        .map(|&position| Vertex {
            position,
            ..Vertex::default()
        })
        .collect();
    let triangles: Vec<[u16; 3]> = vec![[0, 1, 3], [1, 4, 3], [1, 2, 4], [2, 5, 4]];

    let make_subset = |triangles: Vec<[u16; 3]>| Subset {
        vertices: vertices.clone(),
        triangles,
        ..Subset::default()
    };

    let mut subset_zero_error = make_subset(triangles.clone());
    subset_zero_error.simplify_with(
        StaticMeshSimplifierConfig {
            target_error: 0.0,
            ..StaticMeshSimplifierConfig::default()
        },
        &mut StaticMeshContext::default(),
    );

    let mut subset_large_error = make_subset(triangles.clone());
    subset_large_error.simplify_with(
        StaticMeshSimplifierConfig {
            target_error: 1.0,
            ..StaticMeshSimplifierConfig::default()
        },
        &mut StaticMeshContext::default(),
    );

    assert_eq!(subset_zero_error.triangles.len(), triangles.len());
    assert!(subset_large_error.triangles.len() <= subset_zero_error.triangles.len());
}

/// Helper: builds a grid subset with `cols` x `rows` vertices and `(cols-1)*(rows-1)*2` triangles.
fn grid_subset(cols: u16, rows: u16) -> Subset {
    let mut vertices = Vec::with_capacity((cols as usize) * (rows as usize));
    for r in 0..rows {
        for c in 0..cols {
            vertices.push(Vertex {
                position: Vec3::new(c as f32, r as f32, 0.0),
                normal: Vec3::Z,
                color: Vec4::ONE,
                uv: Vec2::new(c as f32 / cols as f32, r as f32 / rows as f32),
                ..Vertex::default()
            });
        }
    }
    let mut triangles = Vec::new();
    for r in 0..(rows - 1) {
        for c in 0..(cols - 1) {
            let tl = r * cols + c;
            let tr = tl + 1;
            let bl = tl + cols;
            let br = bl + 1;
            triangles.push([tl, tr, bl]);
            triangles.push([tr, br, bl]);
        }
    }
    Subset {
        vertices,
        triangles,
        ..Subset::default()
    }
}

#[test]
fn simplify_reused_context_matches_fresh_context() {
    let config = StaticMeshSimplifierConfig::default();

    let small = grid_subset(3, 3);
    let large = grid_subset(6, 6);

    let mut fresh_small = small.clone();
    fresh_small.simplify_with(config, &mut StaticMeshContext::default());

    let mut fresh_large = large.clone();
    fresh_large.simplify_with(config, &mut StaticMeshContext::default());

    let mut context = StaticMeshContext::default();

    let mut scratch_large = large.clone();
    scratch_large.simplify_with(config, &mut context);

    let mut scratch_small = small.clone();
    scratch_small.simplify_with(config, &mut context);

    assert_eq!(scratch_small.triangles, fresh_small.triangles);
    assert_eq!(scratch_small.vertices.len(), fresh_small.vertices.len());
    assert_eq!(scratch_large.triangles, fresh_large.triangles);
    assert_eq!(scratch_large.vertices.len(), fresh_large.vertices.len());
}

#[test]
fn optimize_reused_context_matches_fresh_context() {
    let positions = vec![
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(0.0, 1.0, 0.0),
        Vec3::new(0.005, 0.005, 0.0),
        Vec3::new(1.0, 1.0, 0.0),
    ];

    let make = |positions: &[Vec3]| -> Subset {
        let vertices: Vec<Vertex> = positions
            .iter()
            .map(|&position| Vertex {
                position,
                normal: Vec3::Z,
                color: Vec4::ONE,
                ..Vertex::default()
            })
            .collect();
        Subset {
            vertices,
            triangles: vec![[0, 1, 2], [3, 1, 4]],
            ..Subset::default()
        }
    };

    let small = make(&positions);
    let large = grid_subset(16, 16);
    assert!(large.triangles.len() > small.triangles.len());

    let mut fresh_small = small.clone();
    fresh_small.optimize_with(&mut StaticMeshContext::default(), "test.nif", 0);

    let mut context = StaticMeshContext::default();

    let mut scratch_large = large.clone();
    scratch_large.optimize_with(&mut context, "test.nif", 0);

    let mut scratch_small = small.clone();
    scratch_small.optimize_with(&mut context, "test.nif", 0);

    assert_eq!(
        bytemuck::cast_slice::<_, u8>(&scratch_small.vertices),
        bytemuck::cast_slice::<_, u8>(&fresh_small.vertices)
    );
    assert_eq!(scratch_small.triangles, fresh_small.triangles);
}

#[test]
fn absolute_simplification_is_scale_homogeneous() {
    let source = grid_subset(8, 8);
    let mut scaled = source.clone();
    for vertex in &mut scaled.vertices {
        vertex.position *= 8.0;
    }

    let config = StaticMeshSimplifierConfig {
        target_error: 0.01,
        normal_weight: 4.0,
        color_weight: 4.0,
        merge_error_multiplier: 100.0,
    };
    let mut source_result = source;
    let mut scaled_result = scaled;
    let mut source_context = StaticMeshContext::default();
    let mut scaled_context = StaticMeshContext::default();

    let source_target = source_result.simplify_absolute_with(config, 2.0, &mut source_context);
    let scaled_target = scaled_result.simplify_absolute_with(config, 16.0, &mut scaled_context);

    assert_eq!(source_target.requested, scaled_target.requested);
    assert_eq!(source_target.effective, scaled_target.effective);
    assert_eq!(source_result.triangles, scaled_result.triangles);
}

#[test]
fn member_relative_cap_skips_repeated_same_target_simplification() {
    let config = StaticMeshSimplifierConfig {
        target_error: 0.05,
        merge_error_multiplier: 1.0,
        ..StaticMeshSimplifierConfig::default()
    };
    let mut subset = grid_subset(10, 10);
    subset.simplify_with(config, &mut StaticMeshContext::default());
    let triangles = subset.triangles.clone();
    let mut context = StaticMeshContext::default();

    let target = subset.simplify_absolute_with(config, 10_000.0, &mut context);

    assert!(target.capped);
    assert!(!target.should_simplify);
    assert_eq!(target.effective, config.target_error);
    assert_eq!(subset.triangles, triangles);
}

#[test]
fn repeated_same_target_simplification_can_remove_additional_triangles() {
    let mut source = grid_subset(24, 24);
    for vertex in &mut source.vertices {
        let x = vertex.position.x as u32;
        let y = vertex.position.y as u32;
        vertex.position.z = ((x * 17 + y * 31 + x * y * 7) % 19) as f32 * 0.125;
    }

    let mut counts = Vec::new();
    for target_error in [0.005, 0.01, 0.02, 0.05, 0.1, 0.2] {
        let config = StaticMeshSimplifierConfig {
            target_error,
            ..StaticMeshSimplifierConfig::default()
        };
        let mut subset = source.clone();
        subset.simplify_with(config, &mut StaticMeshContext::default());
        let after_first = subset.triangles.len();
        subset.simplify_with(config, &mut StaticMeshContext::default());
        let after_second = subset.triangles.len();
        counts.push((target_error, after_first, after_second));
        if after_second < after_first {
            return;
        }
    }

    panic!("expected at least one repeated-pass reduction, observed {counts:?}");
}

#[test]
fn zero_absolute_error_never_runs_a_merge_simplification_pass() {
    let config = StaticMeshSimplifierConfig {
        target_error: 0.0,
        merge_error_multiplier: 8.0,
        ..StaticMeshSimplifierConfig::default()
    };
    let mut subset = grid_subset(8, 8);
    let triangles = subset.triangles.clone();
    let mut context = StaticMeshContext::default();

    let target = subset.simplify_absolute_with(config, 0.0, &mut context);

    assert_eq!(target.requested, 0.0);
    assert_eq!(target.effective, 0.0);
    assert!(!target.should_simplify);
    assert_eq!(subset.triangles, triangles);
}

#[test]
fn absolute_simplification_keeps_attribute_weights_at_large_relative_targets() {
    let mut source = grid_subset(10, 10);
    for (index, vertex) in source.vertices.iter_mut().enumerate() {
        let high = (index / 10 + index % 10).is_multiple_of(2);
        vertex.normal = if high { Vec3::Z } else { Vec3::X };
        vertex.color = if high { Vec4::ONE } else { Vec4::new(0.0, 0.0, 0.0, 1.0) };
    }

    let strong_config = StaticMeshSimplifierConfig {
        target_error: 0.01,
        normal_weight: 32.0,
        color_weight: 32.0,
        merge_error_multiplier: 100.0,
    };
    let downscaled_config = StaticMeshSimplifierConfig {
        normal_weight: strong_config.normal_weight * strong_config.target_error,
        color_weight: strong_config.color_weight * strong_config.target_error,
        ..strong_config
    };
    let mut strong = source.clone();
    let mut downscaled = source;
    let mut strong_context = StaticMeshContext::default();
    let mut downscaled_context = StaticMeshContext::default();

    strong.simplify_absolute_with(strong_config, 9.0, &mut strong_context);
    downscaled.simplify_with_error(downscaled_config, 1.0, &mut downscaled_context);

    assert_ne!(
        strong.triangles, downscaled.triangles,
        "absolute simplification must not reproduce the old down-scaled attribute metric"
    );
}

#[test]
fn merge_lod_from_borrowed_source_matches_cloning_and_leaves_source_intact() {
    // Representative opaque mesh that actually simplifies under a large absolute budget, plus an
    // alpha companion (optimize-only, no decimation) and a component-bearing optimize-only mesh
    // so the shared optimize core is covered without a combinatorial suite.
    let mut opaque = grid_subset(12, 12);
    for vertex in &mut opaque.vertices {
        let x = vertex.position.x as u32;
        let y = vertex.position.y as u32;
        vertex.position.z = ((x * 13 + y * 29 + x * y * 5) % 17) as f32 * 0.1;
    }
    opaque.has_uv_controller = true;
    opaque.emissive = 0.25;

    let mut alpha = grid_subset(4, 4);
    alpha.has_alpha = true;
    alpha.has_uv_controller = false;
    alpha.emissive = 0.0;

    // Components must tile the triangle buffer at optimize time; keep absolute error at zero so
    // this case exercises partition-aware cache/overdraw without a prior simplify pass.
    let mut partitioned = grid_subset(5, 5);
    let half = (partitioned.triangles.len() / 2) as u32;
    partitioned.components = vec![
        MergedComponent {
            first_triangle: 0,
            triangle_count: half,
            center: Vec3::new(1.0, 2.0, 3.0),
            radius: 4.0,
            classification: StaticType::StaticAuto,
        },
        MergedComponent {
            first_triangle: half,
            triangle_count: partitioned.triangles.len() as u32 - half,
            center: Vec3::new(4.0, 5.0, 6.0),
            radius: 7.0,
            classification: StaticType::StaticBuilding,
        },
    ];

    let config = StaticMeshSimplifierConfig {
        target_error: 0.01,
        normal_weight: 4.0,
        color_weight: 4.0,
        merge_error_multiplier: 100.0,
    };
    let simplify_error = 4.0;
    let optimize_only_error = 0.0;

    fn clone_then_mutate(
        source: &Subset,
        config: StaticMeshSimplifierConfig,
        absolute_error: f32,
        workspace: &mut StaticMeshContext,
        mesh_path: &str,
        subset_index: usize,
    ) -> Subset {
        let mut subset = source.clone();
        subset.simplify_absolute_with(config, absolute_error, workspace);
        subset.optimize_with(workspace, mesh_path, subset_index);
        subset
    }

    fn assert_subset_geometry_eq(label: &str, expected: &Subset, actual: &Subset) {
        assert_eq!(actual.triangles, expected.triangles, "{label}: triangles");
        assert_eq!(
            bytemuck::cast_slice::<Vertex, u8>(&actual.vertices),
            bytemuck::cast_slice::<Vertex, u8>(&expected.vertices),
            "{label}: vertex bytes"
        );
        assert_eq!(actual.has_alpha, expected.has_alpha, "{label}: has_alpha");
        assert_eq!(
            actual.has_uv_controller, expected.has_uv_controller,
            "{label}: has_uv_controller"
        );
        assert_eq!(actual.emissive, expected.emissive, "{label}: emissive");
        assert_eq!(actual.texture, expected.texture, "{label}: texture");
        assert_eq!(actual.components, expected.components, "{label}: components");
    }

    let mut clone_context = StaticMeshContext::default();
    let mut borrow_context = StaticMeshContext::default();

    let clone_opaque = clone_then_mutate(&opaque, config, simplify_error, &mut clone_context, "lod-test.nif", 0);
    let borrow_opaque =
        Subset::build_merge_lod_from(&opaque, config, simplify_error, &mut borrow_context, "lod-test.nif", 0);

    let clone_alpha = clone_then_mutate(&alpha, config, simplify_error, &mut clone_context, "lod-test.nif", 1);
    let borrow_alpha = Subset::build_merge_lod_from(&alpha, config, simplify_error, &mut borrow_context, "lod-test.nif", 1);

    let clone_partitioned = clone_then_mutate(
        &partitioned,
        config,
        optimize_only_error,
        &mut clone_context,
        "lod-test.nif",
        2,
    );
    let borrow_partitioned = Subset::build_merge_lod_from(
        &partitioned,
        config,
        optimize_only_error,
        &mut borrow_context,
        "lod-test.nif",
        2,
    );

    assert!(
        clone_opaque.triangles.len() < opaque.triangles.len(),
        "fixture must exercise simplification ({} -> {})",
        opaque.triangles.len(),
        clone_opaque.triangles.len()
    );
    assert_subset_geometry_eq("opaque", &clone_opaque, &borrow_opaque);
    assert_subset_geometry_eq("alpha", &clone_alpha, &borrow_alpha);
    assert_subset_geometry_eq("partitioned", &clone_partitioned, &borrow_partitioned);

    assert_eq!(opaque.triangles.len(), 11 * 11 * 2);
    assert_eq!(alpha.triangles.len(), 3 * 3 * 2);
    assert_eq!(partitioned.triangles.len(), 4 * 4 * 2);
}

#[test]
fn lock_border_preserves_open_boundaries_but_not_closed_mesh_vertices() {
    let mut open = grid_subset(6, 6);
    let open_boundary: Vec<u16> = (0..36)
        .filter(|index| {
            let row = index / 6;
            let col = index % 6;
            row == 0 || row == 5 || col == 0 || col == 5
        })
        .collect();
    open.simplify_with(
        StaticMeshSimplifierConfig {
            target_error: 1.0,
            ..StaticMeshSimplifierConfig::default()
        },
        &mut StaticMeshContext::default(),
    );
    let open_indices = open.triangles.as_flattened();
    assert!(open_boundary.iter().all(|index| open_indices.contains(index)));

    let mut closed = Subset {
        vertices: [
            Vec3::new(-1.0, -1.0, -1.0),
            Vec3::new(1.0, -1.0, -1.0),
            Vec3::new(1.0, 1.0, -1.0),
            Vec3::new(-1.0, 1.0, -1.0),
            Vec3::new(-1.0, -1.0, 1.0),
            Vec3::new(1.0, -1.0, 1.0),
            Vec3::new(1.0, 1.0, 1.0),
            Vec3::new(-1.0, 1.0, 1.0),
        ]
        .into_iter()
        .map(|position| Vertex {
            position,
            normal: position.normalize(),
            ..Vertex::default()
        })
        .collect(),
        triangles: vec![
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [1, 2, 6],
            [1, 6, 5],
            [2, 3, 7],
            [2, 7, 6],
            [3, 0, 4],
            [3, 4, 7],
        ],
        ..Subset::default()
    };
    let original_closed_triangles = closed.triangles.len();
    closed.simplify_with(
        StaticMeshSimplifierConfig {
            target_error: 1.0,
            ..StaticMeshSimplifierConfig::default()
        },
        &mut StaticMeshContext::default(),
    );

    assert!(closed.triangles.len() < original_closed_triangles);
}

#[test]
fn coincident_disconnected_components_are_not_pruned() {
    let base = grid_subset(6, 6);
    let split = base.vertices.len() as u16;
    let mut coincident = base.clone();
    coincident.vertices.extend(base.vertices);
    coincident
        .triangles
        .extend(base.triangles.into_iter().map(|triangle| triangle.map(|index| index + split)));

    coincident.simplify_with(
        StaticMeshSimplifierConfig {
            target_error: 1.0,
            ..StaticMeshSimplifierConfig::default()
        },
        &mut StaticMeshContext::default(),
    );
    let indices = coincident.triangles.as_flattened();

    assert!(indices.iter().any(|index| *index < split));
    assert!(indices.iter().any(|index| *index >= split));
}

fn triangle_area(positions: &[Vec3], tri: [u16; 3]) -> f32 {
    let p0 = positions[tri[0] as usize];
    let p1 = positions[tri[1] as usize];
    let p2 = positions[tri[2] as usize];
    0.5 * (p1 - p0).cross(p2 - p0).length()
}

fn all_triangles_non_degenerate(subset: &Subset) -> bool {
    let positions: Vec<Vec3> = subset.vertices.iter().map(|v| v.position).collect();
    subset.triangles.iter().all(|&tri| triangle_area(&positions, tri) > 1e-6)
}

fn all_indices_in_range(subset: &Subset) -> bool {
    subset
        .triangles
        .as_flattened()
        .iter()
        .all(|&index| usize::from(index) < subset.vertices.len())
}

fn triangle_position_multiset(subset: &Subset) -> Vec<[[u32; 3]; 3]> {
    let mut positions: Vec<_> = subset
        .triangles
        .iter()
        .map(|triangle| {
            triangle.map(|index| {
                let position = subset.vertices[index as usize].position;
                [position.x.to_bits(), position.y.to_bits(), position.z.to_bits()]
            })
        })
        .collect();
    positions.sort_unstable();
    positions
}

fn duplicate_origin_subset_with(mut mutate_duplicate: impl FnMut(&mut Vertex)) -> Subset {
    let mut duplicate = Vertex {
        position: Vec3::ZERO,
        normal: Vec3::Z,
        uv: Vec2::ZERO,
        color: Vec4::new(0.0, 0.0, 0.0, 1.0),
        ..Vertex::default()
    };
    mutate_duplicate(&mut duplicate);

    Subset {
        vertices: vec![
            Vertex {
                position: Vec3::ZERO,
                normal: Vec3::Z,
                uv: Vec2::ZERO,
                color: Vec4::new(0.0, 0.0, 0.0, 1.0),
                ..Vertex::default()
            },
            Vertex {
                position: Vec3::X,
                normal: Vec3::Z,
                uv: Vec2::X,
                color: Vec4::new(0.0, 0.0, 0.0, 1.0),
                ..Vertex::default()
            },
            Vertex {
                position: Vec3::Y,
                normal: Vec3::Z,
                uv: Vec2::Y,
                color: Vec4::new(0.0, 0.0, 0.0, 1.0),
                ..Vertex::default()
            },
            duplicate,
        ],
        triangles: vec![[0, 1, 2], [3, 1, 2]],
        ..Subset::default()
    }
}

/// Builds a two-sided foliage card with identical attributes and opposite winding.
fn flipped_foliage_card(has_alpha: bool) -> Subset {
    let corners = [
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(1.0, 1.0, 0.0),
        Vec3::new(0.0, 1.0, 0.0),
    ];
    let uvs = [
        Vec2::new(0.0, 0.0),
        Vec2::new(1.0, 0.0),
        Vec2::new(1.0, 1.0),
        Vec2::new(0.0, 1.0),
    ];

    let make_vertex = |i: usize| Vertex {
        position: corners[i],
        normal: Vec3::Z, // authored normal; identical for both sides
        uv: uvs[i],
        color: Vec4::ONE,
        ..Vertex::default()
    };

    let front: Vec<Vertex> = (0..4).map(make_vertex).collect();
    let back: Vec<Vertex> = (0..4).map(make_vertex).collect();

    let mut vertices = front;
    vertices.extend(back);

    let triangles = vec![[0, 1, 2], [0, 2, 3], [4, 6, 5], [4, 7, 6]];

    Subset {
        vertices,
        triangles,
        has_alpha,
        ..Subset::default()
    }
}

#[test]
fn alpha_flipped_foliage_card_is_preserved_by_optimize() {
    let mut subset = flipped_foliage_card(true);
    let original_tri_count = subset.triangles.len();
    subset.optimize_with(&mut StaticMeshContext::default(), "test.nif", 0);

    assert_eq!(
        subset.vertices.len(),
        8,
        "alpha foliage-card front and back vertices must stay distinct"
    );
    assert_eq!(
        subset.triangles.len(),
        original_tri_count,
        "alpha foliage-card triangle count must not collapse"
    );
    assert!(
        all_triangles_non_degenerate(&subset),
        "no alpha foliage-card triangle should be degenerate after optimize"
    );
}

#[test]
fn alpha_foliage_card_skips_relative_simplification() {
    let mut subset = flipped_foliage_card(true);
    let original_triangles = subset.triangles.clone();
    let original_vertex_shape: Vec<_> = subset
        .vertices
        .iter()
        .map(|vertex| (vertex.position, vertex.normal, vertex.uv, vertex.color))
        .collect();

    subset.simplify_with(
        StaticMeshSimplifierConfig {
            target_error: 1.0,
            merge_error_multiplier: 100.0,
            ..StaticMeshSimplifierConfig::default()
        },
        &mut StaticMeshContext::default(),
    );

    assert_eq!(subset.triangles, original_triangles);
    assert_eq!(
        subset
            .vertices
            .iter()
            .map(|vertex| (vertex.position, vertex.normal, vertex.uv, vertex.color))
            .collect_vec(),
        original_vertex_shape
    );
}

#[test]
fn alpha_foliage_card_skips_absolute_simplification() {
    let mut subset = flipped_foliage_card(true);
    let original_triangles = subset.triangles.clone();
    let mut context = StaticMeshContext::default();

    let target = subset.simplify_absolute_with(
        StaticMeshSimplifierConfig {
            target_error: 0.01,
            merge_error_multiplier: 100.0,
            ..StaticMeshSimplifierConfig::default()
        },
        10_000.0,
        &mut context,
    );

    assert_eq!(target.requested, 0.0);
    assert_eq!(target.effective, 0.0);
    assert!(!target.capped);
    assert!(!target.should_simplify);
    assert_eq!(subset.triangles, original_triangles);
}

#[test]
fn alpha_same_orientation_duplicates_still_weld() {
    let vertices: Vec<Vertex> = vec![
        Vertex {
            position: Vec3::new(0.0, 0.0, 0.0),
            normal: Vec3::Z,
            color: Vec4::ONE,
            ..Vertex::default()
        },
        Vertex {
            position: Vec3::new(1.0, 0.0, 0.0),
            normal: Vec3::Z,
            color: Vec4::ONE,
            ..Vertex::default()
        },
        Vertex {
            position: Vec3::new(0.0, 1.0, 0.0),
            normal: Vec3::Z,
            color: Vec4::ONE,
            ..Vertex::default()
        },
        Vertex {
            position: Vec3::new(0.0, 0.0, 0.0),
            normal: Vec3::Z,
            color: Vec4::ONE,
            ..Vertex::default()
        },
    ];
    let triangles = vec![[0, 1, 2], [3, 1, 2]];

    let mut subset = Subset {
        vertices,
        triangles,
        has_alpha: true,
        ..Subset::default()
    };

    subset.optimize_with(&mut StaticMeshContext::default(), "test.nif", 0);

    assert!(
        subset.vertices.len() < 4,
        "same-orientation alpha duplicates should still weld"
    );
    assert!(
        all_triangles_non_degenerate(&subset),
        "no triangle should be degenerate after welding same-orientation duplicates"
    );
}

#[test]
fn opaque_flipped_foliage_card_still_welds() {
    // The orientation guard is gated to alpha subsets; opaque subsets keep the original
    // attribute-only equivalence, so flipped card vertices still weld. This pins the
    // alpha-only gating so the guard is not accidentally broadened.
    let mut subset = flipped_foliage_card(false);
    subset.optimize_with(&mut StaticMeshContext::default(), "test.nif", 0);

    assert_eq!(
        subset.vertices.len(),
        4,
        "opaque flipped card should weld front/back to 4 vertices (guard is alpha-only)"
    );
}

#[test]
fn exact_duplicate_vertices_collapse_to_one_unique_vertex() {
    let mut subset = duplicate_origin_subset_with(|_| {});
    subset.optimize_with(&mut StaticMeshContext::default(), "test.nif", 0);

    assert_eq!(subset.vertices.len(), 3);
    assert!(all_indices_in_range(&subset));
}

#[test]
fn distinct_uvs_outside_weld_cell_do_not_weld() {
    let mut subset = duplicate_origin_subset_with(|duplicate| {
        duplicate.uv = Vec2::new(0.006, 0.0);
    });
    subset.optimize_with(&mut StaticMeshContext::default(), "test.nif", 0);

    assert_eq!(subset.vertices.len(), 4);
    assert!(all_indices_in_range(&subset));
}

#[test]
fn distinct_normals_outside_weld_cell_do_not_weld() {
    let mut subset = duplicate_origin_subset_with(|duplicate| {
        duplicate.normal = Vec3::new(0.06, 0.0, 1.0);
    });
    subset.optimize_with(&mut StaticMeshContext::default(), "test.nif", 0);

    assert_eq!(subset.vertices.len(), 4);
    assert!(all_indices_in_range(&subset));
}

#[test]
fn distinct_colors_outside_weld_cell_do_not_weld() {
    let mut subset = duplicate_origin_subset_with(|duplicate| {
        duplicate.color = Vec4::new(0.006, 0.0, 0.0, 1.0);
    });
    subset.optimize_with(&mut StaticMeshContext::default(), "test.nif", 0);

    assert_eq!(subset.vertices.len(), 4);
    assert!(all_indices_in_range(&subset));
}

#[test]
fn alpha_vertices_with_ambiguous_orientation_do_not_weld() {
    let vertices = vec![
        Vertex {
            position: Vec3::ZERO,
            normal: Vec3::Z,
            color: Vec4::ONE,
            ..Vertex::default()
        },
        Vertex {
            position: Vec3::X,
            normal: Vec3::Z,
            color: Vec4::ONE,
            ..Vertex::default()
        },
        Vertex {
            position: Vec3::Y,
            normal: Vec3::Z,
            color: Vec4::ONE,
            ..Vertex::default()
        },
        Vertex {
            position: Vec3::ZERO,
            normal: Vec3::Z,
            color: Vec4::ONE,
            ..Vertex::default()
        },
    ];
    let mut subset = Subset {
        vertices,
        triangles: vec![[0, 1, 2], [0, 2, 1], [3, 1, 2], [3, 2, 1]],
        has_alpha: true,
        ..Subset::default()
    };

    subset.optimize_with(&mut StaticMeshContext::default(), "test.nif", 0);

    assert_eq!(subset.vertices.len(), 4);
    assert!(all_indices_in_range(&subset));
}

#[test]
fn nan_position_vertex_does_not_weld_with_origin() {
    let mut subset = duplicate_origin_subset_with(|duplicate| {
        duplicate.position = Vec3::new(f32::NAN, 0.0, 0.0);
    });
    subset.optimize_with(&mut StaticMeshContext::default(), "test.nif", 0);

    assert_eq!(subset.vertices.len(), 4);
    assert!(all_indices_in_range(&subset));
}

#[test]
fn non_finite_attributes_make_vertex_non_weldable() {
    let mut subset = duplicate_origin_subset_with(|duplicate| {
        duplicate.uv = Vec2::new(f32::NAN, 0.0);
    });
    subset.optimize_with(&mut StaticMeshContext::default(), "test.nif", 0);

    assert_eq!(subset.vertices.len(), 4);
    assert!(all_indices_in_range(&subset));
}

#[test]
fn unreferenced_vertices_are_dropped_by_optimize() {
    let mut subset = duplicate_origin_subset_with(|_| {});
    subset.vertices.push(Vertex {
        position: Vec3::new(10.0, 10.0, 10.0),
        normal: Vec3::Z,
        color: Vec4::ONE,
        ..Vertex::default()
    });

    subset.optimize_with(&mut StaticMeshContext::default(), "test.nif", 0);

    assert_eq!(subset.vertices.len(), 3);
    assert!(all_indices_in_range(&subset));
}

#[test]
fn optimize_preserves_triangle_position_multiset_and_index_range() {
    let mut subset = grid_subset(4, 4);
    let before = triangle_position_multiset(&subset);

    subset.optimize_with(&mut StaticMeshContext::default(), "test.nif", 0);

    assert_eq!(triangle_position_multiset(&subset), before);
    assert!(all_indices_in_range(&subset));
}

#[test]
fn coincident_position_distinct_uv_stress_subset_optimizes() {
    let vertex_count = 2048u16;
    let vertices: Vec<_> = (0..vertex_count)
        .map(|i| Vertex {
            position: Vec3::ZERO,
            normal: Vec3::Z,
            uv: Vec2::new(i as f32 * 0.01, 0.0),
            color: Vec4::ONE,
            ..Vertex::default()
        })
        .collect();
    let triangles: Vec<_> = (0..vertex_count - 2).map(|i| [i, i + 1, i + 2]).collect();
    let mut subset = Subset {
        vertices,
        triangles,
        ..Subset::default()
    };

    subset.optimize_with(&mut StaticMeshContext::default(), "test.nif", 0);

    assert_eq!(subset.vertices.len(), vertex_count as usize);
    assert!(all_indices_in_range(&subset));
}
