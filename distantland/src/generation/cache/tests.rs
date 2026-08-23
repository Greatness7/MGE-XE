use std::path::PathBuf;

use super::*;
use crate::generation::{GenerationJob, GenerationSettings};

#[test]
fn request_fingerprint_ignores_output_root() {
    let other_root_job = test_generation_job(PathBuf::from(r"C:\Morrowind\Data Files\alternate_output_root"));
    let final_job = test_generation_job(PathBuf::from(r"C:\Morrowind\Data Files"));

    assert_eq!(
        fingerprint_generation_request(&other_root_job).unwrap(),
        fingerprint_generation_request(&final_job).unwrap()
    );
}

#[test]
fn request_fingerprint_tracks_texture_dedupe_mode() {
    let baseline = test_generation_job(PathBuf::from(r"C:\Morrowind\Data Files"));
    let mut changed = baseline.clone();
    changed.settings.texture_dedupe_mode = crate::TextureDedupeMode::Off;

    assert_ne!(
        fingerprint_generation_request(&baseline).unwrap(),
        fingerprint_generation_request(&changed).unwrap()
    );
}

#[test]
fn request_fingerprint_tracks_static_mesh_merge_error_multiplier() {
    let baseline = test_generation_job(PathBuf::from(r"C:\Morrowind\Data Files"));
    let mut changed = baseline.clone();
    changed.settings.static_mesh_merge_error_multiplier = 2.0;

    assert_ne!(
        fingerprint_generation_request(&baseline).unwrap(),
        fingerprint_generation_request(&changed).unwrap()
    );
}

#[test]
fn request_fingerprint_tracks_deep_water_static_cull_depth() {
    let baseline = test_generation_job(PathBuf::from(r"C:\Morrowind\Data Files"));
    let mut changed = baseline.clone();
    changed.settings.deep_water_static_cull_depth = 256.0;

    assert_ne!(
        fingerprint_generation_request(&baseline).unwrap(),
        fingerprint_generation_request(&changed).unwrap()
    );
}

#[test]
fn request_fingerprint_ignores_force_rebuild() {
    let baseline = test_generation_job(PathBuf::from(r"C:\Morrowind\Data Files"));
    let mut forced = baseline.clone();
    forced.settings.force_rebuild = true;

    assert_eq!(
        fingerprint_generation_request(&baseline).unwrap(),
        fingerprint_generation_request(&forced).unwrap()
    );
}

#[test]
fn request_fingerprint_tracks_grass_plugin_set() {
    let baseline = test_generation_job(PathBuf::from(r"C:\Morrowind\Data Files"));
    let mut explicit_empty = baseline.clone();
    explicit_empty.grass_plugins = Some(vec![]);
    assert_eq!(
        fingerprint_generation_request(&baseline).unwrap(),
        fingerprint_generation_request(&explicit_empty).unwrap()
    );
    let mut changed = baseline.clone();
    changed.grass_plugins = Some(vec![PathBuf::from("Groundcover.esp")]);

    assert_ne!(
        fingerprint_generation_request(&baseline).unwrap(),
        fingerprint_generation_request(&changed).unwrap()
    );

    changed.grass_plugins = Some(vec![PathBuf::from("B.esp"), PathBuf::from("A.esp")]);
    let mut reordered = changed.clone();
    reordered.grass_plugins.as_mut().unwrap().reverse();
    assert_eq!(
        fingerprint_generation_request(&changed).unwrap(),
        fingerprint_generation_request(&reordered).unwrap()
    );
}

fn test_generation_job(output_root: PathBuf) -> GenerationJob {
    GenerationJob {
        morrowind_ini: Some(PathBuf::from(r"C:\Morrowind\Morrowind.ini")),
        data_dirs: Some(vec![PathBuf::from(r"C:\Morrowind\Data Files")]),
        plugins: Some(vec![PathBuf::from("Morrowind.esm"), PathBuf::from("Tribunal.esm")]),
        grass_plugins: None,
        output_root: Some(output_root),
        auto_sync_plugins: false,
        settings: GenerationSettings {
            min_static_size: 256.0,
            ..GenerationSettings::default()
        },
    }
}

#[test]
fn static_meshes_fingerprint_tracks_horizon_footprint() {
    use crate::mge_xe::distant_statics::{HorizonFootprint, PackedDistantStatic, PackedSubset};

    let subset = PackedSubset {
        texture: Box::<str>::from("atlas.dds"),
        ..PackedSubset::default()
    };
    let first: crate::PackedDistantStatics = [(
        "a.nif".to_string(),
        PackedDistantStatic {
            subsets: vec![subset.clone()],
            ..PackedDistantStatic::default()
        },
    )]
    .into_iter()
    .collect();
    let mut second = first.clone();
    second.get_mut("a.nif").unwrap().subsets[0].horizon_footprint = HorizonFootprint {
        max_z: 3.0,
        vertex_count: 3,
        footprint_xy: [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [0.0; 2], [0.0; 2], [0.0; 2]],
        ..HorizonFootprint::default()
    };

    assert_ne!(
        fingerprint_static_meshes_inputs(&first),
        fingerprint_static_meshes_inputs(&second)
    );
}

#[test]
fn static_meshes_fingerprint_tracks_components() {
    use crate::mge_xe::distant_statics::{ComponentRecord, PackedDistantStatic, PackedSubset};

    let subset = PackedSubset {
        components: vec![ComponentRecord {
            radius: 1.0,
            ..ComponentRecord::default()
        }],
        texture: Box::<str>::from("atlas.dds"),
        ..PackedSubset::default()
    };
    let first: crate::PackedDistantStatics = [(
        "a.nif".to_string(),
        PackedDistantStatic {
            subsets: vec![subset],
            ..PackedDistantStatic::default()
        },
    )]
    .into_iter()
    .collect();
    let mut second = first.clone();
    second.get_mut("a.nif").unwrap().subsets[0].components[0].radius = 2.0;

    assert_ne!(
        fingerprint_static_meshes_inputs(&first),
        fingerprint_static_meshes_inputs(&second)
    );
}

#[test]
fn static_shard_assignment_matches_pinned_vectors() {
    let cases = [
        ("meshes\\f\\flora_tree_01.nif", 54),
        ("CELL (0, 0) GROUP (0)", 43),
        ("CELL (-12, 34) GROUP (7)", 89),
    ];

    for (key, expected) in cases {
        assert_eq!(static_mesh_shard_id(key), expected, "assignment changed for {key}");
    }
}

#[test]
fn the_cache_fingerprint_ignores_the_sync_policy_flag() {
    let job = GenerationJob {
        plugins: Some(vec![PathBuf::from("Morrowind.esm")]),
        auto_sync_plugins: true,
        ..GenerationJob::default()
    };
    let manual = GenerationJob {
        auto_sync_plugins: false,
        ..job.clone()
    };

    // The flag chose the list; it is not part of it. Hashing it would rebuild
    // everything the first time a user ticks the checkbox.
    assert_eq!(
        fingerprint_generation_request(&job).unwrap(),
        fingerprint_generation_request(&manual).unwrap()
    );
}
