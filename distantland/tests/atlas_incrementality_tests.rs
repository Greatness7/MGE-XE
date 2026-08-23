//! Atlas reconciliation integration tests.
//!
//! These drive the production generator as isolated subprocesses over the hermetic atlas fixture.
//! These cases cover family-local relocation, zero-write dedupe reconciliation, page pruning,
//! forced-clean logical equivalence, and force-rebuild carry bypass.

use std::fs;
use std::path::Path;
use std::process::Command;

use distantland::{GenerationJob, OutputPaths, STATIC_MESH_SHARD_COUNT, StaticTextureSizingMode};
use distantland_test_support::{
    BASELINE_WORLD_V1_ATLAS, HermeticFixturePaths, build_hermetic_fixture, hermetic_generation_job, rewrite_fixture_texture,
};
use distantland_test_support::{assert_static_outputs_equivalent, write_generation_job_file};

/// Mirrors the retained `s03-static-texture-changed` settings patch: the fixture's 1536px sources
/// pack into more than one page under a 2048px atlas cap, and geometry-informed sizing stays off so
/// selected dimensions follow the source dimensions exactly.
fn atlas_job(fixture: &HermeticFixturePaths, output_root: &Path) -> GenerationJob {
    let mut job = hermetic_generation_job(fixture, output_root);
    job.settings.max_static_atlas_size = 2048;
    job.settings.static_texture_sizing.mode = StaticTextureSizingMode::Off;
    // These tests need the fixture to occupy more than one page, so the texture caps must not
    // shrink it. Pinned rather than inherited: the shipped defaults are a tuning choice and have
    // been lowered before, which would otherwise silently collapse the fixture to a single page.
    job.settings.max_static_texture_long_axis = 2048;
    job.settings.max_static_texture_short_axis = 2048;
    job
}

fn run_generator(job_path: &Path) {
    let output = Command::new(env!("CARGO_BIN_EXE_distantland"))
        .arg("--job")
        .arg(job_path)
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

fn manifest(output_root: &Path) -> serde_json::Value {
    let path = distantland::OutputPaths::new(output_root).generation_report_path;
    let report: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    serde_json::to_value(report).unwrap()
}

fn metric<'a>(manifest: &'a serde_json::Value, pointer: &str) -> &'a serde_json::Value {
    manifest
        .pointer(pointer)
        .unwrap_or_else(|| panic!("missing manifest pointer {pointer}"))
}

/// A same-path opaque edit that changes source dimensions must miss the opaque family's cached
/// layout, relocate only the affected slot, and still carry all untouched pages. It must remain
/// logically equal to an independent forced clean rebuild of the same mutated inputs.
#[test]
fn opaque_dimension_change_repacks_one_family_and_matches_a_clean_rebuild() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = build_hermetic_fixture(BASELINE_WORLD_V1_ATLAS, &temp.path().join("inputs")).unwrap();
    let incremental_output = temp.path().join("output-incremental");
    let clean_output = temp.path().join("output-clean");

    let incremental_job_path = temp.path().join("job-incremental.json");
    write_generation_job_file(&incremental_job_path, &atlas_job(&fixture, &incremental_output)).unwrap();

    // Baseline leg: publish the committed atlas evidence the incremental leg will reuse.
    run_generator(&incremental_job_path);
    let baseline = manifest(&incremental_output);
    assert_eq!(metric(&baseline, "/metrics/atlas/built_opaque_page_count"), 2);
    assert_eq!(metric(&baseline, "/metrics/atlas/built_alpha_page_count"), 1);

    // Rewrite one opaque source at a different square dimension, leaving its path, format, and the
    // alpha family untouched. This changes only the opaque family's layout inputs.
    rewrite_fixture_texture(&fixture.data_dir, "Textures/fixture_opaque.dds", 1024, 145).unwrap();

    // Incremental leg: same job, same output tree.
    run_generator(&incremental_job_path);
    let incremental = manifest(&incremental_output);

    assert_eq!(
        metric(&incremental, "/metrics/atlas/opaque_layout_hit"),
        false,
        "the resized family must miss its cached layout"
    );
    assert_eq!(
        metric(&incremental, "/metrics/atlas/alpha_layout_hit"),
        true,
        "the untouched family must keep its cached layout"
    );
    assert_eq!(
        metric(&incremental, "/metrics/atlas/carried_opaque_page_count"),
        1,
        "the unrelated opaque page must carry"
    );
    assert_eq!(metric(&incremental, "/metrics/atlas/built_opaque_page_count"), 1);
    assert_eq!(metric(&incremental, "/metrics/atlas/opaque_family/plan_mode"), "reconciled");
    assert_eq!(metric(&incremental, "/metrics/atlas/opaque_family/retained_slots"), 2);
    assert_eq!(metric(&incremental, "/metrics/atlas/opaque_family/relocated_slots"), 0);
    assert_eq!(metric(&incremental, "/metrics/atlas/opaque_family/binding_delta/changed"), 1);
    assert_eq!(
        metric(&incremental, "/metrics/atlas/carried_alpha_page_count"),
        1,
        "the other family must carry from committed state"
    );
    assert_eq!(metric(&incremental, "/metrics/atlas/built_alpha_page_count"), 0);
    assert_eq!(metric(&incremental, "/cache/static_atlas_hit"), false);

    // Repacking the opaque family moves its UV bindings, so the owner planner rebuilds only the
    // consuming meshes/cells and splices their affected shards.
    assert_eq!(metric(&incremental, "/cache/static_shards/reuse_mode"), "owner_partial");
    assert!(
        metric(&incremental, "/cache/static_shards/decoded_shard_count")
            .as_u64()
            .unwrap()
            >= 1
    );
    let written_shards = metric(&incremental, "/cache/static_shards/written_shard_count")
        .as_u64()
        .unwrap();
    let carried_shards = metric(&incremental, "/cache/static_shards/carried_shard_count")
        .as_u64()
        .unwrap();
    assert!(written_shards >= 1, "changed bindings must write at least one shard");
    assert_eq!(
        written_shards + carried_shards,
        STATIC_MESH_SHARD_COUNT as u64,
        "every shard is either carried or written"
    );

    // Independent forced clean rebuild of the same mutated inputs, into its own tree.
    let mut clean = atlas_job(&fixture, &clean_output);
    clean.settings.force_rebuild = true;
    let clean_job_path = temp.path().join("job-clean.json");
    write_generation_job_file(&clean_job_path, &clean).unwrap();
    run_generator(&clean_job_path);

    // Logical equality also resolves every usage.data reference ordinal against the published static
    // directory and checks the declared mesh count against the fixed shard set, so a repack that
    // invalidated usage indices could not compare equal here.
    assert_static_outputs_equivalent(&incremental_output, &clean_output);

    let state_path = OutputPaths::new(&incremental_output).generation_state_path;
    let state_before_noop = fs::read(&state_path).unwrap();
    run_generator(&incremental_job_path);
    assert_eq!(
        fs::read(state_path).unwrap(),
        state_before_noop,
        "unchanged follow-up must not republish state"
    );
}

