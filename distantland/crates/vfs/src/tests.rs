use super::*;
use std::fs;
use std::thread;
use std::time::Duration;
use tes3::bsa::Builder;

fn touch(path: &Path) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, []).unwrap();
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

fn write_ini(root: &Path, contents: &str) -> PathBuf {
    fs::create_dir_all(root.join("Data Files")).unwrap();
    let ini_path = root.join("Morrowind.ini");
    fs::write(&ini_path, contents).unwrap();
    ini_path
}

fn make_vfs(dir: &Path) -> Vfs {
    let meshes_dir = dir.join("meshes");
    fs::create_dir_all(&meshes_dir).unwrap();
    let maps = build_directory_map(&[dir.to_path_buf()]).unwrap();
    Vfs {
        ini_path: dir.join("Morrowind.ini"),
        data_dirs: vec![dir.to_path_buf()],
        active_plugins: vec![],
        archives: vec![],
        maps,
    }
}

fn make_vfs_with_archives(dir: &Path, archives: &[PathBuf]) -> Vfs {
    let archives = load_bsa_archives(archives);
    let maps = build_vfs_maps(&[dir.to_path_buf()], &archives).unwrap();
    Vfs {
        ini_path: dir.join("Morrowind.ini"),
        data_dirs: vec![dir.to_path_buf()],
        active_plugins: vec![],
        archives,
        maps,
    }
}

fn write_bsa(path: &Path, entries: &[(&str, &[u8])]) {
    let mut builder = Builder::new();
    for &(name, bytes) in entries {
        builder.insert(name, bytes).unwrap();
    }
    builder.save_path(path).unwrap();
}

fn wait_for_distinct_mtime() {
    thread::sleep(Duration::from_millis(1100));
}

#[test]
fn load_uses_explicit_data_dirs_and_later_loose_files_override_earlier() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("Primary Data Files");
    let second = dir.path().join("Extra Data Files");
    fs::create_dir_all(first.join("meshes")).unwrap();
    fs::create_dir_all(second.join("meshes")).unwrap();
    fs::write(first.join("meshes").join("foo.nif"), b"first").unwrap();
    fs::write(second.join("meshes").join("foo.nif"), b"second").unwrap();

    let ini_path = write_ini(dir.path(), "");
    let options = VfsLoadOptions {
        morrowind_ini: Some(ini_path),
        data_dirs: Some(vec![first.clone(), second.clone()]),
        plugins: None,
    };

    let vfs = Vfs::load(&options).unwrap();
    let mesh = vfs.resolve_mesh("foo.nif").unwrap();

    assert_eq!(
        vfs.data_dirs(),
        &[first.canonicalize().unwrap(), second.canonicalize().unwrap()]
    );
    assert_eq!(vfs.read_asset_bytes(&mesh).unwrap().as_ref(), b"second");
}

#[test]
fn load_preserves_explicit_plugin_order() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("First Data Files");
    let second = dir.path().join("Second Data Files");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&second).unwrap();
    fs::write(first.join("First.esm"), []).unwrap();
    fs::write(second.join("Second.esp"), []).unwrap();

    let ini_path = write_ini(dir.path(), "");
    let options = VfsLoadOptions {
        morrowind_ini: Some(ini_path),
        data_dirs: Some(vec![first.clone(), second.clone()]),
        plugins: Some(vec![second.join("Second.esp"), first.join("First.esm")]),
    };

    let vfs = Vfs::load(&options).unwrap();

    assert_eq!(vfs.active_plugins(), &[second.join("Second.esp"), first.join("First.esm")]);
}

#[test]
fn load_metadata_only_skips_asset_maps_but_keeps_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("Data Files");
    fs::create_dir_all(data_dir.join("Meshes")).unwrap();
    fs::create_dir_all(data_dir.join("Textures")).unwrap();
    fs::write(data_dir.join("Meshes").join("foo.nif"), []).unwrap();
    fs::write(data_dir.join("Textures").join("foo.dds"), []).unwrap();
    fs::write(data_dir.join("Morrowind.esm"), []).unwrap();

    let ini_path = write_ini(dir.path(), "");
    let options = VfsLoadOptions {
        morrowind_ini: Some(ini_path),
        data_dirs: Some(vec![data_dir.clone()]),
        plugins: Some(vec![PathBuf::from("Morrowind.esm")]),
    };

    let vfs = Vfs::load_metadata_only(&options).unwrap();

    let canonical_data_dir = data_dir.canonicalize().unwrap();
    assert_eq!(vfs.data_dir(), canonical_data_dir.as_path());
    assert_eq!(vfs.active_plugins(), &[canonical_data_dir.join("Morrowind.esm")]);
    assert_eq!(vfs.maps.meshes.len(), 0);
    assert_eq!(vfs.maps.textures.len(), 0);
}

