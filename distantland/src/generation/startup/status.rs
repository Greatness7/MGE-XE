use std::time::Duration;

use crate::output_index::{OutputIndexError, OutputValidation, open_output_snapshot};

use super::*;

/// How long a status check waits for the shared reader lock before reporting the tree unreadable.
const SNAPSHOT_LOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// Checks the current output tree for `job` without mutating any files.
///
/// Validity is derived exclusively from the state-backed snapshot. The advisory generation
/// report is not read during routine startup checks.
///
/// # Errors
///
/// Returns an error if the request cannot be validated or if the required VFS metadata
/// cannot be resolved. Future versions are reported as `Invalid` with a refusal code.
pub fn check_output_status(job: &GenerationJob) -> Result<OutputStatus> {
    let output_paths = load_output_paths(job)?;
    match read_version_gate(&output_paths.version_path) {
        VersionGate::Incompatible(issue) => Ok(OutputStatus::new(
            OutputStatusKind::Invalid,
            status_details(&output_paths, output_paths.validate_mge_xe_contract(), vec![issue]),
        )),
        VersionGate::Current => status_from_routine_snapshot(job, &output_paths, |error| {
            format!("version-{MGE_DL_VERSION} output is not serveable: {error}")
        }),
        // Advisory path: a malformed or absent marker is reported alongside the path checks.
        VersionGate::Malformed | VersionGate::Absent => check_output_paths_status(job, &output_paths),
    }
}

/// Classifies a current-version tree from a shared Routine snapshot: `Valid` when the generation
/// inputs are still current, `Stale` when they are not, and `Invalid` when the snapshot cannot be
/// opened at all.
///
/// The two status entry points word an open failure differently, so the caller supplies the
/// message; the reason code and every other branch are shared.
fn status_from_routine_snapshot(
    job: &GenerationJob,
    output_paths: &OutputPaths,
    snapshot_error_message: impl FnOnce(&OutputIndexError) -> String,
) -> Result<OutputStatus> {
    let opened = open_output_snapshot(&output_paths.output_root, SNAPSHOT_LOCK_TIMEOUT, OutputValidation::Routine);
    let snapshot = match opened {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let issue = OutputStatusIssue {
                code: error.code().to_string(),
                message: snapshot_error_message(&error),
            };
            return Ok(OutputStatus::new(
                OutputStatusKind::Invalid,
                status_details(output_paths, output_paths.validate_mge_xe_contract(), vec![issue]),
            ));
        }
    };

    let mut details = status_details(snapshot.paths(), snapshot.paths().validate_mge_xe_contract(), vec![]);
    let input_status =
        crate::generation::generation_inputs_are_current(job, snapshot.paths().clone(), snapshot.committed_state().clone())?;
    if input_status.current {
        return Ok(OutputStatus::new(OutputStatusKind::Valid, details));
    }
    details.issues.push(stale_generation_inputs_issue(input_status.rebuild_cause));
    Ok(OutputStatus::new(OutputStatusKind::Stale, details))
}

/// Builds a status for a Routine-valid base already held under the writer lock.
///
/// Storage is valid; currency is checked separately by the caller.
fn status_from_valid_base(output_paths: &OutputPaths) -> OutputStatus {
    let details = status_details(output_paths, output_paths.validate_mge_xe_contract(), vec![]);
    OutputStatus::new(OutputStatusKind::Valid, details)
}

/// Classifies pre-run status under the exclusive writer session without a second shared snapshot.
///
/// For a Routine-valid base this reports state-backed validity; currency is decided later
/// by the single ensure front half. For absent/migration bases this reports
/// Missing or Invalid from the locked tree without opening a shared reader session.
pub(super) fn previous_status_under_lock(
    writer: &crate::generation::storage::authority::WriterSession,
) -> Result<OutputStatus> {
    use crate::generation::storage::authority::CacheClass;

    match writer.classification() {
        CacheClass::Valid => {
            // A Valid classification must carry the Routine-validated base state that produced it.
            writer
                .base_state()
                .context("valid version-16 output has no committed state")?;
            Ok(status_from_valid_base(writer.paths()))
        }
        CacheClass::Absent => Ok(classify_absent_under_lock(writer.paths())),
    }
}

/// When ensure continued past a Valid advisory classification because the unit diff was dirty,
/// upgrade `previous_status` to `Stale` with the rebuild cause from the published report.
pub(super) fn refine_previous_status_after_dirty_ensure(
    previous_status: OutputStatus,
    report: &GenerationReport,
) -> OutputStatus {
    let mut previous_status = previous_status;
    if previous_status.kind == OutputStatusKind::Valid {
        previous_status
            .details
            .issues
            .push(stale_generation_inputs_issue(report.report.rebuild_cause.clone()));
        previous_status.kind = OutputStatusKind::Stale;
    }
    previous_status
}

