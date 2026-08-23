//! Interruption and cache-absent recovery coverage.

use std::fs;
use std::process::Command;
use std::time::{Duration, SystemTime};

use distantland::mge_xe::distant_terrain::load_terrain_file;
use distantland::output_index::{OutputValidation, open_output_snapshot};
use distantland::{
    NullProgressReporter, OutputPaths, STATIC_MESH_SHARD_COUNT, StaticReuseMode, StaticTextureSizingMode,
    TerrainMeshWorkKey, TerrainReuseMode, generate,
};
use distantland_test_support::{
    BASELINE_WORLD_V1, BASELINE_WORLD_V1_ATLAS, BASELINE_WORLD_V1_HALOSPAN, BASELINE_WORLD_V1_THRESHOLD,
    build_hermetic_fixture, edit_fixture_land_height, edit_fixture_nif_geometry, hermetic_generation_job,
    write_generation_job_file,
};
use itertools::Itertools;
use tempfile::TempDir;

/// World-space size of one exterior cell.
const CELL_SIZE: f32 = 8192.0;

fn mtime(path: &std::path::Path) -> SystemTime {
    fs::metadata(path).unwrap().modified().unwrap()
}

#[test]
fn true_noop_preserves_state_and_artifact_mtimes() {
    let root = TempDir::new().unwrap();
    let inputs = build_hermetic_fixture(BASELINE_WORLD_V1, &root.path().join("inputs")).unwrap();
    let output_root = root.path().join("output");
    let job = hermetic_generation_job(&inputs, &output_root);
    let initial = generate(&job, &mut NullProgressReporter).unwrap();
    assert_eq!(initial.report.cache.static_shards.shard_count, STATIC_MESH_SHARD_COUNT);
    assert_eq!(initial.report.cache.static_shards.carried_shard_count, 0);
    assert_eq!(
        initial.report.cache.static_shards.written_shard_count,
        STATIC_MESH_SHARD_COUNT
    );
    assert_eq!(
        initial.report.cache.static_shards.written_shard_ids,
        (0..STATIC_MESH_SHARD_COUNT).map(|id| id as u8).collect_vec()
    );

    let paths = OutputPaths::new(&output_root);
    let before_state = fs::read(&paths.generation_state_path).unwrap();
    let before_shards = paths
        .static_mesh_shard_paths
        .iter()
        .map(|path| (fs::read(path).unwrap(), mtime(path)))
        .collect_vec();
    let state_mtime = mtime(&paths.generation_state_path);

    // Sleep briefly so mtime resolution cannot hide a rewrite.
    std::thread::sleep(Duration::from_millis(20));
    generate(&job, &mut NullProgressReporter).unwrap();

    assert_eq!(fs::read(&paths.generation_state_path).unwrap(), before_state);
    assert_eq!(mtime(&paths.generation_state_path), state_mtime);
    for (path, (before_bytes, before_mtime)) in paths.static_mesh_shard_paths.iter().zip(before_shards) {
        assert_eq!(fs::read(path).unwrap(), before_bytes);
        assert_eq!(mtime(path), before_mtime);
    }
}

/// An unchanged terrain domain must carry the committed occlusion grid rather than rebuild it.
///
/// The byte-level content gate alone would also report `skipped_unchanged`, so this asserts on
/// `occlusion_built_coarse` is the only signal that distinguishes "rebuilt then discarded" from
/// "never scanned the height domain at all".
#[test]
fn unchanged_terrain_domain_carries_occlusion_without_rebuilding_it() {
    let root = TempDir::new().unwrap();
    let inputs = build_hermetic_fixture(BASELINE_WORLD_V1, &root.path().join("inputs")).unwrap();
    let output_root = root.path().join("output");
    let job = hermetic_generation_job(&inputs, &output_root);

    let initial = generate(&job, &mut NullProgressReporter).unwrap();
    assert!(
        initial.report.metrics.terrain.occlusion_built_coarse,
        "the first run has no committed occlusion to carry"
    );

    let repeat = generate(&job, &mut NullProgressReporter).unwrap();
    assert!(
        !repeat.report.metrics.terrain.occlusion_built_coarse,
        "an unchanged terrain domain must skip the whole-domain occlusion scan"
    );
    assert_eq!(
        repeat.report.cache.writes.terrain_occlusion,
        Some(distantland::OutputWriteDecision::SkippedUnchanged)
    );
}

