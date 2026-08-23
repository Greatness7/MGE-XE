use std::path::Path;

use super::*;

fn file(path: &str, bytes: &[u8]) -> FileIdentity {
    FileIdentity::from_bytes(Path::new(path), bytes)
}

#[test]
fn load_order_identity_is_deterministic_and_plugin_order_sensitive() {
    let plugins = [file("Morrowind.esm", b"morrowind"), file("Tribunal.esm", b"tribunal")];

    let first = load_order_identity(&plugins, &[], &[], &[]).unwrap();
    let repeated = load_order_identity(&plugins, &[], &[], &[]).unwrap();
    let reversed = load_order_identity(&[plugins[1].clone(), plugins[0].clone()], &[], &[], &[]).unwrap();

    assert_eq!(first, repeated);
    assert_ne!(first, reversed);
}

#[test]
fn grass_order_is_normalized_but_content_remains_sensitive() {
    let grass = [file("B.esp", b"b"), file("A.esp", b"a")];
    let reordered = [grass[1].clone(), grass[0].clone()];
    let changed = [grass[1].clone(), file("B.esp", b"changed")];

    let baseline = load_order_identity(&[], &[], &[], &grass).unwrap();
    assert_eq!(baseline, load_order_identity(&[], &[], &[], &reordered).unwrap());
    assert_ne!(baseline, load_order_identity(&[], &[], &[], &changed).unwrap());
}
