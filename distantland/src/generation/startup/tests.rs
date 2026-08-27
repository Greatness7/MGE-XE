//! Startup status unit tests for the complete-or-absent store.

use super::*;
use crate::generation::progress::{GenerationStage, ProgressReporter};
use crate::generation::state_db::{self, GenerationState};
use crate::generation::storage::lock::LockFile;
use crate::generation::storage::state::{ArtifactKind, CommittedState, artifact_from_written, encode};
use crate::generation::{GenerationRunResult, ensure_with_output_root, take_front_half_invocations};
use crate::{NullProgressReporter, generate};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[test]
fn output_status_reports_missing_state() {
    let dir = tempfile::tempdir().unwrap();
    let job = test_job(dir.path());
    let paths = OutputPaths::new(dir.path());

    let status = check_output_paths_status(&job, &paths).unwrap();

    assert_eq!(status.kind(), OutputStatusKind::Missing);
    assert!(status.details().issues.iter().any(|issue| issue.code == "missing_state"));
}

#[test]
fn status_with_an_explicit_output_root_needs_no_morrowind_install() {
    let dir = tempfile::tempdir().unwrap();
    // An ini path that cannot resolve. A job naming its own output_root must not load the VFS,
    // so this must still succeed. The neighbouring tests cover the same path only on a machine
    // that happens to have Morrowind installed, which is how this passed locally and failed CI.
    let job = GenerationJob {
        morrowind_ini: Some(dir.path().join("absent").join("Morrowind.ini")),
        ..test_job(dir.path())
    };

    let status = check_output_status(&job).unwrap();

    assert_eq!(status.kind(), OutputStatusKind::Missing);
}

#[test]
fn output_status_reports_that_a_version_byte_without_state_is_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let job = test_job(dir.path());
    let paths = OutputPaths::new(dir.path());
    paths.ensure_parent_dirs().unwrap();
    fs::write(&paths.version_path, [MGE_DL_VERSION]).unwrap();

    let status = check_output_status(&job).unwrap();

    assert_eq!(status.kind(), OutputStatusKind::Invalid);
    assert!(
        status
            .details()
            .issues
            .iter()
            .any(|issue| { issue.code == "missing_state" || issue.code == "corrupt_state" || issue.code == "state_io" })
    );
}

#[test]
fn output_status_formats_issues_for_logs() {
    let dir = tempfile::tempdir().unwrap();
    let job = test_job(dir.path());
    let paths = OutputPaths::new(dir.path());

    let status = check_output_paths_status(&job, &paths).unwrap();

    let formatted = status.format_issues();
    assert!(formatted.starts_with("missing: missing_state:"));
    assert!(formatted.contains("Missing generation state"));
    assert_eq!(formatted, format_output_status_issues(&status));
}

#[test]
fn older_version_byte_is_incompatible_not_servable() {
    let dir = tempfile::tempdir().unwrap();
    let job = test_job(dir.path());
    let paths = OutputPaths::new(dir.path());
    paths.ensure_parent_dirs().unwrap();
    fs::write(&paths.version_path, [13]).unwrap();

    let status = check_output_status(&job).unwrap();
    assert_eq!(status.kind(), OutputStatusKind::Invalid);
    assert!(
        status
            .details()
            .issues
            .iter()
            .any(|issue| issue.code == "incompatible_older_version")
    );
}