fn classify_absent_under_lock(output_paths: &OutputPaths) -> OutputStatus {
    let validation = output_paths.validate_mge_xe_contract();
    let state_exists = output_paths.generation_state_path.exists();
    let issue = match read_version_gate(&output_paths.version_path) {
        // WriterSession::acquire refuses future versions before this helper runs.
        VersionGate::Incompatible(issue) => issue,
        VersionGate::Current if state_exists => OutputStatusIssue {
            code: "invalid_or_incomplete_state".to_string(),
            message: format!("version-{MGE_DL_VERSION} generation state is absent, corrupt, or incomplete"),
        },
        VersionGate::Current => missing_state_issue(output_paths),
        VersionGate::Malformed => OutputStatusIssue {
            code: "corrupt_version".to_string(),
            message: format!(
                "{} must contain exactly one version byte",
                output_paths.version_path.display()
            ),
        },
        VersionGate::Absent if state_exists => OutputStatusIssue {
            code: "missing_version".to_string(),
            message: format!("Missing version marker: {}", output_paths.version_path.display()),
        },
        // Neither marker nor state: genuinely absent output rather than an invalid tree.
        VersionGate::Absent => {
            return OutputStatus::new(
                OutputStatusKind::Missing,
                status_details(output_paths, validation, vec![missing_state_issue(output_paths)]),
            );
        }
    };
    OutputStatus::new(
        OutputStatusKind::Invalid,
        status_details(output_paths, validation, vec![issue]),
    )
}

/// Outcome of reading the outer version marker, shared by both status entry points.
///
/// Both callers report byte-identical codes and messages for an incompatible version but diverge
/// on every other case, so only the shared classification lives here.
enum VersionGate {
    /// The marker holds exactly the current version; the caller continues to its own state checks.
    Current,
    /// The marker holds a version that cannot be served; report this issue verbatim.
    Incompatible(OutputStatusIssue),
    /// The marker exists but is not exactly one byte.
    Malformed,
    /// No marker file.
    Absent,
}

fn read_version_gate(version_path: &Path) -> VersionGate {
    match fs::read(version_path).ok().as_deref() {
        Some([version]) if *version > MGE_DL_VERSION => VersionGate::Incompatible(OutputStatusIssue {
            code: "future_output_version".to_string(),
            message: format!("unsupported distant-land output version {version}"),
        }),
        Some([MGE_DL_VERSION]) => VersionGate::Current,
        Some([version]) => VersionGate::Incompatible(OutputStatusIssue {
            code: "incompatible_older_version".to_string(),
            message: format!("distant-land output version {version} must be rebuilt for version {MGE_DL_VERSION}"),
        }),
        Some(_) => VersionGate::Malformed,
        None => VersionGate::Absent,
    }
}

fn missing_state_issue(output_paths: &OutputPaths) -> OutputStatusIssue {
    OutputStatusIssue {
        code: "missing_state".to_string(),
        message: format!("Missing generation state: {}", output_paths.generation_state_path.display()),
    }
}

/// Core status check against pre-resolved paths, used when the version marker is malformed or
/// absent.
pub(crate) fn check_output_paths_status(job: &GenerationJob, output_paths: &OutputPaths) -> Result<OutputStatus> {
    if !output_paths.generation_state_path.exists() {
        return Ok(OutputStatus::new(
            OutputStatusKind::Missing,
            status_details(
                output_paths,
                output_paths.validate_mge_xe_contract(),
                vec![missing_state_issue(output_paths)],
            ),
        ));
    }

    // State and its inventoried artifacts are the sole validity authority.
    status_from_routine_snapshot(job, output_paths, ToString::to_string)
}

fn stale_generation_inputs_issue(rebuild_cause: Option<String>) -> OutputStatusIssue {
    OutputStatusIssue {
        code: "stale_generation_inputs".to_string(),
        message: rebuild_cause.map_or_else(
            || "Generation inputs changed".to_string(),
            |cause| format!("Generation inputs changed: {cause}"),
        ),
    }
}

fn status_details(
    output_paths: &OutputPaths,
    validation: OutputContractValidation,
    issues: Vec<OutputStatusIssue>,
) -> OutputStatusDetails {
    OutputStatusDetails {
        output_root: output_paths.output_root.clone(),
        generation_report_path: output_paths.generation_report_path.clone(),
        validation,
        issues,
    }
}