/// Exact-dedupe join of the fixture's two opaque sources must collapse two opaque pages to one
/// without rewriting the retained page, prune the superseded recognized page path, and stay logically
/// equal to an independent forced clean rebuild of the same mutated inputs.
#[test]
fn opaque_dedupe_join_shrinks_pages_prunes_recognized_path_and_matches_clean() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = build_hermetic_fixture(BASELINE_WORLD_V1_ATLAS, &temp.path().join("inputs")).unwrap();
    let incremental_output = temp.path().join("output-incremental");
    let clean_output = temp.path().join("output-clean");

    let incremental_job_path = temp.path().join("job-incremental.json");
    write_generation_job_file(&incremental_job_path, &atlas_job(&fixture, &incremental_output)).unwrap();

    run_generator(&incremental_job_path);
    let baseline = manifest(&incremental_output);
    assert_eq!(metric(&baseline, "/metrics/atlas/built_opaque_page_count"), 2);
    assert_eq!(metric(&baseline, "/metrics/atlas/built_alpha_page_count"), 1);

    let paths = OutputPaths::new(&incremental_output);
    // Page 0 is `_mge_xe_atlas.dds`; page 1 is the numbered form that shrink must prune.
    let opaque_page0 = paths.atlas_texture_dir.join("_mge_xe_atlas.dds");
    let opaque_page1 = paths.atlas_texture_dir.join("_mge_xe_atlas_1.dds");
    let alpha_page0 = paths.atlas_texture_dir.join("_mge_xe_atlas_alpha.dds");
    assert!(opaque_page0.is_file(), "baseline must publish opaque page 0");
    assert!(opaque_page1.is_file(), "baseline must publish opaque page 1");
    assert!(alpha_page0.is_file(), "baseline must publish alpha page 0");

    // Unknown-to-prune file: not a recognized atlas name, so it must survive even when a recognized
    // page is deleted. (Preservation itself is also covered by storage_gc_tests; here it sits next
    // to a real recognized-page deletion so both sides of the keep-set are live.)
    let unknown = paths.atlas_texture_dir.join("user_keep_me.txt");
    fs::write(&unknown, b"keep").unwrap();

    // Make fixture_opaque_other.dds byte-identical to fixture_opaque.dds so Exact dedupe joins them
    // onto one canonical frame and the opaque pack collapses from two pages to one.
    rewrite_fixture_texture(&fixture.data_dir, "Textures/fixture_opaque_other.dds", 1536, 0x11).unwrap();

    run_generator(&incremental_job_path);
    let incremental = manifest(&incremental_output);

    // Vacuity guard: a full rebuild would clear both layout hits and zero alpha carry.
    assert_eq!(
        metric(&incremental, "/metrics/atlas/alpha_layout_hit"),
        true,
        "untouched alpha family must keep its layout"
    );
    assert_eq!(
        metric(&incremental, "/metrics/atlas/carried_alpha_page_count"),
        1,
        "alpha page must carry from committed state"
    );
    assert_eq!(metric(&incremental, "/metrics/atlas/built_alpha_page_count"), 0);

    assert_eq!(
        metric(&incremental, "/metrics/atlas/opaque_layout_hit"),
        false,
        "dedupe join changes opaque layout inputs"
    );
    assert_eq!(
        metric(&incremental, "/metrics/atlas/built_opaque_page_count"),
        0,
        "removing the unreferenced trailing slot does not dirty retained content"
    );
    assert_eq!(metric(&incremental, "/metrics/atlas/carried_opaque_page_count"), 1);
    assert_eq!(metric(&incremental, "/metrics/atlas/opaque_family/plan_mode"), "reconciled");
    assert_eq!(metric(&incremental, "/metrics/atlas/opaque_family/truncated_pages"), 1);
    assert_eq!(metric(&incremental, "/metrics/atlas/opaque_family/binding_delta/changed"), 1);
    assert_eq!(metric(&incremental, "/metrics/atlas/zero_page_write_reconciliation_count"), 1);
    assert_eq!(metric(&incremental, "/cache/static_atlas_hit"), false);

    assert!(opaque_page0.is_file(), "shrunk inventory still lists opaque page 0");
    assert!(!opaque_page1.exists(), "superseded recognized opaque page 1 must be pruned");
    assert!(alpha_page0.is_file(), "carried alpha page must remain on disk");
    assert!(unknown.is_file(), "unrecognized files must not be pruned");

    // Bindings move with the join, so the owner planner rebuilds their consumers and splices only
    // the affected shards.
    assert_eq!(metric(&incremental, "/cache/static_shards/reuse_mode"), "owner_partial");
    assert!(
        metric(&incremental, "/cache/static_shards/decoded_shard_count")
            .as_u64()
            .unwrap()
            >= 1
    );
    let written_shards = metric(&incremental, "/cache/static_shards/written_shard_count")
        .as_u64()
        .unwrap();
    let carried_shards = metric(&incremental, "/cache/static_shards/carried_shard_count")
        .as_u64()
        .unwrap();
    assert!(written_shards >= 1, "changed bindings must write at least one shard");
    assert_eq!(
        written_shards + carried_shards,
        STATIC_MESH_SHARD_COUNT as u64,
        "every shard is either carried or written"
    );

    let mut clean = atlas_job(&fixture, &clean_output);
    clean.settings.force_rebuild = true;
    let clean_job_path = temp.path().join("job-clean.json");
    write_generation_job_file(&clean_job_path, &clean).unwrap();
    run_generator(&clean_job_path);

    assert_static_outputs_equivalent(&incremental_output, &clean_output);

    let state_path = OutputPaths::new(&incremental_output).generation_state_path;
    let state_before_noop = fs::read(&state_path).unwrap();
    run_generator(&incremental_job_path);
    assert_eq!(
        fs::read(state_path).unwrap(),
        state_before_noop,
        "unchanged follow-up must not republish state"
    );
}