#[test]
fn full_startup_check_detects_an_asset_only_edit() {
    let dir = tempfile::tempdir().unwrap();
    let fixture =
        crate::test_support::build_hermetic_fixture(crate::test_support::BASELINE_WORLD_V1, &dir.path().join("inputs"))
            .unwrap();
    let job = crate::test_support::hermetic_generation_job(&fixture, &dir.path().join("output"));
    let mut reporter = NullProgressReporter;
    generate(&job, &mut reporter).unwrap();
    assert_eq!(check_output_status(&job).unwrap().kind(), OutputStatusKind::Valid);

    crate::test_support::edit_fixture_nif_geometry(&fixture.data_dir, "Meshes/fixture_alpha.nif", 0, [-24.0, 0.0, 16.0])
        .unwrap();

    let status = check_output_status(&job).unwrap();
    assert_eq!(status.kind(), OutputStatusKind::Stale);
    assert!(
        status
            .details()
            .issues
            .iter()
            .any(|issue| issue.code == "stale_generation_inputs")
    );
    assert!(
        status
            .details()
            .issues
            .iter()
            .any(|issue| issue.message.contains("meshes +0/-0/~1"))
    );

    let _ = take_front_half_invocations();
    let outcome = ensure_generated(&job, &mut reporter).unwrap();
    assert_eq!(take_front_half_invocations(), 1, "stale ensure must run the front half once");
    match outcome {
        GenerationOutcome::Generated { previous_status, .. } => {
            assert_eq!(previous_status.kind(), OutputStatusKind::Stale);
            assert!(
                previous_status
                    .details()
                    .issues
                    .iter()
                    .any(|issue| issue.code == "stale_generation_inputs")
            );
        }
        other => panic!("expected Generated, got {other:?}"),
    }
    assert_eq!(check_output_status(&job).unwrap().kind(), OutputStatusKind::Valid);
}

#[test]
fn ensure_generated_noop_on_unchanged_tree() {
    let dir = tempfile::tempdir().unwrap();
    let fixture =
        crate::test_support::build_hermetic_fixture(crate::test_support::BASELINE_WORLD_V1, &dir.path().join("inputs"))
            .unwrap();
    let job = crate::test_support::hermetic_generation_job(&fixture, &dir.path().join("output"));
    let mut reporter = NullProgressReporter;
    generate(&job, &mut reporter).unwrap();

    let paths = OutputPaths::new(job.output_root.clone().unwrap());
    let before_state = fs::read(&paths.generation_state_path).unwrap();
    let before_report = fs::metadata(&paths.generation_report_path)
        .ok()
        .and_then(|m| m.modified().ok());
    let before_usage = fs::metadata(&paths.usage_data_path).unwrap().modified().unwrap();

    let outcome = ensure_generated(&job, &mut reporter).unwrap();
    assert!(matches!(outcome, GenerationOutcome::AlreadyValid { .. }));
    assert_eq!(fs::read(&paths.generation_state_path).unwrap(), before_state);
    assert_eq!(
        fs::metadata(&paths.generation_report_path)
            .ok()
            .and_then(|m| m.modified().ok()),
        before_report
    );
    assert_eq!(
        fs::metadata(&paths.usage_data_path).unwrap().modified().unwrap(),
        before_usage
    );
}

#[test]
fn ensure_generated_with_force_rebuild_does_not_report_stale_after_publish() {
    let dir = tempfile::tempdir().unwrap();
    let fixture =
        crate::test_support::build_hermetic_fixture(crate::test_support::BASELINE_WORLD_V1, &dir.path().join("inputs"))
            .unwrap();
    let mut job = crate::test_support::hermetic_generation_job(&fixture, &dir.path().join("output"));
    job.settings.force_rebuild = true;
    let mut reporter = NullProgressReporter;

    let outcome = ensure_generated(&job, &mut reporter).unwrap();
    match &outcome {
        GenerationOutcome::Generated { previous_status, .. } => {
            // First run has no base → Missing (or Invalid if partial debris).
            assert!(
                matches!(previous_status.kind(), OutputStatusKind::Missing | OutputStatusKind::Invalid),
                "force first publish previous_status={previous_status:?}"
            );
        }
        other => panic!("force_rebuild must publish: {other:?}"),
    }
    // Post-publish status must ignore execution-only force_rebuild (MGEXEgui leaves it set).
    assert_eq!(check_output_status(&job).unwrap().kind(), OutputStatusKind::Valid);
    let outcome2 = ensure_generated(&job, &mut reporter).unwrap();
    match outcome2 {
        GenerationOutcome::Generated { previous_status, .. } => {
            // Valid base still forced: previous_status remains Valid, not post-publish Stale.
            assert_eq!(previous_status.kind(), OutputStatusKind::Valid);
        }
        other => panic!("force_rebuild still set should regenerate, not fail as stale: {other:?}"),
    }
    assert_eq!(check_output_status(&job).unwrap().kind(), OutputStatusKind::Valid);
}

