use super::*;
use std::fs;

fn map_with(key: &str) -> AssetMap {
    let mut map = AssetMap::default();
    map.insert_normalized(
        key.to_owned(),
        AssetSource::Loose {
            path: PathBuf::from(key),
        },
    );
    map
}

fn nk(text: &str) -> &NormalizedStr {
    NormalizedStr::from_normalized(text)
}

fn touch(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, []).unwrap();
}

#[test]
fn build_directory_map_indexes_top_level_asset_dirs() {
    let dir = tempfile::tempdir().unwrap();
    touch(&dir.path().join("Meshes").join("Foo").join("Bar.NIF"));
    touch(&dir.path().join("textures").join("foo").join("bar.DDS"));

    let maps = build_directory_map(&[dir.path().to_path_buf()]).unwrap();

    assert_eq!(
        maps.meshes.get(nk("foo\\bar.nif")).unwrap().path().unwrap(),
        dir.path().join("Meshes").join("Foo").join("Bar.NIF").as_path()
    );
    assert_eq!(
        maps.textures.get(nk("foo\\bar.dds")).unwrap().path().unwrap(),
        dir.path().join("textures").join("foo").join("bar.DDS").as_path()
    );
}

#[test]
fn build_directory_map_skips_data_root_files() {
    let dir = tempfile::tempdir().unwrap();
    touch(&dir.path().join("Morrowind.esm"));
    touch(&dir.path().join("Loose.nif"));

    let maps = build_directory_map(&[dir.path().to_path_buf()]).unwrap();

    assert!(maps.meshes.get(nk("morrowind.esm")).is_none());
    assert!(maps.meshes.get(nk("loose.nif")).is_none());
    assert!(maps.textures.get(nk("loose.nif")).is_none());
}

#[test]
fn build_directory_map_indexes_hardlinked_file() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("shared").join("foo.nif");
    touch(&source);
    let hardlink = dir.path().join("Meshes").join("linked.nif");
    fs::create_dir_all(hardlink.parent().unwrap()).unwrap();
    fs::hard_link(&source, &hardlink).unwrap();

    let maps = build_directory_map(&[dir.path().to_path_buf()]).unwrap();

    assert_eq!(maps.meshes.get(nk("linked.nif")).unwrap().path().unwrap(), hardlink.as_path());
}

#[test]
fn build_directory_map_preserves_later_data_dir_precedence() {
    let earlier = tempfile::tempdir().unwrap();
    let later = tempfile::tempdir().unwrap();
    let earlier_mesh = earlier.path().join("Meshes").join("foo.nif");
    let later_mesh = later.path().join("Meshes").join("foo.nif");
    touch(&earlier_mesh);
    touch(&later_mesh);

    let maps = build_directory_map(&[earlier.path().to_path_buf(), later.path().to_path_buf()]).unwrap();

    assert_eq!(maps.meshes.get(nk("foo.nif")).unwrap().path().unwrap(), later_mesh.as_path());
}

#[cfg(windows)]
fn create_dir_symlink_or_skip(link: &Path, target: &Path) -> bool {
    use std::os::windows::fs::symlink_dir;

    match symlink_dir(target, link) {
        Ok(()) => true,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied || err.raw_os_error() == Some(1314) => {
            eprintln!("skipping symlink test without Windows symlink privileges: {err}");
            false
        }
        Err(err) => panic!(
            "failed to create directory symlink {} -> {}: {err}",
            link.display(),
            target.display()
        ),
    }
}

#[cfg(windows)]
fn create_file_symlink_or_skip(link: &Path, target: &Path) -> bool {
    use std::os::windows::fs::symlink_file;

    match symlink_file(target, link) {
        Ok(()) => true,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied || err.raw_os_error() == Some(1314) => {
            eprintln!("skipping symlink test without Windows symlink privileges: {err}");
            false
        }
        Err(err) => panic!(
            "failed to create file symlink {} -> {}: {err}",
            link.display(),
            target.display()
        ),
    }
}

#[cfg(windows)]
#[test]
fn build_directory_map_follows_top_level_asset_dir_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("shared-meshes");
    touch(&target.join("foo.nif"));
    let meshes = dir.path().join("Meshes");
    if !create_dir_symlink_or_skip(&meshes, &target) {
        return;
    }

    let maps = build_directory_map(&[dir.path().to_path_buf()]).unwrap();

    assert_eq!(
        maps.meshes.get(nk("foo.nif")).unwrap().path().unwrap(),
        meshes.join("foo.nif").as_path()
    );
}

#[cfg(windows)]
#[test]
fn build_directory_map_follows_nested_asset_dir_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("shared-textures");
    touch(&target.join("foo").join("bar.dds"));
    let textures = dir.path().join("Textures");
    fs::create_dir_all(&textures).unwrap();
    let linked = textures.join("linked");
    if !create_dir_symlink_or_skip(&linked, &target) {
        return;
    }

    let maps = build_directory_map(&[dir.path().to_path_buf()]).unwrap();

    assert_eq!(
        maps.textures.get(nk("linked\\foo\\bar.dds")).unwrap().path().unwrap(),
        linked.join("foo").join("bar.dds").as_path()
    );
}

