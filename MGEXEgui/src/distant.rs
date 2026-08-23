use std::{fs, path::Path};

use distantland::{MGE_DL_VERSION, OutputPaths, mge_xe::usage_data::load_usage_data};

/// Advisory view of a generated distant-land tree: what the GUI shows in the
/// Distant Land tab, and the source of the generated values that render
/// settings reconcile against.
///
/// This uses the generator's *advisory* contract check rather than its
/// authoritative inventory check. The advisory one needs no `GenerationJob` and
/// takes no writer lock, which is what makes it safe to call from the UI thread;
/// the authoritative check is what the runtime and startup generation use.
///
/// Note the contract does **not** require the terrain package. A tree
/// generated with `generate_terrain = false` is legitimately complete.
#[derive(Clone, Debug, Default)]
pub struct DistantLandStatus {
    pub version: Option<u8>,
    pub supported_version: bool,
    pub complete: bool,
    /// Smallest static size the tree was generated with, read back from the
    /// generated data. Named for the generator's `GenerationSettings` field.
    pub min_static_size: Option<f32>,
    pub missing: Vec<String>,
}

impl DistantLandStatus {
    pub fn inspect(root: &Path) -> Self {
        let paths = OutputPaths::new(root.join("Data Files"));
        let version = read_version(&paths.version_path);
        let validation = paths.validate_mge_xe_contract();

        // `complete` already accounts for a wrong version byte; `missing` stays
        // a pure file list so the UI can report the two failures separately.
        let missing = validation
            .required_files
            .iter()
            .filter(|file| !file.exists)
            .map(|file| file.relative_path.clone())
            .collect();

        let min_static_size = load_usage_data(&paths.usage_data_path)
            .ok()
            .map(|usage| usage.min_static_size);

        Self {
            version,
            supported_version: version == Some(MGE_DL_VERSION),
            complete: validation.complete,
            min_static_size,
            missing,
        }
    }
}

fn read_version(path: &Path) -> Option<u8> {
    let bytes = fs::read(path).ok()?;
    (bytes.len() == 1).then_some(bytes[0])
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf, time::SystemTime};

    use super::*;

    /// Builds a real generated tree rather than a hand-written set of empty
    /// files. The whole point of `inspect` is to agree with what the generator
    /// actually writes, so a mock fixture would only test itself.
    struct GeneratedTree {
        root: PathBuf,
        min_static_size: f32,
    }

    impl GeneratedTree {
        fn new() -> Self {
            let unique = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos();
            let root = std::env::temp_dir().join(format!("mge-gui-dl-{unique}"));
            fs::create_dir_all(&root).unwrap();

            let inputs = distantland_test_support::build_hermetic_fixture(
                distantland_test_support::BASELINE_WORLD_V1,
                &root.join("inputs"),
            )
            .unwrap();
            let output_root = root.join("Data Files");
            let job = distantland_test_support::hermetic_generation_job(&inputs, &output_root);
            distantland::generate(&job, &mut distantland::NullProgressReporter).unwrap();

            Self {
                root,
                min_static_size: job.settings.min_static_size,
            }
        }

        fn distantland(&self) -> PathBuf {
            self.root.join("Data Files").join("distantland")
        }
    }

    impl Drop for GeneratedTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn a_generated_tree_is_complete_and_reports_its_min_static_size() {
        let tree = GeneratedTree::new();
        let status = DistantLandStatus::inspect(&tree.root);

        assert!(status.complete, "missing: {:?}", status.missing);
        assert!(status.missing.is_empty());
        assert_eq!(status.version, Some(MGE_DL_VERSION));
        assert!(status.supported_version);
        assert_eq!(status.min_static_size, Some(tree.min_static_size));
    }

    #[test]
    fn a_removed_shard_is_reported_as_missing() {
        let tree = GeneratedTree::new();
        fs::remove_file(tree.distantland().join("statics").join("static_meshes_017")).unwrap();

        let status = DistantLandStatus::inspect(&tree.root);

        assert!(!status.complete);
        assert!(
            status.missing.iter().any(|item| item.ends_with("static_meshes_017")),
            "missing: {:?}",
            status.missing
        );
    }

    #[test]
    fn a_wrong_version_byte_is_incomplete_without_reporting_missing_files() {
        let tree = GeneratedTree::new();
        fs::write(tree.distantland().join("version"), [MGE_DL_VERSION - 1]).unwrap();

        let status = DistantLandStatus::inspect(&tree.root);

        assert!(!status.complete);
        assert!(!status.supported_version);
        assert_eq!(status.version, Some(MGE_DL_VERSION - 1));
        // Nothing is absent, so the UI reports the format mismatch rather than
        // a file list.
        assert!(status.missing.is_empty(), "missing: {:?}", status.missing);
    }

    #[test]
    fn an_absent_tree_is_incomplete_with_no_version() {
        let root = std::env::temp_dir().join(format!(
            "mge-gui-dl-absent-{}",
            SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let status = DistantLandStatus::inspect(&root);

        assert!(!status.complete);
        assert_eq!(status.version, None);
        assert!(!status.supported_version);
        assert_eq!(status.min_static_size, None);
        assert!(!status.missing.is_empty());
    }
}
