//! Acceptance coverage for refusing complete output trees from a newer generator.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use distantland::{
    GenerationJob, MGE_DL_VERSION, NullProgressReporter, OutputPaths, OutputStatusKind, check_output_status,
    ensure_generated, generate,
};
use distantland_test_support::{
    BASELINE_WORLD_V1, build_hermetic_fixture, hermetic_generation_job, write_generation_job_file,
};
use tempfile::TempDir;

struct FutureVersionFixture {
    _root: TempDir,
    job: GenerationJob,
    paths: OutputPaths,
}

impl FutureVersionFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let inputs = build_hermetic_fixture(BASELINE_WORLD_V1, &root.path().join("inputs")).unwrap();
        let output_root = root.path().join("output");
        let job = hermetic_generation_job(&inputs, &output_root);
        let paths = OutputPaths::new(&output_root);

        generate(&job, &mut NullProgressReporter).unwrap();
        assert_eq!(check_output_status(&job).unwrap().kind(), OutputStatusKind::Valid);
        assert_eq!(fs::read(&paths.version_path).unwrap(), [MGE_DL_VERSION]);
        assert!(paths.generation_state_path.is_file());
        assert!(paths.writer_lock_path.is_file());
        assert!(paths.static_mesh_shard_paths.iter().all(|path| path.is_file()));
        assert!(paths.terrain_path.is_file());

        Self { _root: root, job, paths }
    }

    fn promote_to_future_version(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let before = snapshot_tree(&self.paths.distantland_dir);
        fs::write(&self.paths.version_path, [MGE_DL_VERSION + 1]).unwrap();
        let after = snapshot_tree(&self.paths.distantland_dir);

        let mut expected = before;
        expected.insert(PathBuf::from("version"), vec![MGE_DL_VERSION + 1]);
        assert_eq!(after, expected, "promoting the tree changed more than the version byte");
        after
    }

    fn assert_refusal_untouched(&self, error: &str, before: &BTreeMap<PathBuf, Vec<u8>>) {
        let expected = format!("unsupported distant-land output version {}", MGE_DL_VERSION + 1);
        assert!(error.contains(&expected), "unexpected error: {error}");
        let after = snapshot_tree(&self.paths.distantland_dir);
        assert_eq!(after, *before, "future-version refusal changed the output tree");
        assert_eq!(
            after.get(Path::new("generation_state.bin")),
            before.get(Path::new("generation_state.bin")),
            "future-version refusal changed generation_state.bin"
        );
        assert_eq!(after.get(Path::new("version")), Some(&vec![MGE_DL_VERSION + 1]));
        assert!(after.contains_key(Path::new("terrain.bin")) || after.contains_key(Path::new("terrain.bin")));
    }
}

#[test]
fn generate_refuses_a_complete_future_tree_without_mutation() {
    let fixture = FutureVersionFixture::new();
    let before = fixture.promote_to_future_version();
    let error = generate(&fixture.job, &mut NullProgressReporter).unwrap_err();
    fixture.assert_refusal_untouched(&format!("{error:#}"), &before);
}

#[test]
fn ensure_generated_refuses_a_complete_future_tree_without_mutation() {
    for force_rebuild in [false, true] {
        let fixture = FutureVersionFixture::new();
        let before = fixture.promote_to_future_version();
        let mut job = fixture.job.clone();
        job.settings.force_rebuild = force_rebuild;
        let error = ensure_generated(&job, &mut NullProgressReporter).unwrap_err();
        fixture.assert_refusal_untouched(&format!("{error:#}"), &before);
    }
}

#[test]
fn cli_refuses_a_complete_future_tree_without_mutation() {
    let fixture = FutureVersionFixture::new();
    let before = fixture.promote_to_future_version();
    let job_path = fixture._root.path().join("future-job.json");
    write_generation_job_file(&job_path, &fixture.job).unwrap();

    let mut command = Command::new(env!("CARGO_BIN_EXE_distantland"));
    command.arg("--job").arg(&job_path).env("RUST_LOG", "off");
    for (name, _) in std::env::vars_os().filter(|(name, _)| name.to_string_lossy().starts_with("TES3_DL_FAULT_")) {
        command.env_remove(name);
    }
    let output = command.output().unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    let expected = format!("unsupported distant-land output version {}", MGE_DL_VERSION + 1);
    assert!(stderr.contains(&expected), "unexpected stderr: {stderr}");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Generated distant land"));
    fixture.assert_refusal_untouched(&stderr, &before);
}

fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, dir: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                files.insert(relative, fs::read(path).unwrap());
            }
        }
    }

    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    assert!(files.keys().any(|path| path.file_name() == Some(OsStr::new(".writer.lock"))));
    files
}