/// After a warm atlas cache exists, force rebuild must publish every atlas page and carry none.
#[test]
fn force_rebuild_bypasses_atlas_page_carry() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = build_hermetic_fixture(BASELINE_WORLD_V1_ATLAS, &temp.path().join("inputs")).unwrap();
    let output_root = temp.path().join("output");
    let job_path = temp.path().join("job.json");

    write_generation_job_file(&job_path, &atlas_job(&fixture, &output_root)).unwrap();
    run_generator(&job_path);
    let warm = manifest(&output_root);
    assert_eq!(metric(&warm, "/metrics/atlas/built_opaque_page_count"), 2);
    assert_eq!(metric(&warm, "/metrics/atlas/built_alpha_page_count"), 1);

    let mut forced = atlas_job(&fixture, &output_root);
    forced.settings.force_rebuild = true;
    write_generation_job_file(&job_path, &forced).unwrap();
    run_generator(&job_path);
    let forced_manifest = manifest(&output_root);

    assert_eq!(metric(&forced_manifest, "/cache/static_atlas_hit"), false);
    assert_eq!(metric(&forced_manifest, "/metrics/atlas/carried_opaque_page_count"), 0);
    assert_eq!(metric(&forced_manifest, "/metrics/atlas/carried_alpha_page_count"), 0);
    assert_eq!(metric(&forced_manifest, "/metrics/atlas/built_opaque_page_count"), 2);
    assert_eq!(metric(&forced_manifest, "/metrics/atlas/built_alpha_page_count"), 1);
    assert_eq!(metric(&forced_manifest, "/cache/static_shards/reuse_mode"), "forced_full");
    assert_eq!(metric(&forced_manifest, "/cache/static_shards/carried_shard_count"), 0);
    assert_eq!(
        metric(&forced_manifest, "/cache/static_shards/written_shard_count"),
        STATIC_MESH_SHARD_COUNT
    );
}