#[test]
fn ensure_mode_continues_dirty_diff_without_second_front_half() {
    let dir = tempfile::tempdir().unwrap();
    let fixture =
        crate::test_support::build_hermetic_fixture(crate::test_support::BASELINE_WORLD_V1, &dir.path().join("inputs"))
            .unwrap();
    let job = crate::test_support::hermetic_generation_job(&fixture, &dir.path().join("output"));
    generate(&job, &mut NullProgressReporter).unwrap();

    crate::test_support::edit_fixture_nif_geometry(&fixture.data_dir, "Meshes/fixture_alpha.nif", 0, [-24.0, 0.0, 16.0])
        .unwrap();

    let options = crate::vfs::VfsLoadOptions {
        morrowind_ini: job.morrowind_ini.clone(),
        data_dirs: job.data_dirs.clone(),
        plugins: job.plugins.clone(),
    };
    let vfs = crate::Vfs::load_metadata_only(&options).unwrap();
    let paths = OutputPaths::new(job.resolved_output_root(&vfs));
    let writer = crate::generation::storage::authority::WriterSession::acquire(paths).unwrap();

    let _ = take_front_half_invocations();
    let result = ensure_with_output_root(&job, writer, true, &mut NullProgressReporter).unwrap();
    assert_eq!(take_front_half_invocations(), 1);
    match result {
        GenerationRunResult::Published(validated) => {
            assert!(validated.report.report.mge_xe_contract.complete);
        }
        other => panic!("dirty ensure must publish in one call, got {other:?}"),
    }
}

#[test]
fn ensure_generated_reports_one_front_half_stage_sequence() {
    let dir = tempfile::tempdir().unwrap();
    let fixture =
        crate::test_support::build_hermetic_fixture(crate::test_support::BASELINE_WORLD_V1, &dir.path().join("inputs"))
            .unwrap();
    let job = crate::test_support::hermetic_generation_job(&fixture, &dir.path().join("output"));
    generate(&job, &mut NullProgressReporter).unwrap();

    // Unchanged ensure: front-half stages are now reported.
    let mut recording = RecordingReporter::default();
    let outcome = ensure_generated(&job, &mut recording).unwrap();
    assert!(matches!(outcome, GenerationOutcome::AlreadyValid { .. }));
    let stages = recording.stages();
    assert!(
        stages.contains(&GenerationStage::InitializeVfs),
        "unchanged ensure must report front-half stages: {stages:?}"
    );
    assert!(
        stages.contains(&GenerationStage::ComputeUnitDiff),
        "unchanged ensure must reach the diff boundary: {stages:?}"
    );
    assert!(
        !stages.contains(&GenerationStage::WriteGenerationReport),
        "unchanged ensure must not publish: {stages:?}"
    );

    crate::test_support::edit_fixture_nif_geometry(&fixture.data_dir, "Meshes/fixture_alpha.nif", 0, [-24.0, 0.0, 16.0])
        .unwrap();

    let mut recording = RecordingReporter::default();
    let outcome = ensure_generated(&job, &mut recording).unwrap();
    assert!(matches!(outcome, GenerationOutcome::Generated { .. }));
    let stages = recording.stages();
    assert!(
        stages.contains(&GenerationStage::InitializeVfs) && stages.contains(&GenerationStage::WriteGenerationReport),
        "dirty ensure reports one front half then publish stages: {stages:?}"
    );
    let front_half_starts = stages
        .iter()
        .filter(|stage| **stage == GenerationStage::InitializeVfs)
        .count();
    assert_eq!(front_half_starts, 1, "only one front half in dirty ensure: {stages:?}");
}