#[test]
fn exit_after_state_invalidation_leaves_cache_absent_and_next_run_rebuilds() {
    let root = TempDir::new().unwrap();
    let inputs = build_hermetic_fixture(BASELINE_WORLD_V1, &root.path().join("inputs")).unwrap();
    let output_root = root.path().join("output");
    let mut job = hermetic_generation_job(&inputs, &output_root);
    generate(&job, &mut NullProgressReporter).unwrap();
    open_output_snapshot(&output_root, Duration::from_secs(5), OutputValidation::Routine).unwrap();

    let job_path = root.path().join("job.json");
    job.settings.force_rebuild = true;
    write_generation_job_file(&job_path, &job).unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_distantland"))
        .arg("--job")
        .arg(&job_path)
        .env("RUST_LOG", "off")
        .env("TES3_DL_FAULT_EXIT", "after_state_invalidation")
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(42));

    let refused = open_output_snapshot(&output_root, Duration::from_secs(5), OutputValidation::Routine);
    assert!(refused.is_err(), "invalidated state must not serve");

    job.settings.force_rebuild = false;
    generate(&job, &mut NullProgressReporter).unwrap();
    open_output_snapshot(&output_root, Duration::from_secs(5), OutputValidation::Routine).unwrap();
}

#[test]
fn missing_required_artifact_forces_full_regeneration() {
    let root = TempDir::new().unwrap();
    let inputs = build_hermetic_fixture(BASELINE_WORLD_V1, &root.path().join("inputs")).unwrap();
    let output_root = root.path().join("output");
    let job = hermetic_generation_job(&inputs, &output_root);
    generate(&job, &mut NullProgressReporter).unwrap();

    let paths = OutputPaths::new(&output_root);
    fs::remove_file(&paths.usage_data_path).unwrap();
    assert!(open_output_snapshot(&output_root, Duration::from_secs(5), OutputValidation::Routine).is_err());

    generate(&job, &mut NullProgressReporter).unwrap();
    assert!(paths.usage_data_path.is_file());
    open_output_snapshot(&output_root, Duration::from_secs(5), OutputValidation::Routine).unwrap();
}

#[test]
fn corrupt_dirty_static_shard_falls_back_and_rebuilds_the_complete_bundle() {
    let root = TempDir::new().unwrap();
    let inputs = build_hermetic_fixture(BASELINE_WORLD_V1, &root.path().join("inputs")).unwrap();
    let output_root = root.path().join("output");
    let job = hermetic_generation_job(&inputs, &output_root);
    generate(&job, &mut NullProgressReporter).unwrap();

    let paths = OutputPaths::new(&output_root);
    for path in &paths.static_mesh_shard_paths {
        let mut bytes = fs::read(path).unwrap();
        if !distantland::mge_xe::distant_statics::deserialize_static_meshes(&bytes)
            .unwrap()
            .is_empty()
        {
            let last = bytes.last_mut().unwrap();
            *last ^= 0x5a;
            fs::write(path, bytes).unwrap();
        }
    }
    edit_fixture_nif_geometry(&inputs.data_dir, "Meshes/fixture_alpha.nif", 0, [-24.0, 0.0, 16.0]).unwrap();

    let recovered = generate(&job, &mut NullProgressReporter).unwrap();
    let shards = &recovered.report.cache.static_shards;
    assert_eq!(shards.reuse_mode, StaticReuseMode::FullRebuild);
    assert_eq!(shards.reuse_fallback_reason.as_deref(), Some("shard_hash_mismatch"));
    assert_eq!(shards.carried_shard_count, 0);
    assert_eq!(shards.written_shard_count, STATIC_MESH_SHARD_COUNT);
    open_output_snapshot(&output_root, Duration::from_secs(5), OutputValidation::Full).unwrap();
}

