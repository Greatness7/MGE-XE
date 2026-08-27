use std::fs;
use std::path::{Component, Path, PathBuf};

use distantland::{
    GenerationJob, GenerationOutcome, GenerationReport, GenerationStage, OutputStatus, OutputStatusKind, ProgressReporter,
    check_output_status, ensure_generated, load_generation_job_file, resolve_generation_job_paths,
    sync_plugins_from_load_order,
};
use tracing::{error, info, info_span, warn};

use crate::config::Configuration;
use crate::win::NamedMutex;

const STARTUP_JOB_PATH: &str = "mgeXE.toml";
const RUNTIME_OUTPUT_ROOT: &str = "Data Files";
const RUNTIME_MORROWIND_INI: &str = "Morrowind.ini";
const MUTEX_WAIT_CHUNK_MS: u32 = 1000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DistantLandStartupStatus {
    GenerationDisabled,
    JobMissing,
    JobInvalid,
    StatusCheckFailed,
    AlreadyValid,
    Generated,
    GenerationFailedExistingOutputValid,
    GenerationFailedDistantLandDisabled,
}

pub fn ensure_distant_land_ready(configuration: &mut Configuration, morrowind_root: &Path) -> DistantLandStartupStatus {
    if !configuration.distant_land_enabled() {
        info!("Distant-land startup generation skipped because distant land is disabled");
        return DistantLandStartupStatus::GenerationDisabled;
    }

    let job_path = morrowind_root.join(STARTUP_JOB_PATH);

    if !job_path.exists() {
        info!(path = %job_path.display(), "No startup distant-land generation job found");
        return DistantLandStartupStatus::JobMissing;
    }

    let job = match load_startup_job(&job_path, morrowind_root) {
        Ok(job) => job,
        Err(err) => {
            warn!(path = %job_path.display(), error = %err, "Startup distant-land generation job is invalid");
            return DistantLandStartupStatus::JobInvalid;
        }
    };

    info!("Checking distant-land output status");
    let status = match info_span!("check_output_status", phase = "initial").in_scope(|| check_output_status(&job)) {
        Ok(status) => status,
        Err(err) => {
            warn!(error = %format!("{err:#}"), "Distant-land output status check failed; automatic generation is disabled for this session");
            return DistantLandStartupStatus::StatusCheckFailed;
        }
    };
    log_output_status(&status);

    if !status.should_generate() {
        return DistantLandStartupStatus::AlreadyValid;
    }

    let mutex_name = generation_mutex_name(morrowind_root);
    let mutex = match NamedMutex::create(&mutex_name) {
        Ok(mutex) => mutex,
        Err(err) => {
            error!(error = %err, "Failed to create distant-land generation mutex");
            return handle_generation_failure(configuration, &job);
        }
    };

    let guard = match mutex.try_acquire() {
        Ok(Some(guard)) => guard,
        Ok(None) => {
            wait_for_generation_mutex_owner(&mutex);
            info!("Re-checking distant-land output status after startup generation worker finished");
            match info_span!("check_output_status", phase = "after_worker_wait").in_scope(|| check_output_status(&job)) {
                Ok(status) => {
                    log_output_status(&status);
                    if !status.should_generate() {
                        return DistantLandStartupStatus::AlreadyValid;
                    }
                    match mutex.acquire_for(MUTEX_WAIT_CHUNK_MS) {
                        Ok(Some(guard)) => {
                            let result = run_generation(configuration, &job, status);
                            drop(guard);
                            return result;
                        }
                        Ok(None) => {
                            warn!("Startup generation mutex was still owned after worker wait");
                            return DistantLandStartupStatus::StatusCheckFailed;
                        }
                        Err(err) => {
                            error!(error = %err, "Failed to acquire startup generation mutex after worker wait");
                            return handle_generation_failure(configuration, &job);
                        }
                    }
                }
                Err(err) => {
                    warn!(error = %format!("{err:#}"), "Distant-land output status check failed after worker wait");
                    return DistantLandStartupStatus::StatusCheckFailed;
                }
            }
        }
        Err(err) => {
            error!(error = %err, "Failed to acquire distant-land generation mutex");
            return handle_generation_failure(configuration, &job);
        }
    };
    let result = run_generation(configuration, &job, status);
    drop(guard);
    result
}