#[cfg(windows)]
#[test]
fn build_directory_map_indexes_file_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("shared").join("foo.nif");
    touch(&target);
    let meshes = dir.path().join("Meshes");
    fs::create_dir_all(&meshes).unwrap();
    let link = meshes.join("foo.nif");
    if !create_file_symlink_or_skip(&link, &target) {
        return;
    }

    let maps = build_directory_map(&[dir.path().to_path_buf()]).unwrap();

    assert_eq!(maps.meshes.get(nk("foo.nif")).unwrap().path().unwrap(), link.as_path());
}

#[cfg(windows)]
#[test]
fn build_directory_map_keeps_distinct_keys_for_duplicate_physical_targets() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("shared");
    touch(&target.join("foo.nif"));
    let meshes = dir.path().join("Meshes");
    fs::create_dir_all(&meshes).unwrap();
    let alias_a = meshes.join("alias-a");
    let alias_b = meshes.join("alias-b");
    if !create_dir_symlink_or_skip(&alias_a, &target) {
        return;
    }
    if !create_dir_symlink_or_skip(&alias_b, &target) {
        return;
    }

    let maps = build_directory_map(&[dir.path().to_path_buf()]).unwrap();

    assert_eq!(
        maps.meshes.get(nk("alias-a\\foo.nif")).unwrap().path().unwrap(),
        alias_a.join("foo.nif").as_path()
    );
    assert_eq!(
        maps.meshes.get(nk("alias-b\\foo.nif")).unwrap().path().unwrap(),
        alias_b.join("foo.nif").as_path()
    );
}

#[cfg(windows)]
#[test]
fn build_directory_map_fails_on_recursive_directory_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let meshes = dir.path().join("Meshes");
    fs::create_dir_all(&meshes).unwrap();
    let loop_dir = meshes.join("loop");
    if !create_dir_symlink_or_skip(&loop_dir, &meshes) {
        return;
    }

    let error = build_directory_map(&[dir.path().to_path_buf()]).unwrap_err().to_string();

    assert!(error.contains("recursion depth exceeded"));
    assert!(error.contains("loop"));
}

#[test]
fn asset_map_direct_lookup_uses_normalized_query() {
    let map = map_with("foo\\bar.dds");
    let query = NormalizedString::texture_key("Foo/Bar.dds").unwrap();
    let (key, source) = map.get_key_value(&query).unwrap();
    assert_eq!(key, "foo\\bar.dds");
    assert_eq!(source.path().unwrap(), Path::new("foo\\bar.dds"));
}

#[test]
fn asset_map_virtual_extension_lookup() {
    let map = map_with("foo\\bar.dds");
    let (key, _) = map.get_key_value_parts_normalized(&["foo\\bar.", "dds"]).unwrap();
    assert_eq!(key, "foo\\bar.dds");
}

#[test]
fn asset_map_virtual_override_lookup() {
    let map = map_with("a\\b_dist.nif");
    let (key, _) = map.get_key_value_parts_normalized(&["a", "\\", "b", "_dist.nif"]).unwrap();
    assert_eq!(key, "a\\b_dist.nif");
}

#[test]
fn asset_map_virtual_lookup_requires_full_key_match() {
    let map = map_with("foo.dds.extra");
    assert!(map.get_key_value_parts_normalized(&["foo.dds"]).is_none());
}

#[test]
fn asset_map_get_returns_path() {
    let map = map_with("foo\\bar.dds");
    let query = NormalizedString::texture_key("Foo/Bar.dds").unwrap();
    let path = map.get(&query).unwrap().path().unwrap();
    assert_eq!(path, Path::new("foo\\bar.dds"));
}

#[test]
fn asset_map_get_returns_none_for_missing() {
    let map = map_with("foo\\bar.dds");
    assert!(map.get(nk("missing.nif")).is_none());
}

#[test]
fn asset_map_contains_key_parts_true() {
    let map = map_with("a\\b_dist.nif");
    assert!(map.contains_key_parts_normalized(&["a", "\\", "b", "_dist.nif"]));
}

#[test]
fn asset_map_contains_key_parts_false() {
    let map = map_with("a\\b_dist.nif");
    assert!(!map.contains_key_parts_normalized(&["a", "\\", "missing.nif"]));
}

#[test]
#[should_panic]
fn asset_map_contains_key_parts_requires_normalized_input() {
    // contains_key_parts_normalized does NOT normalize - input must already be normalized
    let map = map_with("foo\\bar.dds");
    assert!(map.contains_key_parts_normalized(&["foo", "\\", "bar.dds"]));
    // Mixed case won't match
    assert!(!map.contains_key_parts_normalized(&["Foo", "/", "Bar.dDs"]));
}

#[test]
fn trim_normalized_prefix_trims_matching() {
    // Prefix must already be normalized; text is normalized during comparison
    assert_eq!(trim_normalized_prefix("foo/bar\\baz", "foo\\bar\\"), "baz");
}

#[test]
fn trim_normalized_prefix_no_trim_on_mismatch() {
    assert_eq!(trim_normalized_prefix("foo\\bar", "baz\\"), "foo\\bar");
}

#[test]
fn trim_normalized_prefix_empty_when_full_match() {
    assert_eq!(trim_normalized_prefix("foo\\bar", "foo\\bar"), "");
}

#[test]
fn trim_normalized_prefix_handles_case_differences() {
    // Text is normalized during comparison, prefix is expected to already be normalized
    assert_eq!(trim_normalized_prefix("FOO/BAR", "foo\\bar"), "");
    assert_eq!(trim_normalized_prefix("Foo\\Bar\\baz", "foo\\bar\\"), "baz");
}