#[test]
fn ensure_generated_reports_missing_previous_status() {
    let dir = tempfile::tempdir().unwrap();
    let fixture =
        crate::test_support::build_hermetic_fixture(crate::test_support::BASELINE_WORLD_V1, &dir.path().join("inputs"))
            .unwrap();
    let job = crate::test_support::hermetic_generation_job(&fixture, &dir.path().join("output"));
    let outcome = ensure_generated(&job, &mut NullProgressReporter).unwrap();
    match outcome {
        GenerationOutcome::Generated { previous_status, .. } => {
            assert_eq!(previous_status.kind(), OutputStatusKind::Missing)
        }
        other => panic!("expected Generated from missing base, got {other:?}"),
    }
}

#[test]
fn ensure_generated_reports_invalid_previous_status() {
    let dir = tempfile::tempdir().unwrap();
    let fixture =
        crate::test_support::build_hermetic_fixture(crate::test_support::BASELINE_WORLD_V1, &dir.path().join("inputs"))
            .unwrap();
    let job = crate::test_support::hermetic_generation_job(&fixture, &dir.path().join("output"));
    generate(&job, &mut NullProgressReporter).unwrap();

    let paths = OutputPaths::new(job.output_root.as_ref().unwrap());
    fs::remove_file(&paths.usage_data_path).unwrap();

    let outcome = ensure_generated(&job, &mut NullProgressReporter).unwrap();
    match outcome {
        GenerationOutcome::Generated { previous_status, .. } => {
            assert_eq!(previous_status.kind(), OutputStatusKind::Invalid);
            assert!(
                previous_status
                    .details()
                    .issues
                    .iter()
                    .any(|issue| issue.code.contains("state")
                        || issue.code.contains("artifact")
                        || issue.code == "invalid_or_incomplete_state")
            );
        }
        other => panic!("expected Generated from invalid base, got {other:?}"),
    }
    assert_eq!(check_output_status(&job).unwrap().kind(), OutputStatusKind::Valid);
}

#[test]
fn ensure_holds_exclusive_lock_through_generation_report_stage() {
    let dir = tempfile::tempdir().unwrap();
    let fixture =
        crate::test_support::build_hermetic_fixture(crate::test_support::BASELINE_WORLD_V1, &dir.path().join("inputs"))
            .unwrap();
    let job = crate::test_support::hermetic_generation_job(&fixture, &dir.path().join("output"));

    let paths = OutputPaths::new(job.output_root.as_ref().unwrap());
    let mut reporter = LockProbeReporter {
        lock_path: paths.writer_lock_path.clone(),
        shared_failed_during_generation_report: false,
    };
    ensure_generated(&job, &mut reporter).unwrap();
    assert!(
        reporter.shared_failed_during_generation_report,
        "shared lock must fail while exclusive ensure session is still held at WriteGenerationReport"
    );
}

#[test]
fn ensure_fails_when_artifact_is_damaged_before_final_validation() {
    let dir = tempfile::tempdir().unwrap();
    let fixture =
        crate::test_support::build_hermetic_fixture(crate::test_support::BASELINE_WORLD_V1, &dir.path().join("inputs"))
            .unwrap();
    let job = crate::test_support::hermetic_generation_job(&fixture, &dir.path().join("output"));

    let paths = OutputPaths::new(job.output_root.as_ref().unwrap());
    let mut reporter = DamageUsageOnManifest {
        usage_path: paths.usage_data_path.clone(),
    };
    let error = ensure_generated(&job, &mut reporter).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("validation") || message.contains("Routine") || message.contains("artifact"),
        "expected validation failure, got {message}"
    );
}