fn wait_for_generation_mutex_owner(mutex: &NamedMutex) {
    let mut elapsed_seconds = 0u64;
    loop {
        info!(elapsed_seconds, "Waiting for startup distant-land generation worker");
        match mutex.acquire_for(MUTEX_WAIT_CHUNK_MS) {
            Ok(Some(guard)) => {
                drop(guard);
                return;
            }
            Ok(None) => {
                elapsed_seconds += 1;
            }
            Err(err) => {
                error!(error = %err, "Failed while waiting for startup generation mutex");
                return;
            }
        }
    }
}

fn run_generation(configuration: &mut Configuration, job: &GenerationJob, status: OutputStatus) -> DistantLandStartupStatus {
    info!(status = ?status.kind(), "Starting distant-land generation");
    let mut reporter = HostProgressReporter;
    match ensure_generated(job, &mut reporter) {
        Ok(GenerationOutcome::AlreadyValid { status }) => {
            log_output_status(&status);
            DistantLandStartupStatus::AlreadyValid
        }
        Ok(GenerationOutcome::Generated { previous_status, report }) => {
            log_output_status(&previous_status);
            log_generation_report(&report);
            DistantLandStartupStatus::Generated
        }
        Err(err) => {
            error!(error = %format!("{err:#}"), "Distant-land generation failed");
            handle_generation_failure(configuration, job)
        }
    }
}

/// Pin generation to the running install before reading the job's paths. MGEXEgui pins the same
/// path but persists `None`, while the library fallback is registry-only; on a portable or second
/// install, the two processes would generate from different directories and flip the output back
/// and forth on every launch.
///
/// Sync plugins before validation.
fn load_startup_job(path: &Path, morrowind_root: &Path) -> Result<GenerationJob, String> {
    let mut job = load_generation_job_file(path).map_err(|err| format!("{err:#}"))?;
    resolve_generation_job_paths(&mut job, morrowind_root);

    let morrowind_ini = morrowind_root.join(RUNTIME_MORROWIND_INI);
    job.morrowind_ini = Some(morrowind_ini.clone());

    if job.auto_sync_plugins {
        let data_dirs = job.data_dirs.clone();
        sync_plugins_from_load_order(&mut job, &morrowind_ini, data_dirs.as_deref()).map_err(|err| format!("{err:#}"))?;
        info!(
            plugins = job.plugins.as_ref().map_or(0, Vec::len),
            "Synced the distant-land plugin list from the live load order"
        );
    }

    validate_startup_job(&job, morrowind_root)?;
    Ok(job)
}

fn validate_startup_job(job: &GenerationJob, morrowind_root: &Path) -> Result<(), String> {
    job.validate_for_generation().map_err(|err| format!("{err:#}"))?;
    // The host must not guess generation inputs.
    if job.plugins.is_none() {
        return Err("plugins is required for host startup generation".to_string());
    }
    let output_root = job
        .output_root
        .as_deref()
        .ok_or_else(|| "output_root is required for host startup generation".to_string())?;
    let runtime_output_root = morrowind_root.join(RUNTIME_OUTPUT_ROOT);
    if !paths_equivalent(output_root, &runtime_output_root) {
        return Err(format!(
            "output_root {} does not match runtime output root {}",
            output_root.display(),
            runtime_output_root.display()
        ));
    }

    Ok(())
}

fn handle_generation_failure(configuration: &mut Configuration, job: &GenerationJob) -> DistantLandStartupStatus {
    match info_span!("check_output_status", phase = "post_failure").in_scope(|| check_output_status(job)) {
        Ok(status) => {
            log_output_status(&status);
            if matches!(status.kind(), OutputStatusKind::Valid) {
                info!("Keeping distant land enabled because final output still validates");
                DistantLandStartupStatus::GenerationFailedExistingOutputValid
            } else {
                // Never advertise distant land with unusable runtime files.
                configuration.disable_distant_land();
                warn!("Distant land is disabled for this host session because no valid final output is available");
                DistantLandStartupStatus::GenerationFailedDistantLandDisabled
            }
        }
        Err(err) => {
            error!(error = %format!("{err:#}"), "Final distant-land output status check failed");
            configuration.disable_distant_land();
            warn!("Distant land is disabled for this host session because final output could not be validated");
            DistantLandStartupStatus::GenerationFailedDistantLandDisabled
        }
    }
}

fn log_output_status(status: &OutputStatus) {
    let details = status.details();
    match status.kind() {
        OutputStatusKind::Valid => {
            info!(
                output_root = %details.output_root.display(),
                generation_report_path = %details.generation_report_path.display(),
                "Distant-land output is valid"
            );
        }
        kind => {
            warn!(
                status = ?kind,
                output_root = %details.output_root.display(),
                generation_report_path = %details.generation_report_path.display(),
                issues = %status.format_issues(),
                "Distant-land output is not valid"
            );
            for issue in &details.issues {
                warn!(code = %issue.code, message = %issue.message, "Distant-land output issue");
            }
        }
    }
}