#[test]
fn load_resolves_bare_plugin_names_from_highest_priority_data_dir() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("First Data Files");
    let second = dir.path().join("Second Data Files");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&second).unwrap();
    fs::write(first.join("Shared.esp"), []).unwrap();
    fs::write(second.join("Shared.esp"), []).unwrap();

    let ini_path = write_ini(dir.path(), "");
    let options = VfsLoadOptions {
        morrowind_ini: Some(ini_path),
        data_dirs: Some(vec![first.clone(), second.clone()]),
        plugins: Some(vec![PathBuf::from("Shared.esp")]),
    };

    let vfs = Vfs::load(&options).unwrap();

    assert_eq!(vfs.active_plugins(), &[second.canonicalize().unwrap().join("Shared.esp")]);
}

#[test]
fn load_errors_when_selected_plugin_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("Data Files Override");
    fs::create_dir_all(&data_dir).unwrap();

    let ini_path = write_ini(dir.path(), "");
    let options = VfsLoadOptions {
        morrowind_ini: Some(ini_path),
        data_dirs: Some(vec![data_dir]),
        plugins: Some(vec![PathBuf::from("Missing.esp")]),
    };

    let error = Vfs::load(&options).err().unwrap().to_string();
    assert!(error.contains("Missing.esp"));
}

#[test]
fn load_resolves_archives_through_active_data_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("First Data Files");
    let second = dir.path().join("Second Data Files");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&second).unwrap();
    // Decoy: the INI below lists only Tribunal.bsa, so this one is never opened. It needs a
    // file because a zero-entry archive cannot be written -- the engine discards those too.
    write_bsa(&first.join("Morrowind.bsa"), &[("meshes\\unused.nif", b"decoy")]);
    write_bsa(&second.join("Tribunal.bsa"), &[("meshes\\foo.nif", b"from-extra")]);

    let ini_path = write_ini(
        dir.path(),
        "\
[Archives]
Archive 0=Tribunal.bsa
",
    );
    let options = VfsLoadOptions {
        morrowind_ini: Some(ini_path),
        data_dirs: Some(vec![first, second]),
        plugins: None,
    };

    let vfs = Vfs::load(&options).unwrap();
    let mesh = vfs.resolve_mesh("foo.nif").unwrap();

    assert_eq!(vfs.read_asset_bytes(&mesh).unwrap().as_ref(), b"from-extra");
}

#[test]
fn resolve_mesh_path_no_overrides() {
    let dir = tempfile::tempdir().unwrap();
    touch(&dir.path().join("meshes").join("foo.nif"));
    let vfs = make_vfs(dir.path());
    let asset = vfs.resolve_mesh("meshes\\foo.nif").unwrap();
    assert_eq!(asset.mesh_resolution_rule, Some(MeshResolutionRule::Original));
    let result = vfs.resolve_mesh_path("meshes\\foo.nif").unwrap();
    assert!(result.ends_with("foo.nif"));
}

#[test]
fn resolve_mesh_path_dist_wins() {
    let dir = tempfile::tempdir().unwrap();
    touch(&dir.path().join("meshes").join("foo.nif"));
    touch(&dir.path().join("meshes").join("foo_dist.nif"));
    let vfs = make_vfs(dir.path());
    let asset = vfs.resolve_mesh("meshes\\foo.nif").unwrap();
    assert_eq!(asset.mesh_resolution_rule, Some(MeshResolutionRule::Dist));
    let result = vfs.resolve_mesh_path("meshes\\foo.nif").unwrap();
    assert!(result.ends_with("foo_dist.nif"));
}

#[test]
fn resolve_mesh_path_xnif_applies() {
    let dir = tempfile::tempdir().unwrap();
    touch(&dir.path().join("meshes").join("foo.nif"));
    touch(&dir.path().join("meshes").join("xfoo.nif")); // Deliberately no xfoo.kf.
    touch(&dir.path().join("meshes").join("xfoo.kf"));
    let vfs = make_vfs(dir.path());
    let asset = vfs.resolve_mesh("meshes\\foo.nif").unwrap();
    assert_eq!(asset.mesh_resolution_rule, Some(MeshResolutionRule::XWithKf));
    let result = vfs.resolve_mesh_path("meshes\\foo.nif").unwrap();
    assert!(result.ends_with("xfoo.nif"));
}

