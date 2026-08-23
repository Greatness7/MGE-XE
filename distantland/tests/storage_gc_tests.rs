//! Pruning and cache-absent regeneration coverage.

use std::fs;
use std::time::Duration;

use distantland::output_index::{OutputValidation, open_output_snapshot};
use distantland::{MGE_DL_VERSION, NullProgressReporter, OutputPaths, STATIC_MESH_SHARD_COUNT, generate};
use distantland_test_support::{BASELINE_WORLD_V1, build_hermetic_fixture, hermetic_generation_job};
use tempfile::TempDir;

#[test]
fn superseded_version_tree_is_rebuilt_and_its_owned_artifacts_pruned() {
    let root = TempDir::new().unwrap();
    let inputs = build_hermetic_fixture(BASELINE_WORLD_V1, &root.path().join("inputs")).unwrap();
    let output_root = root.path().join("output");
    let job = hermetic_generation_job(&inputs, &output_root);
    let paths = OutputPaths::new(&output_root);

    // Plant superseded version-13 evidence before the first v16 publication.
    fs::create_dir_all(&paths.distantland_dir).unwrap();
    fs::create_dir_all(&paths.statics_dir).unwrap();
    fs::write(&paths.version_path, [13]).unwrap();
    fs::write(paths.distantland_dir.join("commit_journal.bin"), b"journal").unwrap();
    fs::write(paths.distantland_dir.join("generation_index_a.bin"), b"index-a").unwrap();
    fs::write(paths.distantland_dir.join("generation_index_b.bin"), b"index-b").unwrap();
    fs::write(paths.statics_dir.join("static_meshes.e000001"), b"epoch-static").unwrap();
    fs::write(paths.statics_dir.join("static_meshes_00"), b"version-15-static").unwrap();
    fs::write(paths.statics_dir.join("static_meshes_999"), b"stray-static").unwrap();
    fs::write(paths.distantland_dir.join("terrain.e000001.bin"), b"epoch-terrain").unwrap();
    fs::write(paths.distantland_dir.join("leftover_noise.bin"), b"keep").unwrap();
    fs::write(paths.atlas_texture_dir.join("user_keep_me.txt"), b"keep").unwrap_or_else(|_| {
        fs::create_dir_all(&paths.atlas_texture_dir).unwrap();
        fs::write(paths.atlas_texture_dir.join("user_keep_me.txt"), b"keep").unwrap();
    });
    fs::write(paths.atlas_texture_dir.join("_mge_xe_atlas_01.dds"), b"stray-atlas").unwrap();

    let report = generate(&job, &mut NullProgressReporter).unwrap();

    // The older tree is rebuilt outright, never adopted: no shard is carried forward.
    assert_eq!(report.report.cache.static_shards.carried_shard_count, 0);
    assert_eq!(report.report.cache.static_shards.written_shard_count, STATIC_MESH_SHARD_COUNT);
    assert!(!paths.distantland_dir.join("commit_journal.bin").exists());
    assert!(!paths.distantland_dir.join("generation_index_a.bin").exists());
    assert!(!paths.distantland_dir.join("generation_index_b.bin").exists());
    assert!(!paths.statics_dir.join("static_meshes.e000001").exists());
    assert!(!paths.statics_dir.join("static_meshes_00").exists());
    assert!(!paths.distantland_dir.join("terrain.e000001.bin").exists());
    assert!(paths.distantland_dir.join("leftover_noise.bin").is_file());
    assert!(paths.statics_dir.join("static_meshes_999").is_file());
    assert!(paths.atlas_texture_dir.join("user_keep_me.txt").is_file());
    assert!(paths.atlas_texture_dir.join("_mge_xe_atlas_01.dds").is_file());
    assert_eq!(fs::read(&paths.version_path).unwrap(), [MGE_DL_VERSION]);
    assert!(paths.generation_state_path.is_file());
    open_output_snapshot(&output_root, Duration::from_secs(5), OutputValidation::Routine).unwrap();
}

#[test]
fn unlisted_stale_files_do_not_affect_serving() {
    let root = TempDir::new().unwrap();
    let inputs = build_hermetic_fixture(BASELINE_WORLD_V1, &root.path().join("inputs")).unwrap();
    let output_root = root.path().join("output");
    let job = hermetic_generation_job(&inputs, &output_root);
    generate(&job, &mut NullProgressReporter).unwrap();

    let paths = OutputPaths::new(&output_root);
    fs::write(paths.distantland_dir.join("leftover_noise.bin"), b"noise").unwrap();
    let snapshot = open_output_snapshot(&output_root, Duration::from_secs(5), OutputValidation::Full).unwrap();
    assert_eq!(snapshot.version(), MGE_DL_VERSION);
    assert!(snapshot.paths().static_mesh_shard_paths.iter().all(|path| path.is_file()));
}
