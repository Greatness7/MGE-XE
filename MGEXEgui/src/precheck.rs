//! Classification of an existing live distant-land tree, run when the generator
//! window opens.
//!
//! A complete tree at the current format version can be reused as a cache.
//! Anything else needs consent; a tree from a *newer* format must never be
//! replaced. Nothing here mutates the tree.

use std::time::Duration;

use distantland::output_index::{OutputValidation, open_output_snapshot};
use distantland::{MGE_DL_VERSION, OutputPaths};

/// Prompt text for a tree that is incomplete, corrupt, or from an older MGE.
pub const MSG_OLD_OR_CORRUPT: &str = "generator.precheck.old_or_corrupt";

/// Prompt text for a complete tree that announces a different MGE version.
pub const MSG_DIFFERENT_VERSION: &str = "generator.precheck.different_version";

/// Refusal text for a tree written by a newer format than this build knows.
pub const MSG_NEWER_FORMAT: &str = "generator.precheck.newer_format";

/// How the existing `Data Files\distantland` tree must be treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExistingTree {
    /// No tree, or a complete tree at the current version. Safe to reuse as cache.
    Reuse,
    /// Tree present but incomplete/corrupt. Consent needed, "old/corrupt" wording.
    PromptOldCorrupt,
    /// Complete tree from a different MGE version. Consent needed, "different" wording.
    PromptDifferent,
    /// Tree announces a newer format and must not be replaced by this writer.
    RefuseNewer,
}

/// Inspect the live tree under `<root>\Data Files` and decide how to treat it.
///
/// The snapshot open takes the output lock briefly (2 s timeout), so this runs
/// once when the window opens rather than per frame. A held lock means Morrowind is
/// running. Treat that as "not reusable", which is the safe answer: the run itself
/// re-checks the lock and refuses with a clearer message.
pub fn classify(paths: &OutputPaths) -> ExistingTree {
    if !paths.distantland_dir.exists() {
        return ExistingTree::Reuse;
    }
    let version = std::fs::read(&paths.version_path)
        .ok()
        .and_then(|bytes| (bytes.len() == 1).then_some(bytes[0]));
    if version.is_some_and(|version| version > MGE_DL_VERSION) {
        return ExistingTree::RefuseNewer;
    }
    if version == Some(MGE_DL_VERSION) {
        return if open_output_snapshot(&paths.output_root, Duration::from_secs(2), OutputValidation::Routine).is_ok() {
            ExistingTree::Reuse
        } else {
            ExistingTree::PromptOldCorrupt
        };
    }
    // What is left announces a superseded version, or carries no readable version
    // at all. It is never reused as a cache; the user consents to a clean rebuild
    // that replaces it. A complete old tree gets the "different version" wording,
    // a partial one "old/corrupt".
    if has_complete_terrain_contract(paths) {
        ExistingTree::PromptDifferent
    } else {
        ExistingTree::PromptOldCorrupt
    }
}

/// The seven files a complete legacy terrain tree contains. Used only
/// to pick the wording for a tree this build cannot read anyway, so it stays a
/// plain file check rather than a contract validation.
fn has_complete_terrain_contract(paths: &OutputPaths) -> bool {
    paths.version_path.exists()
        && paths.terrain_path.exists()
        && paths.terrain_atlas_path.exists()
        && paths.terrain_material_path.exists()
        && paths.terrain_material_flags_path.exists()
        && paths.terrain_patch_albedo_path.exists()
        && paths.terrain_blend_patterns_path.exists()
}

#[cfg(test)]
mod tests {
    //! Temp-dir tests for the pure classification: absent, incomplete,
    //! complete-but-superseded, and newer-format trees. A complete *current*
    //! tree is covered by `distant.rs`'s generated-fixture tests, which build a
    //! real one rather than a hand-written set of empty files.

    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// The version byte written by the superseded staged-tree format.
    const LEGACY_VERSION: u8 = 12;

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    struct TempTree {
        data_files: PathBuf,
    }

    impl TempTree {
        fn new(name: &str) -> Self {
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let data_files = std::env::temp_dir().join(format!("mge-gui-precheck-{}-{unique}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&data_files);
            fs::create_dir_all(&data_files).unwrap();
            Self { data_files }
        }

        fn paths(&self) -> OutputPaths {
            OutputPaths::new(&self.data_files)
        }

        /// Creates `distantland\` with only the version file in it.
        fn write_version(&self, version: u8) -> OutputPaths {
            let paths = self.paths();
            paths.ensure_parent_dirs().unwrap();
            fs::write(&paths.version_path, [version]).unwrap();
            paths
        }

        /// Writes all seven terrain-contract files at `version`.
        fn write_terrain_contract(&self, version: u8) -> OutputPaths {
            let paths = self.write_version(version);
            for path in [
                &paths.terrain_path,
                &paths.terrain_atlas_path,
                &paths.terrain_material_path,
                &paths.terrain_material_flags_path,
                &paths.terrain_patch_albedo_path,
                &paths.terrain_blend_patterns_path,
            ] {
                fs::write(path, b"x").unwrap();
            }
            paths
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.data_files);
        }
    }

    #[test]
    fn an_absent_tree_opens_straight_into_editing() {
        let tree = TempTree::new("absent");
        assert_eq!(classify(&tree.paths()), ExistingTree::Reuse);
    }

    #[test]
    fn an_incomplete_current_tree_prompts_as_old_or_corrupt() {
        let tree = TempTree::new("incomplete");
        let paths = tree.write_version(MGE_DL_VERSION);
        assert_eq!(classify(&paths), ExistingTree::PromptOldCorrupt);
    }

    #[test]
    fn a_complete_superseded_tree_prompts_as_a_different_version() {
        let tree = TempTree::new("legacy");
        let paths = tree.write_terrain_contract(LEGACY_VERSION);
        assert_eq!(classify(&paths), ExistingTree::PromptDifferent);
    }

    #[test]
    fn a_partial_superseded_tree_prompts_as_old_or_corrupt() {
        let tree = TempTree::new("partial-legacy");
        let paths = tree.write_version(LEGACY_VERSION);
        assert_eq!(classify(&paths), ExistingTree::PromptOldCorrupt);
    }

    #[test]
    fn a_newer_format_tree_is_refused() {
        let tree = TempTree::new("newer");
        let paths = tree.write_terrain_contract(MGE_DL_VERSION + 1);
        assert_eq!(classify(&paths), ExistingTree::RefuseNewer);
    }

    /// Regression guard: classification is a read. Whatever the answer, the
    /// user's tree is untouched, and declining the prompt leaves it byte-identical.
    #[test]
    fn classification_never_mutates_the_tree() {
        let tree = TempTree::new("no-mutate");
        let paths = tree.write_terrain_contract(LEGACY_VERSION);
        let before = snapshot(&paths.distantland_dir);

        assert_eq!(classify(&paths), ExistingTree::PromptDifferent);

        assert_eq!(snapshot(&paths.distantland_dir), before);
    }

    /// Every file under `root` as relative path → content, for byte comparison.
    fn snapshot(root: &std::path::Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        let mut files = std::collections::BTreeMap::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(directory) = stack.pop() {
            for entry in fs::read_dir(&directory).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let key = path.strip_prefix(root).unwrap().to_path_buf();
                    files.insert(key, fs::read(&path).unwrap());
                }
            }
        }
        files
    }
}