#[test]
fn ensure_fails_when_state_is_corrupted_before_final_validation() {
    let dir = tempfile::tempdir().unwrap();
    let fixture =
        crate::test_support::build_hermetic_fixture(crate::test_support::BASELINE_WORLD_V1, &dir.path().join("inputs"))
            .unwrap();
    let job = crate::test_support::hermetic_generation_job(&fixture, &dir.path().join("output"));

    let paths = OutputPaths::new(job.output_root.as_ref().unwrap());
    let mut reporter = CorruptStateOnManifest {
        state_path: paths.generation_state_path.clone(),
    };
    let error = ensure_generated(&job, &mut reporter).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("validation")
            || message.contains("generation_state")
            || message.contains("state")
            || message.contains("decode"),
        "expected state reload failure, got {message}"
    );
}

#[test]
fn advisory_report_changes_do_not_affect_state_backed_validity() {
    let dir = tempfile::tempdir().unwrap();
    let fixture =
        crate::test_support::build_hermetic_fixture(crate::test_support::BASELINE_WORLD_V1, &dir.path().join("inputs"))
            .unwrap();
    let job = crate::test_support::hermetic_generation_job(&fixture, &dir.path().join("output"));
    generate(&job, &mut NullProgressReporter).unwrap();

    let paths = OutputPaths::new(job.output_root.as_ref().unwrap());
    let before_state = fs::read(&paths.generation_state_path).unwrap();
    fs::write(&paths.generation_report_path, "not valid toml = [").unwrap();

    let outcome = ensure_generated(&job, &mut NullProgressReporter).unwrap();
    assert!(matches!(outcome, GenerationOutcome::AlreadyValid { .. }));
    assert_eq!(fs::read(&paths.generation_state_path).unwrap(), before_state);
    assert_eq!(check_output_status(&job).unwrap().kind(), OutputStatusKind::Valid);
}

#[test]
fn damaged_required_artifact_makes_status_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let fixture =
        crate::test_support::build_hermetic_fixture(crate::test_support::BASELINE_WORLD_V1, &dir.path().join("inputs"))
            .unwrap();
    let job = crate::test_support::hermetic_generation_job(&fixture, &dir.path().join("output"));
    generate(&job, &mut NullProgressReporter).unwrap();

    let paths = OutputPaths::new(job.output_root.as_ref().unwrap());
    fs::remove_file(&paths.usage_data_path).unwrap();
    let status = check_output_status(&job).unwrap();
    assert_eq!(status.kind(), OutputStatusKind::Invalid);
    assert!(status.details().issues.iter().any(|issue| issue.code == "missing_artifact"));
}

#[derive(Default)]
struct RecordingReporter {
    stages: Vec<GenerationStage>,
}

impl RecordingReporter {
    fn stages(&self) -> &[GenerationStage] {
        &self.stages
    }
}

impl ProgressReporter for RecordingReporter {
    fn begin_stage(&mut self, stage: GenerationStage) {
        self.stages.push(stage);
    }
}

struct LockProbeReporter {
    lock_path: PathBuf,
    shared_failed_during_generation_report: bool,
}

impl ProgressReporter for LockProbeReporter {
    fn begin_stage(&mut self, stage: GenerationStage) {
        if stage == GenerationStage::WriteGenerationReport {
            let lock = LockFile::new(&self.lock_path);
            self.shared_failed_during_generation_report = lock.try_shared().ok().flatten().is_none();
        }
    }
}

struct DamageUsageOnManifest {
    usage_path: PathBuf,
}

impl ProgressReporter for DamageUsageOnManifest {
    fn finish_stage(&mut self, stage: GenerationStage, _elapsed_ms: f64) {
        if stage == GenerationStage::WriteGenerationReport {
            let _ = fs::remove_file(&self.usage_path);
        }
    }
}

struct CorruptStateOnManifest {
    state_path: PathBuf,
}

impl ProgressReporter for CorruptStateOnManifest {
    fn finish_stage(&mut self, stage: GenerationStage, _elapsed_ms: f64) {
        if stage == GenerationStage::WriteGenerationReport {
            // Overwrite the just-published state so reload/validation fails despite a valid in-memory value.
            let _ = fs::write(&self.state_path, b"not-a-valid-generation-state");
        }
    }
}