/// Atlas pages are inventory artifacts: removing one from disk must refuse serve and the next run
/// must republish it. (`missing_required_artifact_forces_full_regeneration` covers usage.data only.)
#[test]
fn missing_atlas_page_refuses_serve_and_next_run_republishes() {
    let root = TempDir::new().unwrap();
    let inputs = build_hermetic_fixture(BASELINE_WORLD_V1_ATLAS, &root.path().join("inputs")).unwrap();
    let output_root = root.path().join("output");
    let mut job = hermetic_generation_job(&inputs, &output_root);
    job.settings.max_static_atlas_size = 2048;
    job.settings.static_texture_sizing.mode = StaticTextureSizingMode::Off;
    // Pinned so the fixture keeps spilling onto a second page; the shipped defaults are a tuning
    // choice and lowering them would otherwise leave this test with no page 1 to remove.
    job.settings.max_static_texture_long_axis = 2048;
    job.settings.max_static_texture_short_axis = 2048;
    generate(&job, &mut NullProgressReporter).unwrap();

    let paths = OutputPaths::new(&output_root);
    let page1 = paths.atlas_texture_dir.join("_mge_xe_atlas_1.dds");
    assert!(page1.is_file(), "atlas fixture must publish opaque page 1");
    fs::remove_file(&page1).unwrap();
    assert!(
        open_output_snapshot(&output_root, Duration::from_secs(5), OutputValidation::Routine).is_err(),
        "missing inventory atlas page must not serve"
    );

    generate(&job, &mut NullProgressReporter).unwrap();
    assert!(page1.is_file(), "next run must republish the missing atlas page");
    open_output_snapshot(&output_root, Duration::from_secs(5), OutputValidation::Routine).unwrap();
}

/// Full validation must hash atlas DDS bodies. Same-length corruption keeps Routine (existence,
/// length, DDS header) happy while Full rejects on `content_blake3`, proving real publication
/// recorded a non-vacuous hash for atlas pages.
#[test]
fn full_validation_rejects_same_length_atlas_page_corruption() {
    let root = TempDir::new().unwrap();
    let inputs = build_hermetic_fixture(BASELINE_WORLD_V1_ATLAS, &root.path().join("inputs")).unwrap();
    let output_root = root.path().join("output");
    let mut job = hermetic_generation_job(&inputs, &output_root);
    job.settings.max_static_atlas_size = 2048;
    job.settings.static_texture_sizing.mode = StaticTextureSizingMode::Off;
    generate(&job, &mut NullProgressReporter).unwrap();

    let paths = OutputPaths::new(&output_root);
    let page = paths.atlas_texture_dir.join("_mge_xe_atlas.dds");
    let mut bytes = fs::read(&page).unwrap();
    // Flip a body byte past a typical DDS header so length and header magic stay valid for Routine.
    let idx = bytes.len().saturating_sub(16).max(128);
    bytes[idx] ^= 0xFF;
    fs::write(&page, &bytes).unwrap();

    open_output_snapshot(&output_root, Duration::from_secs(5), OutputValidation::Routine).unwrap();
    let error = open_output_snapshot(&output_root, Duration::from_secs(5), OutputValidation::Full).unwrap_err();
    assert_eq!(error.code(), "artifact_hash_mismatch");
}

/// Drives a one-cell LAND height edit away from the halo fixture's chunk edges.
fn edit_one_land_height(data_dir: &std::path::Path) {
    edit_fixture_land_height(data_dir, "fixture.esp", [0, 0], [32, 32], 3).unwrap();
}

