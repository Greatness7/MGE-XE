use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use distantland::{GenerationJob, MGE_DL_VERSION, OutputPaths};

static COUNTER: AtomicU32 = AtomicU32::new(0);

pub(crate) struct FutureTree {
    root: PathBuf,
    pub(crate) job: GenerationJob,
    pub(crate) paths: OutputPaths,
}

impl FutureTree {
    pub(crate) fn new(name: &str) -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("mgehost_future_{}_{unique}_{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let inputs = distantland_test_support::build_hermetic_fixture(
            distantland_test_support::BASELINE_WORLD_V1,
            &root.join("inputs"),
        )
        .unwrap();
        let output_root = root.join("Data Files");
        let job = distantland_test_support::hermetic_generation_job(&inputs, &output_root);
        distantland::generate(&job, &mut distantland::NullProgressReporter).unwrap();
        let paths = OutputPaths::new(&output_root);
        assert!(paths.writer_lock_path.is_file());
        assert_eq!(fs::read(&paths.version_path).unwrap(), [MGE_DL_VERSION]);

        Self { root, job, paths }
    }

    pub(crate) fn promote(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let before = snapshot_tree(&self.paths.distantland_dir);
        fs::write(&self.paths.version_path, [MGE_DL_VERSION + 1]).unwrap();
        let after = snapshot_tree(&self.paths.distantland_dir);
        let mut expected = before;
        expected.insert(PathBuf::from("version"), vec![MGE_DL_VERSION + 1]);
        assert_eq!(after, expected, "promoting the tree changed more than the version byte");
        after
    }

    pub(crate) fn assert_unchanged(&self, before: &BTreeMap<PathBuf, Vec<u8>>) {
        assert_eq!(snapshot_tree(&self.paths.distantland_dir), *before);
    }
}

impl Drop for FutureTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, dir: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.insert(path.strip_prefix(root).unwrap().to_path_buf(), fs::read(path).unwrap());
            }
        }
    }

    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    assert!(files.contains_key(Path::new(".writer.lock")));
    files
}