#[test]
fn resolve_mesh_path_dist_beats_xnif() {
    let dir = tempfile::tempdir().unwrap();
    touch(&dir.path().join("meshes").join("foo.nif"));
    touch(&dir.path().join("meshes").join("foo_dist.nif"));
    touch(&dir.path().join("meshes").join("xfoo.nif"));
    touch(&dir.path().join("meshes").join("xfoo.kf"));
    let vfs = make_vfs(dir.path());
    let asset = vfs.resolve_mesh("meshes\\foo.nif").unwrap();
    assert_eq!(asset.mesh_resolution_rule, Some(MeshResolutionRule::Dist));
    let result = vfs.resolve_mesh_path("meshes\\foo.nif").unwrap();
    assert!(result.ends_with("foo_dist.nif"));
}

#[test]
fn resolve_mesh_path_xnif_needs_both() {
    let dir = tempfile::tempdir().unwrap();
    touch(&dir.path().join("meshes").join("foo.nif"));
    touch(&dir.path().join("meshes").join("xfoo.nif"));
    let vfs = make_vfs(dir.path());
    let asset = vfs.resolve_mesh("meshes\\foo.nif").unwrap();
    assert_eq!(asset.mesh_resolution_rule, Some(MeshResolutionRule::Original));
    let result = vfs.resolve_mesh_path("meshes\\foo.nif").unwrap();
    assert!(result.ends_with("foo.nif"));
}

#[test]
fn resolve_mesh_path_accepts_unprefixed_and_normalizes_query() {
    let dir = tempfile::tempdir().unwrap();
    touch(&dir.path().join("meshes").join("foo").join("bar.nif"));
    let vfs = make_vfs(dir.path());
    let result = vfs.resolve_mesh_path("Foo/Bar.nif").unwrap();
    assert!(result.ends_with(Path::new("foo").join("bar.nif")));
}

#[test]
fn resolve_mesh_accepts_non_nif_entries_like_the_base_key() {
    let dir = tempfile::tempdir().unwrap();
    touch(&dir.path().join("meshes").join("anim.kf"));
    let vfs = make_vfs(dir.path());

    // Override conventions are `.nif`-only, but the entry exists, so resolution must not report it
    // as missing: `resolve_mesh` and `resolve_model_mesh_key` accept the same set of keys.
    let asset = vfs.resolve_mesh("anim.kf").unwrap();

    assert_eq!(asset.key, "anim.kf");
    assert_eq!(asset.mesh_resolution_rule, Some(MeshResolutionRule::Original));
    assert_eq!(vfs.resolve_model_mesh_key("anim.kf"), Some("anim.kf"));
}

#[test]
fn resolve_model_mesh_key_accepts_prefixes_and_returns_base_key() {
    let dir = tempfile::tempdir().unwrap();
    touch(&dir.path().join("meshes").join("foo.nif"));
    touch(&dir.path().join("meshes").join("foo_dist.nif"));
    touch(&dir.path().join("meshes").join("xfoo.nif"));
    touch(&dir.path().join("meshes").join("xfoo.kf"));
    let vfs = make_vfs(dir.path());

    assert_eq!(vfs.resolve_model_mesh_key("Data Files/Meshes/Foo.NIF"), Some("foo.nif"));
    assert_eq!(vfs.resolve_model_mesh_key("meshes\\Foo.NIF"), Some("foo.nif"));
    assert_eq!(vfs.resolve_model_mesh_key("Foo.NIF"), Some("foo.nif"));
}

#[test]
fn resolve_texture_path_accepts_prefixed_and_unprefixed() {
    let dir = tempfile::tempdir().unwrap();
    touch(&dir.path().join("textures").join("foo.dds"));
    let vfs = make_vfs(dir.path());

    let prefixed = vfs.resolve_texture_path("textures\\foo.dds").unwrap();
    let unprefixed = vfs.resolve_texture_path("foo.dds").unwrap();

    assert_eq!(prefixed, unprefixed);
    assert!(prefixed.ends_with("foo.dds"));
}

#[test]
fn resolve_texture_path_prefers_dds_for_source_tga() {
    let dir = tempfile::tempdir().unwrap();
    touch(&dir.path().join("textures").join("foo.tga"));
    touch(&dir.path().join("textures").join("foo.dds"));
    let vfs = make_vfs(dir.path());

    let key = vfs.resolve_texture_key("Data Files/textures/Foo.TGA").unwrap();
    let path = vfs.resolve_texture_path("Data Files/textures/Foo.TGA").unwrap();

    assert_eq!(key, "foo.dds");
    assert!(path.ends_with("foo.dds"));
}