#[test]
fn interrupted_partial_terrain_run_leaves_cache_absent_and_next_run_rebuilds() {
    let root = TempDir::new().unwrap();
    let inputs = build_hermetic_fixture(BASELINE_WORLD_V1_HALOSPAN, &root.path().join("inputs")).unwrap();
    let output_root = root.path().join("output");
    let job = hermetic_generation_job(&inputs, &output_root);
    generate(&job, &mut NullProgressReporter).unwrap();

    // A height-only edit makes the next run a partial terrain rebuild rather than a clean one.
    edit_one_land_height(&inputs.data_dir);
    let job_path = root.path().join("job.json");
    write_generation_job_file(&job_path, &job).unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_distantland"))
        .arg("--job")
        .arg(&job_path)
        .env("RUST_LOG", "off")
        .env("TES3_DL_FAULT_EXIT", "after_state_invalidation")
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(42));

    assert!(
        open_output_snapshot(&output_root, Duration::from_secs(5), OutputValidation::Routine).is_err(),
        "an interrupted partial terrain run must not serve"
    );

    generate(&job, &mut NullProgressReporter).unwrap();
    open_output_snapshot(&output_root, Duration::from_secs(5), OutputValidation::Full).unwrap();
}

/// A crash mid-terrain-DDS leaves deferred payload syncs outstanding: shards, atlas pages, and
/// terrain.bin were buffered but the pre-state barrier never ran. This is exactly the "crash
/// between buffered writes and barrier" window. The invalidated state must refuse to serve and
/// the next run must rebuild cleanly.
#[test]
fn exit_mid_terrain_dds_with_deferred_syncs_outstanding_refuses_serve_and_rebuilds() {
    let root = TempDir::new().unwrap();
    let inputs = build_hermetic_fixture(BASELINE_WORLD_V1_HALOSPAN, &root.path().join("inputs")).unwrap();
    let output_root = root.path().join("output");
    let mut job = hermetic_generation_job(&inputs, &output_root);
    generate(&job, &mut NullProgressReporter).unwrap();
    open_output_snapshot(&output_root, Duration::from_secs(5), OutputValidation::Routine).unwrap();

    let job_path = root.path().join("job.json");
    job.settings.force_rebuild = true;
    write_generation_job_file(&job_path, &job).unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_distantland"))
        .arg("--job")
        .arg(&job_path)
        .env("RUST_LOG", "off")
        .env("TES3_DL_FAULT_EXIT", "mid_terrain_dds")
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(42));

    let refused = open_output_snapshot(&output_root, Duration::from_secs(5), OutputValidation::Routine);
    assert!(refused.is_err(), "a run interrupted before the sync barrier must not serve");

    job.settings.force_rebuild = false;
    generate(&job, &mut NullProgressReporter).unwrap();
    open_output_snapshot(&output_root, Duration::from_secs(5), OutputValidation::Full).unwrap();
}

#[test]
fn forced_rebuild_publishes_metadata_a_later_unchanged_run_can_reuse() {
    let root = TempDir::new().unwrap();
    let inputs = build_hermetic_fixture(BASELINE_WORLD_V1_HALOSPAN, &root.path().join("inputs")).unwrap();
    let output_root = root.path().join("output");
    let mut job = hermetic_generation_job(&inputs, &output_root);

    job.settings.force_rebuild = true;
    let forced = generate(&job, &mut NullProgressReporter).unwrap();
    assert_eq!(forced.report.cache.static_shards.shard_count, STATIC_MESH_SHARD_COUNT);
    assert_eq!(forced.report.cache.static_shards.carried_shard_count, 0);
    assert_eq!(forced.report.cache.static_shards.written_shard_count, STATIC_MESH_SHARD_COUNT);
    assert_eq!(
        forced.report.cache.static_shards.written_shard_ids,
        (0..STATIC_MESH_SHARD_COUNT).map(|id| id as u8).collect_vec()
    );

    // The forced run must leave state a plain rerun can treat as a true no-op.
    let paths = OutputPaths::new(&output_root);
    let before_state = fs::read(&paths.generation_state_path).unwrap();
    let before_terrain = fs::read(&paths.terrain_path).unwrap();
    let before_shards = paths
        .static_mesh_shard_paths
        .iter()
        .map(|path| (fs::read(path).unwrap(), mtime(path)))
        .collect_vec();
    let state_mtime = mtime(&paths.generation_state_path);
    let terrain_mtime = mtime(&paths.terrain_path);

    std::thread::sleep(Duration::from_millis(20));
    job.settings.force_rebuild = false;
    generate(&job, &mut NullProgressReporter).unwrap();

    assert_eq!(fs::read(&paths.generation_state_path).unwrap(), before_state);
    assert_eq!(fs::read(&paths.terrain_path).unwrap(), before_terrain);
    for (path, (bytes, modified)) in paths.static_mesh_shard_paths.iter().zip(before_shards) {
        assert_eq!(fs::read(path).unwrap(), bytes);
        assert_eq!(mtime(path), modified);
    }
    assert_eq!(mtime(&paths.generation_state_path), state_mtime);
    assert_eq!(mtime(&paths.terrain_path), terrain_mtime);
}