fn log_generation_report(report: &GenerationReport) {
    info!(
        output_root = %report.output_root.display(),
        generation_report_path = %report.report_path.display(),
        "Distant-land generation completed"
    );
    for warning in &report.warnings {
        warn!(code = %warning.code, message = %warning.message, "Distant-land generation warning");
    }
}

fn paths_equivalent(left: &Path, right: &Path) -> bool {
    let left = comparable_path(left);
    let right = comparable_path(right);
    paths_equal(&left, &right)
}

fn comparable_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| normalize_path(path))
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(component.as_os_str());
                }
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

#[cfg(windows)]
fn paths_equal(left: &Path, right: &Path) -> bool {
    left.to_string_lossy().eq_ignore_ascii_case(&right.to_string_lossy())
}

#[cfg(not(windows))]
fn paths_equal(left: &Path, right: &Path) -> bool {
    left == right
}

fn generation_mutex_name(morrowind_root: &Path) -> String {
    let root = comparable_path(morrowind_root).to_string_lossy().to_ascii_lowercase();
    format!("Local\\MGE_XE_DistantLandStartupGeneration_{:016x}", fnv1a64(root.as_bytes()))
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

struct HostProgressReporter;

impl ProgressReporter for HostProgressReporter {
    fn begin_stage(&mut self, stage: GenerationStage) {
        info!(stage = ?stage, "Distant-land generation stage started");
    }

    fn finish_stage(&mut self, stage: GenerationStage, _elapsed_ms: f64) {
        info!(stage = ?stage, "Distant-land generation stage finished");
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::abi::USE_DISTANT_LAND;
    use crate::test_support::FutureTree;

    struct TestRoot {
        path: PathBuf,
    }

    impl TestRoot {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let path = env::temp_dir().join(format!("mgehost64_{name}_{}_{}", std::process::id(), nanos));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn enabled_configuration() -> Configuration {
        Configuration {
            mge_flags: USE_DISTANT_LAND,
            ..Configuration::default()
        }
    }

    #[test]
    fn disabled_distant_land_skips_generation_before_file_checks() {
        let mut configuration = Configuration::default();

        let root = TestRoot::new("disabled_distant_land");
        let status = ensure_distant_land_ready(&mut configuration, root.path());

        assert_eq!(status, DistantLandStartupStatus::GenerationDisabled);
        assert!(!configuration.distant_land_enabled());
    }

    #[test]
    fn missing_startup_job_skips_generation() {
        let root = TestRoot::new("missing_startup_job");
        let mut configuration = enabled_configuration();

        let status = ensure_distant_land_ready(&mut configuration, root.path());

        assert_eq!(status, DistantLandStartupStatus::JobMissing);
        assert!(configuration.distant_land_enabled());
    }

    #[test]
    fn invalid_startup_job_skips_generation() {
        let root = TestRoot::new("invalid_startup_job");
        fs::write(root.path().join("mgeXE.toml"), b"[distantland\n").unwrap();
        let mut configuration = enabled_configuration();

        let status = ensure_distant_land_ready(&mut configuration, root.path());

        assert_eq!(status, DistantLandStartupStatus::JobInvalid);
        assert!(configuration.distant_land_enabled());
    }

    #[test]
    fn configuration_helpers_read_and_clear_distant_land_flag() {
        let mut configuration = enabled_configuration();

        assert!(configuration.distant_land_enabled());

        configuration.disable_distant_land();

        assert!(!configuration.distant_land_enabled());
    }

    /// Job fixture with an explicit plugin list.
    fn job_with_plugins() -> GenerationJob {
        GenerationJob {
            plugins: Some(vec![PathBuf::from("Morrowind.esm")]),
            auto_sync_plugins: false,
            ..GenerationJob::default()
        }
    }

    /// Creates real plugin files and a `[Game Files]` load order.
    fn stub_install(root: &Path, plugins: &[&str], load_order: &[&str]) {
        let data_files = root.join("Data Files");
        fs::create_dir_all(&data_files).unwrap();
        for name in plugins {
            fs::write(data_files.join(name), b"plugin").unwrap();
        }
        let mut ini = String::from("[Game Files]\n");
        for (index, name) in load_order.iter().enumerate() {
            ini.push_str(&format!("GameFile{index}={name}\n"));
        }
        fs::write(root.join("Morrowind.ini"), ini).unwrap();
    }

    #[test]
    fn validate_startup_job_requires_a_plugin_list() {
        let job = GenerationJob {
            output_root: Some(PathBuf::from(r"C:\Morrowind\Data Files")),
            auto_sync_plugins: false,
            ..GenerationJob::default()
        };

        let error = validate_startup_job(&job, Path::new(r"C:\Morrowind")).unwrap_err();

        // This requires a hand-edited job with sync disabled and no plugin list.
        assert!(error.contains("plugins is required"));
    }

    #[test]
    fn load_startup_job_syncs_the_plugin_list_from_the_live_load_order() {
        let root = TestRoot::new("startup_job_sync");
        stub_install(
            root.path(),
            &["Morrowind.esm", "Rem_GL.esp", "Installed.esp"],
            &["Morrowind.esm", "Rem_GL.esp", "Installed.esp"],
        );

        // The saved list omits installed plugins.
        let saved = GenerationJob {
            plugins: Some(vec![root.path().join("Data Files").join("Morrowind.esm")]),
            grass_plugins: Some(vec![PathBuf::from("Rem_GL.esp")]),
            output_root: Some(PathBuf::from("Data Files")),
            auto_sync_plugins: true,
            ..GenerationJob::default()
        };
        let job_path = root.path().join("mgeXE.toml");
        fs::write(&job_path, distantland::serialize_generation_job_document(&saved).unwrap()).unwrap();

        let job = load_startup_job(&job_path, root.path()).unwrap();

        let data_files = root.path().join("Data Files");
        assert_eq!(
            job.plugins,
            Some(vec![data_files.join("Morrowind.esm"), data_files.join("Installed.esp"),])
        );
        // Generation is pinned to the running install.
        assert_eq!(job.morrowind_ini, Some(root.path().join("Morrowind.ini")));
    }

    #[test]
    fn load_startup_job_rejects_a_sync_that_resolves_to_nothing() {
        let root = TestRoot::new("startup_job_sync_empty");
        stub_install(root.path(), &["Morrowind.esm"], &[]);

        let saved = GenerationJob {
            plugins: Some(vec![PathBuf::from("Morrowind.esm")]),
            output_root: Some(PathBuf::from("Data Files")),
            auto_sync_plugins: true,
            ..GenerationJob::default()
        };
        let job_path = root.path().join("mgeXE.toml");
        fs::write(&job_path, distantland::serialize_generation_job_document(&saved).unwrap()).unwrap();

        // Sync must reject the derived empty list before generation.
        let error = load_startup_job(&job_path, root.path()).unwrap_err();
        assert!(error.contains("plugins must not be empty"), "{error}");
    }

    #[test]
    fn validate_startup_job_requires_output_root() {
        let job = job_with_plugins();

        let error = validate_startup_job(&job, Path::new(r"C:\Morrowind")).unwrap_err();

        assert!(error.contains("output_root is required"));
    }

    #[test]
    fn validate_startup_job_accepts_runtime_output_root() {
        let root = Path::new(r"C:\Morrowind");
        let mut job = GenerationJob {
            output_root: Some(PathBuf::from("Data Files")),
            ..job_with_plugins()
        };
        resolve_generation_job_paths(&mut job, root);

        validate_startup_job(&job, root).unwrap();
    }

    #[test]
    fn load_startup_job_accepts_expanded_mge_xe_contract_fields() {
        let root = TestRoot::new("expanded_startup_job_contract");
        let data_files = root.path().join("Data Files");
        let extra_data_files = root.path().join("Extra Data Files");
        fs::create_dir_all(&data_files).unwrap();
        fs::create_dir_all(&extra_data_files).unwrap();

        let morrowind_ini = root.path().join("Morrowind.ini");
        fs::write(&morrowind_ini, "[Game Files]\n").unwrap();
        let selected_plugin = extra_data_files.join("Selected.esp");
        fs::write(&selected_plugin, "").unwrap();

        let job_path = root.path().join("mgeXE.toml");
        let mut configured = GenerationJob {
            morrowind_ini: Some(morrowind_ini),
            plugins: Some(vec![selected_plugin.clone()]),
            data_dirs: Some(vec![data_files.clone(), extra_data_files.clone()]),
            output_root: Some(PathBuf::from("Data Files")),
            // An explicit list must survive loading without sync.
            auto_sync_plugins: false,
            ..GenerationJob::default()
        };
        configured.settings.grass_density = 0.5;
        let mut document = distantland::serialize_generation_job_document(&configured).unwrap();
        document.push_str("\n[render]\nfov = 75.0\n");
        fs::write(&job_path, document).unwrap();

        let job = load_startup_job(&job_path, root.path()).unwrap();

        assert_eq!(job.plugins, Some(vec![selected_plugin]));
        assert_eq!(job.data_dirs, Some(vec![data_files, extra_data_files]));
        assert_eq!(job.settings.grass_density, 0.5);
        assert_eq!(job.output_root, Some(root.path().join("Data Files")));
    }

    /// Builds a committed tree for storage-authority checks.
    ///
    /// Deliberately a real committed tree rather than a stub status lookup: what the policy
    /// turns on is what the storage authority says about committed state and inventory hashes.
    fn generated_tree(root: &TestRoot) -> GenerationJob {
        let inputs = distantland_test_support::build_hermetic_fixture(
            distantland_test_support::BASELINE_WORLD_V1,
            &root.path().join("inputs"),
        )
        .unwrap();
        let job = distantland_test_support::hermetic_generation_job(&inputs, &root.path().join("Data Files"));
        distantland::generate(&job, &mut distantland::NullProgressReporter).unwrap();
        job
    }

    /// A failed regeneration preserves a valid existing tree.
    #[test]
    fn a_failed_generation_keeps_distant_land_enabled_when_the_committed_tree_still_validates() {
        let root = TestRoot::new("failed_existing_valid");
        let job = generated_tree(&root);
        let mut configuration = enabled_configuration();

        let status = handle_generation_failure(&mut configuration, &job);

        assert_eq!(status, DistantLandStartupStatus::GenerationFailedExistingOutputValid);
        assert!(
            configuration.distant_land_enabled(),
            "a valid committed tree must keep distant land enabled",
        );
    }

    /// An unusable tree disables distant land for the session.
    #[test]
    fn a_failed_generation_disables_distant_land_when_the_committed_tree_is_damaged() {
        let root = TestRoot::new("failed_damaged");
        let job = generated_tree(&root);
        let usage = root
            .path()
            .join("Data Files")
            .join("distantland")
            .join("statics")
            .join("usage.data");
        assert!(usage.is_file(), "the fixture must have committed a usage table to damage");
        fs::write(&usage, b"").unwrap();

        let mut configuration = enabled_configuration();
        let status = handle_generation_failure(&mut configuration, &job);

        assert_eq!(status, DistantLandStartupStatus::GenerationFailedDistantLandDisabled);
        assert!(
            !configuration.distant_land_enabled(),
            "an unusable tree must disable distant land for the session",
        );
    }

    /// No output leaves distant land disabled.
    #[test]
    fn a_failed_generation_disables_distant_land_when_there_is_no_output() {
        let root = TestRoot::new("failed_no_output");
        let inputs = distantland_test_support::build_hermetic_fixture(
            distantland_test_support::BASELINE_WORLD_V1,
            &root.path().join("inputs"),
        )
        .unwrap();
        // The job addresses an output root that was never generated into.
        let job = distantland_test_support::hermetic_generation_job(&inputs, &root.path().join("Data Files"));

        let mut configuration = enabled_configuration();
        let status = handle_generation_failure(&mut configuration, &job);

        assert_eq!(status, DistantLandStartupStatus::GenerationFailedDistantLandDisabled);
        assert!(!configuration.distant_land_enabled());
    }

    #[test]
    fn startup_generation_refuses_a_complete_future_tree_without_mutation() {
        let fixture = FutureTree::new("startup_generation");
        let before = fixture.promote();
        let input_status = check_output_status(&fixture.job).unwrap();
        assert_eq!(input_status.kind(), OutputStatusKind::Invalid);
        let mut configuration = enabled_configuration();

        let status = run_generation(&mut configuration, &fixture.job, input_status);

        assert_eq!(status, DistantLandStartupStatus::GenerationFailedDistantLandDisabled);
        assert!(!configuration.distant_land_enabled());
        fixture.assert_unchanged(&before);
    }

    #[test]
    fn validate_startup_job_rejects_output_root_mismatch() {
        let root = Path::new(r"C:\Morrowind");
        let mut job = GenerationJob {
            output_root: Some(PathBuf::from("Other Files")),
            ..job_with_plugins()
        };
        resolve_generation_job_paths(&mut job, root);

        let error = validate_startup_job(&job, root).unwrap_err();

        assert!(error.contains("does not match runtime output root"));
    }
}