#[test]
fn resolve_texture_path_rejects_invalid_extension() {
    let dir = tempfile::tempdir().unwrap();
    touch(&dir.path().join("textures").join("foo.png"));
    let vfs = make_vfs(dir.path());

    assert!(vfs.resolve_texture_path("foo.png").is_none());
}

#[test]
fn static_texture_resolver_maps_missing_and_invalid_to_error_texture() {
    let dir = tempfile::tempdir().unwrap();
    let vfs = make_vfs(dir.path());

    assert_eq!(
        vfs.resolve_static_texture_key_or_error("missing.dds"),
        STATIC_ERROR_TEXTURE_KEY
    );
    assert_eq!(vfs.resolve_static_texture_key_or_error("bad.png"), STATIC_ERROR_TEXTURE_KEY);

    let sym = vfs.resolve_static_texture_sym_or_error("missing.dds");
    assert_eq!(vfs.texture_key_for_sym(sym), Some(STATIC_ERROR_TEXTURE_KEY));
}

#[test]
fn embedded_error_texture_reads_from_vfs() {
    let dir = tempfile::tempdir().unwrap();
    let vfs = make_vfs(dir.path());
    let asset = vfs.resolve_texture(STATIC_ERROR_TEXTURE_KEY).unwrap();

    assert_eq!(asset.key, STATIC_ERROR_TEXTURE_KEY);
    assert!(asset.source.path().is_none());
    assert_eq!(vfs.read_asset_bytes(&asset).unwrap().as_ref(), STATIC_ERROR_TEXTURE_DDS);
    let image =
        distantland_texture::texture_io::decode_texture_rgba(STATIC_ERROR_TEXTURE_KEY, STATIC_ERROR_TEXTURE_DDS, 4096)
            .unwrap();
    assert_eq!(image.get_pixel(0, 0).0, [255, 0, 255, 255]);
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "TextureSym used with a different VFS snapshot")]
fn texture_symbols_are_snapshot_local() {
    let first_dir = tempfile::tempdir().unwrap();
    let second_dir = tempfile::tempdir().unwrap();
    let first = make_vfs(first_dir.path());
    let second = make_vfs(second_dir.path());
    let sym = first.resolve_static_texture_sym_or_error("missing.dds");

    let _ = second.texture_key_for_sym(sym);
}

#[test]
fn bsa_only_mesh_and_texture_resolve() {
    let dir = tempfile::tempdir().unwrap();
    let bsa_path = dir.path().join("Test.bsa");
    write_bsa(&bsa_path, &[("meshes\\foo.nif", b"mesh"), ("textures\\foo.dds", b"texture")]);

    let vfs = make_vfs_with_archives(dir.path(), &[bsa_path]);
    let mesh = vfs.resolve_mesh("meshes\\foo.nif").unwrap();
    let texture = vfs.resolve_texture("textures\\foo.dds").unwrap();

    assert_eq!(mesh.key, "foo.nif");
    assert_eq!(vfs.read_asset_bytes(&mesh).unwrap().as_ref(), b"mesh");
    assert_eq!(texture.key, "foo.dds");
    assert_eq!(vfs.read_asset_bytes(&texture).unwrap().as_ref(), b"texture");
}

#[test]
fn later_bsa_overrides_earlier_bsa() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("First.bsa");
    let second = dir.path().join("Second.bsa");
    write_bsa(&first, &[("meshes\\foo.nif", b"first")]);
    write_bsa(&second, &[("meshes\\foo.nif", b"second")]);

    let vfs = make_vfs_with_archives(dir.path(), &[first, second]);
    let mesh = vfs.resolve_mesh("foo.nif").unwrap();

    assert_eq!(vfs.read_asset_bytes(&mesh).unwrap().as_ref(), b"second");
}

#[test]
fn loose_file_wins_only_when_newer_than_bsa() {
    let dir = tempfile::tempdir().unwrap();
    let bsa_path = dir.path().join("Test.bsa");
    write_bsa(&bsa_path, &[("meshes\\foo.nif", b"bsa")]);
    wait_for_distinct_mtime();
    fs::create_dir_all(dir.path().join("meshes")).unwrap();
    fs::write(dir.path().join("meshes").join("foo.nif"), b"loose").unwrap();

    let vfs = make_vfs_with_archives(dir.path(), &[bsa_path]);
    let mesh = vfs.resolve_mesh("foo.nif").unwrap();

    assert_eq!(vfs.read_asset_bytes(&mesh).unwrap().as_ref(), b"loose");
}