#[test]
fn content_corrupt_terrain_payload_is_not_reused_and_the_next_run_regenerates() {
    let root = TempDir::new().unwrap();
    let inputs = build_hermetic_fixture(BASELINE_WORLD_V1_HALOSPAN, &root.path().join("inputs")).unwrap();
    let output_root = root.path().join("output");
    let job = hermetic_generation_job(&inputs, &output_root);
    generate(&job, &mut NullProgressReporter).unwrap();

    // Corrupt a payload byte past the header, preserving length: the header stays valid and Routine
    // validation still passes, so only the full-hash reuse check can catch this.
    let paths = OutputPaths::new(&output_root);
    let mut terrain = fs::read(&paths.terrain_path).unwrap();
    let last = terrain.len() - 1;
    terrain[last] ^= 0xFF;
    fs::write(&paths.terrain_path, &terrain).unwrap();

    // A height edit would otherwise reuse records from that payload; it must rebuild them instead.
    edit_one_land_height(&inputs.data_dir);
    let report = generate(&job, &mut NullProgressReporter).unwrap();

    // Assert the rejection itself: a weaker "output changed" check would pass anyway, because the
    // height edit rewrites terrain.bin regardless of whether the corrupt records were reused.
    let terrain_metrics = &report.report.metrics.terrain;
    assert_eq!(
        terrain_metrics.reuse_fallback_reason.as_deref(),
        Some("terrain_payload_hash_mismatch")
    );
    assert_eq!(terrain_metrics.reuse_mode, TerrainReuseMode::FullRebuild);
    assert_eq!(terrain_metrics.reused_records, 0);
    assert_eq!(terrain_metrics.rebuilt_work_items, terrain_metrics.mesh_work_items);

    let snapshot = open_output_snapshot(&output_root, Duration::from_secs(5), OutputValidation::Full).unwrap();
    assert!(snapshot.terrain_available());
    assert_ne!(
        fs::read(&paths.terrain_path).unwrap(),
        terrain,
        "the corrupt payload must not survive into the published tree"
    );
}

/// Reads each committed terrain record's minimum world X, in published file order.
fn committed_record_origins(paths: &OutputPaths) -> Vec<f32> {
    load_terrain_file(&paths.terrain_path)
        .unwrap()
        .meshes
        .iter()
        .map(|mesh| mesh.bounding_box.min.x)
        .collect()
}

