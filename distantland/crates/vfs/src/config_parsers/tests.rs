use super::*;
use std::fs;

fn write_ini(root: &Path, contents: &str) -> PathBuf {
    fs::create_dir_all(root.join("Data Files")).unwrap();
    let ini_path = root.join("Morrowind.ini");
    fs::write(&ini_path, contents).unwrap();
    ini_path
}

#[test]
fn parse_archives_registers_morrowind_first_then_contiguous_entries() {
    let dir = tempfile::tempdir().unwrap();
    let ini_path = write_ini(
        dir.path(),
        "\
[Archives]
Archive 1=Patch.bsa
Archive 0=Tribunal.bsa
",
    );

    let archives = parse_morrowind_archive_files(&ini_path).unwrap();

    assert_eq!(
        archives,
        vec![
            dir.path().join("Data Files").join("Morrowind.bsa"),
            dir.path().join("Data Files").join("Tribunal.bsa"),
            dir.path().join("Data Files").join("Patch.bsa"),
        ]
    );
}

#[test]
fn parse_archives_stops_at_first_missing_index() {
    let dir = tempfile::tempdir().unwrap();
    let ini_path = write_ini(
        dir.path(),
        "\
[Archives]
Archive 0=Tribunal.bsa
Archive 2=Skipped.bsa
",
    );

    let archives = parse_morrowind_archive_files(&ini_path).unwrap();

    assert_eq!(
        archives,
        vec![
            dir.path().join("Data Files").join("Morrowind.bsa"),
            dir.path().join("Data Files").join("Tribunal.bsa"),
        ]
    );
}

#[test]
fn parse_archives_ignores_non_bsa_values_and_malformed_keys() {
    let dir = tempfile::tempdir().unwrap();
    let ini_path = write_ini(
        dir.path(),
        "\
[Archives]
Archive X=Bad.bsa
Archive 0=Readme.txt
Archive 1=Tribunal.bsa
",
    );

    let archives = parse_morrowind_archive_files(&ini_path).unwrap();

    assert_eq!(
        archives,
        vec![
            dir.path().join("Data Files").join("Morrowind.bsa"),
            dir.path().join("Data Files").join("Tribunal.bsa"),
        ]
    );
}

#[test]
fn parse_archives_searches_active_data_dirs_in_priority_order() {
    let dir = tempfile::tempdir().unwrap();
    let primary = dir.path().join("Primary Data Files");
    let extra = dir.path().join("Extra Data Files");
    fs::create_dir_all(&primary).unwrap();
    fs::create_dir_all(&extra).unwrap();
    fs::write(primary.join("Morrowind.bsa"), []).unwrap();
    fs::write(primary.join("Tribunal.bsa"), b"primary").unwrap();
    fs::write(extra.join("Tribunal.bsa"), b"extra").unwrap();

    let ini_path = write_ini(
        dir.path(),
        "\
[Archives]
Archive 0=Tribunal.bsa
",
    );

    let archives = parse_morrowind_archive_files_with_data_dirs(&ini_path, &[primary.clone(), extra.clone()]).unwrap();

    assert_eq!(archives[0], primary.join("Morrowind.bsa"));
    assert_eq!(archives[1], extra.join("Tribunal.bsa"));
}

#[test]
fn parse_game_files_searches_active_data_dirs_in_priority_order() {
    let dir = tempfile::tempdir().unwrap();
    let primary = dir.path().join("Primary Data Files");
    let extra = dir.path().join("Extra Data Files");
    fs::create_dir_all(&primary).unwrap();
    fs::create_dir_all(&extra).unwrap();
    fs::write(primary.join("Example.esp"), []).unwrap();
    fs::write(extra.join("Example.esp"), []).unwrap();

    let ini_path = write_ini(
        dir.path(),
        "\
[Game Files]
GameFile0=Example.esp
",
    );

    let game_files = parse_morrowind_game_files_with_data_dirs(&ini_path, &[primary.clone(), extra.clone()]).unwrap();

    assert_eq!(game_files, vec![extra.join("Example.esp")]);
}
