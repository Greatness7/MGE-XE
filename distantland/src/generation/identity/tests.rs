use std::fs::{self, File, FileTimes};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use tempfile::tempdir;

use super::*;

#[test]
fn content_identity_ignores_mtime_changes() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("source.bin");
    fs::write(&path, b"unchanged").unwrap();
    let before = ContentIdentity::from_bytes(&fs::read(&path).unwrap());

    let file = File::options().write(true).open(&path).unwrap();
    file.set_times(FileTimes::new().set_modified(SystemTime::now() + Duration::from_secs(60)))
        .unwrap();

    let after = ContentIdentity::from_bytes(&fs::read(&path).unwrap());
    assert_eq!(before, after);
}

#[test]
fn content_identity_changes_with_one_byte() {
    let before = ContentIdentity::from_bytes(b"content-a");
    let after = ContentIdentity::from_bytes(b"content-b");
    assert_ne!(before, after);
}

#[test]
fn load_order_identity_has_declared_order_and_content_semantics() {
    let temp = tempdir().unwrap();
    let file = |name: &str, bytes: &[u8]| {
        let path = temp.path().join(name);
        fs::write(&path, bytes).unwrap();
        FileIdentity::from_bytes(&path, bytes)
    };
    let plugin_a = file("A.esp", b"plugin-a");
    let plugin_b = file("B.esp", b"plugin-b");
    let override_a = file("A.ovr", b"override-a");
    let override_b = file("B.ovr", b"override-b");
    let metadata = file("A-metadata.toml", b"invalid but readable");
    let grass_a = file("Grass-A.esp", b"grass-a");
    let grass_b = file("Grass-B.esp", b"grass-b");

    let baseline = load_order_identity(
        &[plugin_a.clone(), plugin_b.clone()],
        &[override_a.clone(), override_b.clone()],
        std::slice::from_ref(&metadata),
        &[grass_a.clone(), grass_b.clone()],
    )
    .unwrap();
    assert_eq!(
        baseline,
        load_order_identity(
            &[plugin_a.clone(), plugin_b.clone()],
            &[override_a.clone(), override_b.clone()],
            std::slice::from_ref(&metadata),
            &[grass_b.clone(), grass_a.clone()],
        )
        .unwrap()
    );
    assert_ne!(
        baseline,
        load_order_identity(
            &[plugin_b.clone(), plugin_a.clone()],
            &[override_a.clone(), override_b.clone()],
            std::slice::from_ref(&metadata),
            &[grass_a.clone(), grass_b.clone()],
        )
        .unwrap()
    );
    assert_ne!(
        baseline,
        load_order_identity(
            &[plugin_a.clone(), plugin_b.clone()],
            &[override_b, override_a],
            std::slice::from_ref(&metadata),
            &[grass_a.clone(), grass_b.clone()],
        )
        .unwrap()
    );
    let edited = file("A-edited.esp", b"plugin-c");
    assert_ne!(
        baseline,
        load_order_identity(&[edited, plugin_b], &[], &[], &[grass_a, grass_b],).unwrap()
    );
    assert_ne!(baseline, load_order_identity(&[plugin_a], &[], &[], &[]).unwrap());
}

#[test]
fn collector_supplies_each_identity_kind_to_its_own_slot() {
    // Guards the collector's field mapping: swapping overrides with metadata, or grass with
    // plugins, would still assemble *an* identity, just not the one the pipeline promises.
    let temp = tempdir().unwrap();
    let file = |name: &str, bytes: &[u8]| {
        let path = temp.path().join(name);
        fs::write(&path, bytes).unwrap();
        FileIdentity::from_bytes(&path, bytes)
    };
    let plugin = file("Fixture.esp", b"plugin");
    let grass = file("Grass.esp", b"grass");
    let override_file = file("Fixture.ovr", b"override");
    let metadata = file("Fixture-metadata.toml", b"readable invalid metadata");

    let mut collector = ContentIdentityCollector::default();
    collector.set_plugins(vec![plugin.clone()]);
    collector.set_grass_plugins(vec![grass.clone()]);
    collector.record_override(override_file.clone());
    collector.record_metadata(metadata.clone());

    assert_eq!(
        collected_load_order_identity(&collector).unwrap(),
        load_order_identity(&[plugin], &[override_file], &[metadata], &[grass]).unwrap()
    );
}

#[test]
fn load_order_identity_ignores_path_spelling() {
    // Override files arrive from job TOML in whatever spelling the user typed, so normalization
    // has to fold separator and case differences before hashing.
    let temp = tempdir().unwrap();
    let path = temp.path().join("Fixture.ovr");
    fs::write(&path, b"override").unwrap();
    let mixed = PathBuf::from(path.to_string_lossy().replace('\\', "/").to_ascii_uppercase());

    assert_eq!(
        load_order_identity(&[], &[FileIdentity::from_bytes(&path, b"override")], &[], &[]).unwrap(),
        load_order_identity(&[], &[FileIdentity::from_bytes(&mixed, b"override")], &[], &[]).unwrap()
    );
}