/// A work item crossing the deep-water threshold inserts a record, in key order, mid-file.
///
/// The threshold fixture's work items at absolute cells 4 and 8 are uniformly flat at
/// `DEEP_WATER_Z`, so the default-cell fast path emits no record for either. Perturbing one height
/// inside the item at cell 4 makes that cell non-default and rises far enough above the selected
/// simplification error to survive the dense path, flipping the item from `None` to `Some(mesh)`.
/// The new record must land at ordinal 1, between the items starting at cells 0 and 12. This proves
/// assembly orders by work key rather than appending.
#[test]
fn deep_water_threshold_crossing_inserts_a_record_in_key_order() {
    let root = TempDir::new().unwrap();
    let inputs = build_hermetic_fixture(BASELINE_WORLD_V1_THRESHOLD, &root.path().join("inputs")).unwrap();
    let output_root = root.path().join("output");
    let job = hermetic_generation_job(&inputs, &output_root);
    let paths = OutputPaths::new(&output_root);

    let baseline = generate(&job, &mut NullProgressReporter).unwrap();
    assert_eq!(baseline.report.metrics.terrain.mesh_work_items, 4);
    // Only the items at cells 0 and 12 straddle non-deep-water terrain and emit.
    assert_eq!(committed_record_origins(&paths), [0.0, 12.0 * CELL_SIZE]);

    edit_fixture_land_height(&inputs.data_dir, "fixture.esp", [5, 0], [32, 32], 32).unwrap();
    let incremental = generate(&job, &mut NullProgressReporter).unwrap();

    let terrain = &incremental.report.metrics.terrain;
    assert_eq!(terrain.reuse_mode, TerrainReuseMode::PartialRebuild);
    assert_eq!(terrain.mesh_work_items, 4);
    assert_eq!(terrain.rebuilt_work_items, 1);
    assert_eq!(
        terrain.rebuilt_keys,
        [TerrainMeshWorkKey {
            start_cell_x: 4,
            start_cell_y: 0,
            cells_per_side: 4,
        }]
    );
    // The item at cell 8 stays deep water and clean: its prior *absence* is carried.
    assert_eq!(terrain.reused_records, 2);
    assert_eq!(terrain.reused_absent_work_items, 1);
    assert!(terrain.dds_carried, "a height-only edit must carry every DDS product");

    // The record count grew by exactly the one flipped item, inserted between its neighbours.
    assert_eq!(committed_record_origins(&paths), [0.0, 4.0 * CELL_SIZE, 12.0 * CELL_SIZE]);

    // Full validation binds the persisted record-key count to the header's mesh_count, so this
    // passing is what proves the published state metadata tracked the insertion.
    open_output_snapshot(&output_root, Duration::from_secs(5), OutputValidation::Full).unwrap();
}

/// Merged static geometry is trimmed against the LAND heightmap, so a height edit must reach the
/// statics cache and rebuild the affected cell's merge groups. Nothing else makes statics
/// terrain-dependent, so without that dependency in the merge unit fingerprint the statics bundle
/// would be carried whole and serve geometry culled against the previous heights.
#[test]
fn land_height_edit_rebuilds_the_merged_statics_it_culls() {
    let root = TempDir::new().unwrap();
    let inputs = build_hermetic_fixture(BASELINE_WORLD_V1, &root.path().join("inputs")).unwrap();
    let output_root = root.path().join("output");
    let job = hermetic_generation_job(&inputs, &output_root);

    let first = generate(&job, &mut NullProgressReporter).unwrap();
    assert!(
        first.report.cache.static_shards.merge_groups_planned > 0,
        "the fixture must merge something for this test to mean anything"
    );

    // An untouched rerun must still carry the bundle: the new terrain inputs have to be stable
    // across runs, not merely sensitive.
    let noop = generate(&job, &mut NullProgressReporter).unwrap();
    assert_eq!(noop.report.cache.static_shards.reuse_mode, StaticReuseMode::CarriedBundle);

    edit_one_land_height(&inputs.data_dir);
    let edited = generate(&job, &mut NullProgressReporter).unwrap();
    let shards = &edited.report.cache.static_shards;
    assert_ne!(
        shards.reuse_mode,
        StaticReuseMode::CarriedBundle,
        "a LAND height edit must invalidate the statics bundle"
    );
    assert!(
        shards.dirty_cell_count > 0,
        "the edited cell's merge unit must land in the dirty-owner closure"
    );
    assert!(
        shards.merge_groups_geometry_built > 0,
        "the dirty cell's merged geometry must actually be rebuilt"
    );
    open_output_snapshot(&output_root, Duration::from_secs(5), OutputValidation::Full).unwrap();
}
