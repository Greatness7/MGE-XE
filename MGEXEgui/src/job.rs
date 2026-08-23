//! Distant-land generation job integration for the owned `mgeXE.toml` namespace.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use distantland::{
    GenerationJob, SUPPORTED_STATIC_ATLAS_SIZES, SUPPORTED_TERRAIN_ATLAS_SIZES, load_generation_job_file_with_warnings,
    serialize_generation_job_document,
};
use rust_i18n::t;

use crate::platform;

/// Output root the host's automatic startup generation expects.
pub const RUNTIME_OUTPUT_ROOT: &str = "Data Files";
/// Default global override source, relative to the Morrowind root.
pub const DEFAULT_OVERRIDE_FILE: &str = "mge3/MGE XE Default Statics Classifiers.toml";

/// Creates the MGE-XE product default. The library's atlas/control-map
/// defaults are already the largest size it supports; the only local adjustment
/// is scaling them down when the detected adapter cannot create a texture that
/// large.
fn default_job() -> GenerationJob {
    let mut job = GenerationJob::default();
    job.settings.override_files = vec![PathBuf::from(DEFAULT_OVERRIDE_FILE)];
    if let Some(max) = platform::max_texture_dimension() {
        job.settings.max_terrain_atlas_size =
            largest_supported_at_most(SUPPORTED_TERRAIN_ATLAS_SIZES, max).unwrap_or(job.settings.max_terrain_atlas_size);
        job.settings.max_static_atlas_size =
            largest_supported_at_most(SUPPORTED_STATIC_ATLAS_SIZES, max).unwrap_or(job.settings.max_static_atlas_size);
        job.settings.max_terrain_control_texture_size = job.settings.max_terrain_control_texture_size.min(max);
    }
    job
}

/// The largest entry of `supported` that does not exceed `max`, or `None` if
/// every option (even the smallest) is over it.
fn largest_supported_at_most(supported: &[u32], max: u32) -> Option<u32> {
    supported.iter().copied().filter(|&size| size <= max).max()
}

/// Result of loading the generator-owned configuration namespace.
pub struct JobLoad {
    /// Loaded job, with local defaults substituted for recoverable problems.
    pub job: GenerationJob,
    /// Localized syntax/read error. Its presence must block TOML saves.
    pub error: Option<String>,
    /// Recoverable schema problems reported while loading.
    pub warnings: Vec<String>,
    /// Whether the obsolete standalone JSON job still exists.
    pub legacy_present: bool,
}

/// Location of the shared MGE-XE configuration document.
pub fn path(root: &Path) -> PathBuf {
    root.join(mge_config::FILE_NAME)
}

/// Location of the obsolete standalone generation job.
pub fn legacy_path(root: &Path) -> PathBuf {
    root.join("MGE3").join("distantland-job.json")
}

/// Reads the embedded job namespace without validating unrelated MGE-XE tables.
pub fn load(root: &Path) -> JobLoad {
    let config_path = path(root);
    let legacy_present = legacy_path(root).is_file();
    if !config_path.is_file() {
        return JobLoad {
            job: default_job(),
            error: None,
            warnings: Vec::new(),
            legacy_present,
        };
    }

    match load_generation_job_file_with_warnings(&config_path) {
        Ok(loaded) => JobLoad {
            job: if loaded.namespace_present { loaded.job } else { default_job() },
            error: None,
            warnings: loaded
                .warnings
                .into_iter()
                .map(|warning| format!("{}: {}", warning.path, warning.message))
                .collect(),
            legacy_present,
        },
        Err(error) => JobLoad {
            job: default_job(),
            error: Some(t!("generator.messages.job_read_failed", error = format!("{error:#}")).into_owned()),
            warnings: Vec::new(),
            legacy_present,
        },
    }
}

/// Serializes `job` in its persistence form as a complete generator-owned TOML namespace.
///
/// # Errors
///
/// Returns an error when the normalized job is invalid or TOML serialization fails.
pub fn serialize_for_persist(job: &GenerationJob) -> Result<String> {
    let mut persisted = job.clone();
    persisted.output_root = Some(PathBuf::from(RUNTIME_OUTPUT_ROOT));
    persisted.settings.force_rebuild = false;
    persisted
        .validate_for_generation()
        .context("Distant-land generation settings are not valid, so they were not saved")?;
    serialize_generation_job_document(&persisted).context("serialize distant-land generation settings")
}