#[test]
fn older_loose_file_loses_to_bsa_but_remains_fallback_without_bsa_entry() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("meshes")).unwrap();
    fs::write(dir.path().join("meshes").join("foo.nif"), b"old loose").unwrap();
    fs::write(dir.path().join("meshes").join("bar.nif"), b"fallback").unwrap();
    wait_for_distinct_mtime();
    let bsa_path = dir.path().join("Test.bsa");
    write_bsa(&bsa_path, &[("meshes\\foo.nif", b"bsa")]);

    let vfs = make_vfs_with_archives(dir.path(), &[bsa_path]);
    let overridden = vfs.resolve_mesh("foo.nif").unwrap();
    let fallback = vfs.resolve_mesh("bar.nif").unwrap();

    assert_eq!(vfs.read_asset_bytes(&overridden).unwrap().as_ref(), b"bsa");
    assert_eq!(vfs.read_asset_bytes(&fallback).unwrap().as_ref(), b"fallback");
}

#[cfg(windows)]
#[test]
fn file_symlink_uses_target_mtime_for_bsa_precedence() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("shared").join("foo.nif");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, b"old target").unwrap();
    wait_for_distinct_mtime();

    let bsa_path = dir.path().join("Test.bsa");
    write_bsa(&bsa_path, &[("meshes\\foo.nif", b"bsa")]);
    wait_for_distinct_mtime();

    let link = dir.path().join("meshes").join("foo.nif");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    if !create_file_symlink_or_skip(&link, &target) {
        return;
    }

    let vfs = make_vfs_with_archives(dir.path(), &[bsa_path]);
    let mesh = vfs.resolve_mesh("foo.nif").unwrap();

    assert_eq!(mesh.source.path(), None);
    assert_eq!(vfs.read_asset_bytes(&mesh).unwrap().as_ref(), b"bsa");
}

#[test]
fn texture_tga_lookup_prefers_bsa_dds() {
    let dir = tempfile::tempdir().unwrap();
    let bsa_path = dir.path().join("Test.bsa");
    write_bsa(&bsa_path, &[("textures\\foo.dds", b"dds")]);

    let vfs = make_vfs_with_archives(dir.path(), &[bsa_path]);
    let texture = vfs.resolve_texture("textures\\foo.tga").unwrap();

    assert_eq!(texture.key, "foo.dds");
    assert_eq!(vfs.read_asset_bytes(&texture).unwrap().as_ref(), b"dds");
}

#[test]
fn mesh_override_rules_apply_across_bsa_sources() {
    let dir = tempfile::tempdir().unwrap();
    let bsa_path = dir.path().join("Test.bsa");
    write_bsa(
        &bsa_path,
        &[
            ("meshes\\foo.nif", b"base"),
            ("meshes\\foo_dist.nif", b"dist"),
            ("meshes\\xbar.nif", b"xbar"),
        ],
    );
    fs::create_dir_all(dir.path().join("meshes")).unwrap();
    fs::write(dir.path().join("meshes").join("bar.nif"), b"bar").unwrap();
    fs::write(dir.path().join("meshes").join("xbar.kf"), b"kf").unwrap();

    let vfs = make_vfs_with_archives(dir.path(), &[bsa_path]);
    let dist = vfs.resolve_mesh("foo.nif").unwrap();
    let xmesh = vfs.resolve_mesh("bar.nif").unwrap();

    assert_eq!(dist.key, "foo_dist.nif");
    assert_eq!(dist.mesh_resolution_rule, Some(MeshResolutionRule::Dist));
    assert_eq!(vfs.read_asset_bytes(&dist).unwrap().as_ref(), b"dist");
    assert_eq!(xmesh.key, "xbar.nif");
    assert_eq!(xmesh.mesh_resolution_rule, Some(MeshResolutionRule::XWithKf));
    assert_eq!(vfs.read_asset_bytes(&xmesh).unwrap().as_ref(), b"xbar");
}

#[ignore]
#[test]
fn default_morrowind_ini() {
    let ini_path = find_morrowind_ini();
    assert!(ini_path.is_ok());
    let ini_path = ini_path.unwrap();
    dbg!(&ini_path);

    let game_files = config_parsers::parse_morrowind_game_files(&ini_path).unwrap();
    dbg!(game_files);
}