fn test_job(output_root: &Path) -> GenerationJob {
    GenerationJob {
        output_root: Some(output_root.to_path_buf()),
        ..GenerationJob::default()
    }
}

/// Builds a minimal syntactically valid state-backed tree for header/inventory experiments.
#[allow(dead_code)]
fn complete_output(output_root: &Path) -> OutputPaths {
    let paths = OutputPaths::new(output_root);
    paths.ensure_parent_dirs().unwrap();
    fs::write(&paths.version_path, [MGE_DL_VERSION]).unwrap();

    let static_bytes = crate::statics::serialize_static_meshes(&crate::PackedDistantStatics::default()).unwrap();
    for path in &paths.static_mesh_shard_paths {
        fs::write(path, &static_bytes).unwrap();
    }
    let usage_bytes = 0_u32.to_le_bytes();
    fs::write(&paths.usage_data_path, usage_bytes).unwrap();

    let mut terrain_bytes = b"XELAND02".to_vec();
    terrain_bytes.extend_from_slice(&2_u32.to_le_bytes());
    fs::write(&paths.terrain_path, &terrain_bytes).unwrap();
    let mut occlusion = b"XEOCCL02".to_vec();
    occlusion.extend_from_slice(&2_u32.to_le_bytes());
    fs::write(&paths.terrain_occlusion_path, &occlusion).unwrap();
    let mut dds = b"DDS ".to_vec();
    dds.resize(128, 0);
    for path in [
        &paths.terrain_atlas_path,
        &paths.terrain_blend_patterns_path,
        &paths.terrain_material_path,
        &paths.terrain_material_flags_path,
        &paths.terrain_patch_albedo_path,
    ] {
        fs::write(path, &dds).unwrap();
    }
    fs::write(&paths.writer_lock_path, []).unwrap();

    let mut generation = GenerationState::default();
    generation.terrain_enabled = true;
    let mut artifacts = vec![
        artifact_from_written(
            ArtifactKind::Version,
            "version",
            1,
            *blake3::hash(&[MGE_DL_VERSION]).as_bytes(),
        ),
        artifact_from_written(
            ArtifactKind::Usage,
            "statics\\usage.data",
            usage_bytes.len() as u64,
            *blake3::hash(&usage_bytes).as_bytes(),
        ),
        artifact_from_written(
            ArtifactKind::TerrainPayload,
            "terrain.bin",
            terrain_bytes.len() as u64,
            *blake3::hash(&terrain_bytes).as_bytes(),
        ),
        artifact_from_written(
            ArtifactKind::TerrainOcclusion,
            "terrain_occlusion.bin",
            occlusion.len() as u64,
            *blake3::hash(&occlusion).as_bytes(),
        ),
    ];
    artifacts.extend(paths.static_mesh_shard_paths.iter().enumerate().map(|(shard_id, _)| {
        artifact_from_written(
            ArtifactKind::StaticShard,
            crate::generation::output::static_mesh_shard_relative_path(shard_id),
            static_bytes.len() as u64,
            *blake3::hash(&static_bytes).as_bytes(),
        )
    }));
    for name in [
        "terrain_atlas.dds",
        "terrain_blend_patterns.dds",
        "terrain_material.dds",
        "terrain_material_flags.dds",
        "terrain_patch_albedo.dds",
    ] {
        artifacts.push(artifact_from_written(
            ArtifactKind::TerrainDds,
            name,
            dds.len() as u64,
            *blake3::hash(&dds).as_bytes(),
        ));
    }
    let state = CommittedState::new(state_db::encode(&generation).unwrap(), artifacts).unwrap();
    fs::write(&paths.generation_state_path, encode(&state).unwrap()).unwrap();
    let _ = Duration::from_secs(0);
    paths
}