/// Removes the obsolete standalone JSON job after a successful embedded save.
///
/// # Errors
///
/// Returns an error when the legacy file exists but cannot be removed.
pub fn remove_legacy(root: &Path) -> Result<bool> {
    let path = legacy_path(root);
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("remove obsolete {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;

    struct Root(PathBuf);

    impl Root {
        fn new(name: &str) -> Self {
            let unique = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos();
            let root = std::env::temp_dir().join(format!("mge-gui-job-{name}-{unique}"));
            fs::create_dir_all(root.join("MGE3")).unwrap();
            Self(root)
        }
    }

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_namespace_uses_defaults_without_reading_legacy_job() {
        let root = Root::new("missing");
        fs::write(path(&root.0), "schema_version = 1\n").unwrap();
        fs::write(legacy_path(&root.0), "{not json").unwrap();

        let loaded = load(&root.0);
        assert!(loaded.error.is_none());
        assert!(loaded.legacy_present);
        assert!(loaded.job.plugins.is_none());
        assert_eq!(loaded.job.settings.override_files, [PathBuf::from(DEFAULT_OVERRIDE_FILE)]);
    }

    /// The file `DEFAULT_OVERRIDE_FILE` names is shipped to users and parsed by
    /// `distantland`, which has no view of the shipped asset, so nothing there
    /// can catch it going missing or stale against a parser change. Only that is
    /// checked here; what the entries *mean* is covered by that crate's own
    /// tests against inline fixtures.
    #[test]
    fn shipped_default_classifier_toml_parses() {
        use distantland::OverridesBuilder;
        use distantland::statics::metadata::apply_override_source_with_identity;

        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../assets")
            .join(DEFAULT_OVERRIDE_FILE);
        let mut builder = OverridesBuilder::new();
        apply_override_source_with_identity(&path, &mut builder).expect("parse shipped default override TOML");
        let overrides = builder.finish();

        assert!(!overrides.mesh_overrides.is_empty());
        assert!(!overrides.dynamic_vis.groups.is_empty());
    }

    #[test]
    fn owned_table_round_trips_through_shared_config() {
        let root = Root::new("round-trip");
        let job = GenerationJob {
            plugins: Some(vec![PathBuf::from("Morrowind.esm")]),
            // The generator-only grass list rides the same namespace, and shares
            // no filename with `plugins`; `validate_for_generation` rejects that.
            grass_plugins: Some(vec![PathBuf::from("Rem_GL.esp")]),
            ..GenerationJob::default()
        };
        fs::write(path(&root.0), serialize_for_persist(&job).unwrap()).unwrap();

        let loaded = load(&root.0);
        assert!(loaded.error.is_none());
        assert_eq!(loaded.job.plugins, job.plugins);
        assert_eq!(loaded.job.grass_plugins, job.grass_plugins);
        assert_eq!(loaded.job.output_root, Some(PathBuf::from(RUNTIME_OUTPUT_ROOT)));
    }

    #[test]
    fn malformed_owned_value_warns_and_uses_its_default() {
        let root = Root::new("invalid");
        fs::write(
            path(&root.0),
            "[generation]\nversion = 3\n[generation.settings]\nmin_static_size = \"large\"\n",
        )
        .unwrap();

        let loaded = load(&root.0);
        assert!(loaded.error.is_none());
        assert!(!loaded.warnings.is_empty());
        assert_eq!(
            loaded.job.settings.min_static_size,
            distantland::GenerationSettings::default().min_static_size
        );
    }

    #[test]
    fn obsolete_generation_key_warns_without_blocking_saves() {
        let root = Root::new("obsolete-key");
        fs::write(
            path(&root.0),
            "[generation]\nversion = 2\n[generation.settings]\nterrain_mesh_raw_normal_weight = 0.0\ngrass_density = 0.5\n",
        )
        .unwrap();

        let loaded = load(&root.0);

        assert!(loaded.error.is_none());
        assert_eq!(loaded.job.settings.grass_density, 0.5);
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.contains("terrain_mesh_raw_normal_weight"))
        );
        assert!(loaded.warnings.iter().any(|warning| warning.contains("generation.version")));
    }

    #[test]
    fn malformed_toml_is_a_blocking_error() {
        let root = Root::new("invalid-syntax");
        fs::write(path(&root.0), "[generation\nversion = 3\n").unwrap();

        let loaded = load(&root.0);
        assert!(loaded.error.is_some());
        assert!(loaded.warnings.is_empty());
    }

    /// The path that actually runs when the user clicks Generate: the generator serializes its
    /// table, `mge-config` splices it into the live document, and the file is saved.
    #[test]
    fn persisted_table_lands_after_every_existing_table() {
        let root = Root::new("splice-order");
        let config_path = path(&root.0);
        fs::write(&config_path, mge_config::DEFAULT_DOCUMENT).unwrap();

        let job = GenerationJob {
            plugins: Some(vec![PathBuf::from("Morrowind.esm")]),
            ..GenerationJob::default()
        };
        let mut document = mge_config::ConfigDocument::open(&config_path);
        document
            .replace_root_table_from_document(distantland::GENERATION_JOB_NAMESPACE, &serialize_for_persist(&job).unwrap())
            .unwrap();
        document.save().unwrap();

        let written = fs::read_to_string(&config_path).unwrap();
        let headers: Vec<&str> = written.lines().map(str::trim).filter(|line| line.starts_with('[')).collect();
        let first = headers
            .iter()
            .position(|header| header.starts_with("[generation"))
            .expect("generator table is present");
        assert!(
            headers[first..].iter().all(|header| header.starts_with("[generation")),
            "generator table interleaved with MGE-XE tables: {headers:?}"
        );
        assert!(
            written.contains("\n\n[generation]\n"),
            "generator header is not separated from the preceding table: {written}"
        );

        // And it still round-trips back through the generator's own loader.
        assert_eq!(load(&root.0).job.plugins, job.plugins);
    }

    #[test]
    fn successful_cleanup_removes_legacy_job_without_reading_it() {
        let root = Root::new("legacy-cleanup");
        let legacy = legacy_path(&root.0);
        fs::write(&legacy, "{not json").unwrap();

        assert!(remove_legacy(&root.0).unwrap());
        assert!(!legacy.exists());
        assert!(!remove_legacy(&root.0).unwrap());
    }
}
