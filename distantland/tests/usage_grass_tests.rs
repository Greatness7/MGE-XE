//! Cross-crate coverage for dedicated grass-plugin usage loading.

use std::path::{Path, PathBuf};

use distantland::{
    DistantReference, StaticOverrides, UsageFilterOptions, UsageInfo, Vfs, VfsLoadOptions, classify_grass_plugins,
    is_grass_plugin,
};
use distantland_test_support::{
    BASELINE_WORLD_V1_GRASSLIST, BASELINE_WORLD_V1_MAINGRASS, FIXTURE_GRASS_PLUGIN_NAME, FIXTURE_PLUGIN_NAME,
    FIXTURE_SECOND_GRASS_PLUGIN_NAME, build_hermetic_fixture,
};
use itertools::Itertools;
use tes3::esp::{Cell, Header, Plugin, Reference, Static, TES3Object};

fn placement_facts(usage: &UsageInfo<'_>) -> Vec<(String, [u32; 3], [u32; 3], u32)> {
    let mut facts = usage
        .exterior_references()
        .into_iter()
        .flat_map(|references| references.values())
        .map(|reference: &DistantReference<'_>| {
            (
                reference.id.to_string(),
                reference.translation.to_array().map(f32::to_bits),
                reference.rotation.to_array().map(f32::to_bits),
                reference.scale.to_bits(),
            )
        })
        .collect_vec();
    facts.sort_unstable();
    facts
}

type TestGrassRef<'a> = (u32, u32, &'a str, bool);

fn write_grass_plugin(path: &Path, masters: &[&str], definition: Option<(&str, &str)>, references: &[TestGrassRef<'_>]) {
    let mut header = Header::default();
    header.masters = masters.iter().map(|name| ((*name).to_owned(), 0)).collect();
    let mut objects: Vec<TES3Object> = vec![header.into()];
    if let Some((id, mesh)) = definition {
        objects.push(
            Static {
                id: id.to_owned(),
                mesh: mesh.to_owned(),
                ..Static::default()
            }
            .into(),
        );
    }
    if !references.is_empty() {
        let mut cell = Cell::default();
        for &(mast_index, refr_index, id, deleted) in references {
            cell.references.insert(
                (mast_index, refr_index),
                Reference {
                    mast_index,
                    refr_index,
                    id: id.to_owned(),
                    deleted: deleted.then_some(true),
                    ..Reference::default()
                },
            );
        }
        objects.push(cell.into());
    }
    Plugin { objects }.save_path(path).unwrap();
}

fn grass_options() -> UsageFilterOptions {
    UsageFilterOptions {
        include_activators: true,
        include_misc: true,
        include_interiors_with_water: false,
        include_behaves_like_exterior: true,
        include_large_interiors: true,
        exclude_script_disable_targets: true,
        grass_density: 1.0,
    }
}

/// The two lists are deliberately not equivalent for groundcover.
///
/// The fixture's grass plugin restarts `refr_index` in each of its four cells, as generator tools
/// do. In the main load order that is a malformed plugin: identity is `(plugin, refr_index)`, so the
/// four cells collapse onto one another and the loss is reported. The grass list scopes identity per
/// cell, so every placement survives. That is why groundcover belongs there.
#[test]
fn main_load_order_collapses_groundcover_that_the_grass_list_keeps() {
    let temp = tempfile::tempdir().unwrap();
    let main_fixture = build_hermetic_fixture(BASELINE_WORLD_V1_MAINGRASS, &temp.path().join("main")).unwrap();
    let grass_fixture = build_hermetic_fixture(BASELINE_WORLD_V1_GRASSLIST, &temp.path().join("grass")).unwrap();
    let main_vfs = Vfs::load(&VfsLoadOptions {
        morrowind_ini: Some(main_fixture.morrowind_ini),
        data_dirs: Some(vec![main_fixture.data_dir]),
        plugins: Some(main_fixture.plugin_names),
    })
    .unwrap();
    let grass_vfs = Vfs::load(&VfsLoadOptions {
        morrowind_ini: Some(grass_fixture.morrowind_ini),
        data_dirs: Some(vec![grass_fixture.data_dir]),
        plugins: Some(grass_fixture.plugin_names),
    })
    .unwrap();
    let grass_paths =
        distantland::vfs::resolve_selected_plugins(&grass_fixture.grass_plugin_names, grass_vfs.data_dirs()).unwrap();
    let options = UsageFilterOptions {
        include_activators: true,
        include_misc: true,
        include_interiors_with_water: false,
        include_behaves_like_exterior: true,
        include_large_interiors: true,
        exclude_script_disable_targets: true,
        grass_density: 1.0,
    };
    let overrides = StaticOverrides::default();

    let main_usage = UsageInfo::from_load_order(&main_vfs, main_vfs.active_plugins(), &options, &overrides);
    let (grass_usage, _, _, grass_warnings, ()) =
        UsageInfo::setup_with_grass_plugins_and_capture(&grass_vfs, &grass_paths, &options, &overrides, |_| ()).unwrap();

    let main_grass = placement_facts(&main_usage)
        .into_iter()
        .filter(|fact| fact.0 == "grass\\fixture_grass.nif")
        .collect_vec();
    let dedicated_grass = placement_facts(&grass_usage)
        .into_iter()
        .filter(|fact| fact.0 == "grass\\fixture_grass.nif")
        .collect_vec();

    // The plugin places 16 references in each of 4 cells, all numbered 1..=16.
    assert_eq!(dedicated_grass.len(), 64);
    assert_eq!(main_grass.len(), 16);
    assert!(
        main_grass.iter().all(|fact| dedicated_grass.contains(fact)),
        "the placements the main list keeps should be a subset of what the grass list keeps"
    );

    assert!(grass_warnings.is_empty(), "active content-master placements should not warn");
}

#[test]
fn later_grass_master_reports_order_error_until_moved_first() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path();
    std::fs::create_dir_all(data_dir.join("Meshes/grass")).unwrap();
    std::fs::write(data_dir.join("Meshes/grass/blade.nif"), b"").unwrap();

    let ini_path = data_dir.join("Morrowind.ini");
    std::fs::write(&ini_path, b"").unwrap();

    let content_path = data_dir.join("content.esm");
    write_grass_plugin(&content_path, &[], None, &[]);
    let master_path = data_dir.join("master-grass.esm");
    let dependent_path = data_dir.join("dependent-grass.esp");
    write_grass_plugin(
        &master_path,
        &[],
        Some(("grass_blade", "grass\\blade.nif")),
        &[(0, 1, "grass_blade", false)],
    );
    write_grass_plugin(
        &dependent_path,
        &["master-grass.esm"],
        None,
        &[(1, 1, "grass_blade", true), (1, 2, "grass_blade", false)],
    );

    let vfs = Vfs::load(&VfsLoadOptions {
        morrowind_ini: Some(ini_path),
        data_dirs: Some(vec![data_dir.to_path_buf()]),
        plugins: Some(vec![content_path]),
    })
    .unwrap();
    let wrong_order = vec![dependent_path.clone(), master_path.clone()];
    let (_, _, _, warnings, ()) = UsageInfo::setup_with_grass_plugins_and_capture(
        &vfs,
        &wrong_order,
        &grass_options(),
        &StaticOverrides::default(),
        |_| (),
    )
    .unwrap();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].code, "grass_plugin_master_after_dependent");
    assert!(warnings[0].message.contains("1 non-delete reference(s)"));
    assert!(
        warnings[0].message.contains("1 delete reference(s)"),
        "{}",
        warnings[0].message
    );

    let correct_order = vec![master_path, dependent_path];
    let (_, _, _, warnings, ()) = UsageInfo::setup_with_grass_plugins_and_capture(
        &vfs,
        &correct_order,
        &grass_options(),
        &StaticOverrides::default(),
        |_| (),
    )
    .unwrap();
    assert!(warnings.is_empty());
}

#[test]
fn missing_and_invalid_grass_masters_have_distinct_diagnostics() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path();
    std::fs::create_dir_all(data_dir.join("Meshes/grass")).unwrap();
    std::fs::write(data_dir.join("Meshes/grass/blade.nif"), b"").unwrap();

    let ini_path = data_dir.join("Morrowind.ini");
    std::fs::write(&ini_path, b"").unwrap();

    let content_path = data_dir.join("content.esm");
    write_grass_plugin(&content_path, &[], None, &[]);
    let dependent_path = data_dir.join("dependent-grass.esp");
    write_grass_plugin(
        &dependent_path,
        &["absent.esm"],
        Some(("grass_blade", "grass\\blade.nif")),
        &[
            (1, 1, "grass_blade", false),
            (1, 2, "grass_blade", true),
            (7, 3, "grass_blade", false),
            (7, 4, "grass_blade", true),
        ],
    );

    let vfs = Vfs::load(&VfsLoadOptions {
        morrowind_ini: Some(ini_path),
        data_dirs: Some(vec![data_dir.to_path_buf()]),
        plugins: Some(vec![content_path]),
    })
    .unwrap();
    let (_, _, _, warnings, ()) = UsageInfo::setup_with_grass_plugins_and_capture(
        &vfs,
        &[dependent_path],
        &grass_options(),
        &StaticOverrides::default(),
        |_| (),
    )
    .unwrap();

    assert_eq!(
        warnings.iter().map(|warning| warning.code.as_str()).collect_vec(),
        ["grass_plugin_master_unselected", "grass_plugin_master_index_invalid"]
    );
    assert!(warnings[1].message.contains("MAST index 7"));
    for warning in warnings {
        assert!(warning.message.contains("1 non-delete reference(s)"));
        assert!(warning.message.contains("1 delete reference(s)"));
    }
}

/// List order decides overrides between grass plugins, but must not perturb disjoint ones.
///
/// The two fixture plugins touch different cells and different statics, so neither can override the
/// other and the resolved placements are identical either way round. Order dependence beyond actual
/// conflicts would leak into placement identity and the input fingerprint.
#[test]
fn disjoint_grass_plugins_resolve_identically_in_either_list_order() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = build_hermetic_fixture(BASELINE_WORLD_V1_GRASSLIST, temp.path()).unwrap();
    let data_dirs = vec![fixture.data_dir.clone()];
    assert!(is_grass_plugin(&fixture.data_dir.join(FIXTURE_GRASS_PLUGIN_NAME), &data_dirs));
    // Gate A: an ordinary plugin has no grass static, from itself or from a master, so it is
    // rejected without decoding cells.
    assert!(!is_grass_plugin(&fixture.data_dir.join(FIXTURE_PLUGIN_NAME), &data_dirs));
    // The batch form answers positionally, which is the whole contract its callers index by. A
    // missing file classifies `false` rather than failing the run alongside it.
    assert_eq!(
        classify_grass_plugins(
            &[
                fixture.data_dir.join(FIXTURE_PLUGIN_NAME),
                fixture.data_dir.join(FIXTURE_GRASS_PLUGIN_NAME),
                fixture.data_dir.join("absent.esp"),
            ],
            &data_dirs
        ),
        [false, true, false]
    );

    let vfs = Vfs::load(&VfsLoadOptions {
        morrowind_ini: Some(fixture.morrowind_ini),
        data_dirs: Some(vec![fixture.data_dir]),
        plugins: Some(fixture.plugin_names),
    })
    .unwrap();
    let mut paths = fixture.grass_plugin_names;
    paths.push(PathBuf::from(FIXTURE_SECOND_GRASS_PLUGIN_NAME));
    let paths = distantland::vfs::resolve_selected_plugins(&paths, vfs.data_dirs()).unwrap();
    let mut reverse = paths.clone();
    reverse.reverse();
    let options = UsageFilterOptions {
        include_activators: true,
        include_misc: true,
        include_interiors_with_water: false,
        include_behaves_like_exterior: true,
        include_large_interiors: true,
        exclude_script_disable_targets: true,
        grass_density: 1.0,
    };
    let overrides = StaticOverrides::default();
    let (forward, _, _, _, ()) =
        UsageInfo::setup_with_grass_plugins_and_capture(&vfs, &paths, &options, &overrides, |_| ()).unwrap();
    let (reverse, _, _, _, ()) =
        UsageInfo::setup_with_grass_plugins_and_capture(&vfs, &reverse, &options, &overrides, |_| ()).unwrap();
    assert_eq!(placement_facts(&forward), placement_facts(&reverse));
}

/// Classifier coverage, built from plugins written here rather than from a hermetic fixture.
///
/// The gates read record framing, mesh path strings, and reference ids, so none of these plugins
/// need meshes, textures, or a VFS.
mod classification {
    use std::path::{Path, PathBuf};

    use distantland::{classify_grass_plugins, is_grass_plugin};
    use tes3::esp::{Cell, FileType, Header, Landscape, LandscapeTexture, Plugin, Reference, Static, TES3Object};

    const GRASS_MESH: &str = "grass\\test_grass.nif";
    /// One past `GRASS_PLUGIN_INSTANCE_THRESHOLD`, so Gate B never decides these tests.
    const BULK_PLACEMENTS: u32 = 51;

    fn header(masters: &[&str]) -> TES3Object {
        let mut header = Header {
            file_type: FileType::Esp,
            ..Header::default()
        };
        header.masters = masters.iter().map(|name| ((*name).to_owned(), 0)).collect();
        TES3Object::Header(header)
    }

    fn grass_static(id: &str) -> TES3Object {
        TES3Object::Static(Static {
            id: id.to_owned(),
            mesh: GRASS_MESH.to_owned(),
            ..Static::default()
        })
    }

    /// An exterior cell placing `count` references to `id`, all locally addressed.
    fn placements(grid: (i32, i32), id: &str, count: u32) -> TES3Object {
        let mut cell = Cell::default();
        cell.data.grid = grid;
        for refr_index in 1..=count {
            let reference = Reference {
                mast_index: 0,
                refr_index,
                id: id.to_owned(),
                translation: [refr_index as f32 * 128.0, 0.0, 0.0],
                ..Reference::default()
            };
            cell.references.insert((0, refr_index), reference);
        }
        TES3Object::Cell(cell)
    }

    /// `count` records outside the `TES3`/`STAT`/`CELL` whitelist Gate 0 counts against.
    fn foreign_records(count: usize) -> Vec<TES3Object> {
        (0..count)
            .map(|index| {
                TES3Object::LandscapeTexture(LandscapeTexture {
                    id: format!("test_ltex_{index}"),
                    index: index as u32,
                    file_name: "test_land.dds".to_owned(),
                    ..LandscapeTexture::default()
                })
            })
            .collect()
    }

    fn write_plugin(dir: &Path, name: &str, objects: Vec<TES3Object>) -> PathBuf {
        let mut plugin = Plugin::new();
        plugin.objects = objects;
        let path = dir.join(name);
        plugin.save_path(&path).unwrap();
        path
    }

    /// A groundcover plugin needing no master: defines its own grass static and places it in bulk.
    fn write_grass_plugin(dir: &Path, name: &str) -> PathBuf {
        write_plugin(
            dir,
            name,
            vec![
                header(&[]),
                grass_static("test_grass"),
                placements((0, 0), "test_grass", BULK_PLACEMENTS),
            ],
        )
    }

    /// The `Sky_Main_Grass.esp` shape: no `STAT` records at all, every grass static from the
    /// master. Requiring a local definition rejects this before any placement is counted.
    #[test]
    fn grass_statics_defined_only_by_a_master_still_classify() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();

        write_plugin(dir, "master.esm", vec![header(&[]), grass_static("master_grass")]);
        let dependent = write_plugin(
            dir,
            "dependent.esp",
            vec![header(&["master.esm"]), placements((0, 0), "master_grass", BULK_PLACEMENTS)],
        );

        assert!(is_grass_plugin(&dependent, &[dir.to_path_buf()]));
        // An unresolvable master is "unknown", not "defines no grass".
        assert!(!is_grass_plugin(&dependent, &[]));
    }

    /// The `Tamriel_Data.esm` shape: defines grass statics, places none. Broadening Gate A must
    /// not make groundcover out of the masters that feed it.
    #[test]
    fn a_master_that_defines_grass_but_places_none_is_not_groundcover() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let master = write_plugin(dir, "master.esm", vec![header(&[]), grass_static("master_grass")]);

        assert!(!is_grass_plugin(&master, &[dir.to_path_buf()]));
    }

    /// Gate 0's boundary, asserted from both sides.
    #[test]
    fn foreign_records_are_tolerated_up_to_one_hundred() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();

        let mut at_tolerance = vec![
            header(&[]),
            grass_static("test_grass"),
            placements((0, 0), "test_grass", BULK_PLACEMENTS),
        ];
        at_tolerance.extend(foreign_records(100));
        let accepted = write_plugin(dir, "at-tolerance.esp", at_tolerance.clone());

        at_tolerance.extend(foreign_records(1));
        let rejected = write_plugin(dir, "past-tolerance.esp", at_tolerance);

        assert_eq!(
            classify_grass_plugins(&[accepted, rejected], &[dir.to_path_buf()]),
            [true, false]
        );
    }

    /// Groundcover mixed into a plugin that also carries a landmass is deliberately unsupported:
    /// Gate 0 rejects it on record count. This pins the exclusion, not a desired pass.
    #[test]
    fn a_grass_plugin_carrying_a_landmass_is_the_documented_exclusion() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();

        let mut objects = vec![
            header(&[]),
            grass_static("test_grass"),
            placements((0, 0), "test_grass", BULK_PLACEMENTS),
        ];
        objects.extend((0..101).map(|index| {
            TES3Object::Landscape(Landscape {
                grid: (index, 0),
                ..Landscape::default()
            })
        }));
        let hybrid = write_plugin(dir, "hybrid.esp", objects);

        assert!(!is_grass_plugin(&hybrid, &[dir.to_path_buf()]));
    }

    /// `File::seek` past the end succeeds silently, so a streaming walk trusting the declared
    /// extent would report the records before the damage as a clean parse.
    #[test]
    fn a_truncated_plugin_is_rejected_rather_than_read_as_a_clean_end() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();

        let intact = write_grass_plugin(dir, "intact.esp");
        assert!(is_grass_plugin(&intact, &[dir.to_path_buf()]));

        let bytes = std::fs::read(&intact).unwrap();
        let truncated = dir.join("truncated.esp");
        std::fs::write(&truncated, &bytes[..bytes.len() - 16]).unwrap();

        assert!(!is_grass_plugin(&truncated, &[dir.to_path_buf()]));
    }

    /// A single flattened set of every master's grass ids would classify the bystander here as
    /// groundcover, which reads as a plausible verdict rather than a failure.
    #[test]
    fn a_master_lends_its_grass_ids_only_to_the_plugins_that_declare_it() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();

        write_plugin(dir, "master.esm", vec![header(&[]), grass_static("shared_grass")]);
        let dependent = write_plugin(
            dir,
            "dependent.esp",
            vec![header(&["master.esm"]), placements((0, 0), "shared_grass", BULK_PLACEMENTS)],
        );
        // Places the same ids without declaring a master, so nothing defines them.
        let bystander = write_plugin(
            dir,
            "bystander.esp",
            vec![header(&[]), placements((1, 0), "shared_grass", BULK_PLACEMENTS)],
        );

        assert_eq!(
            classify_grass_plugins(&[dependent, bystander], &[dir.to_path_buf()]),
            [true, false]
        );
    }

    /// Both wrong handlings produce ordinary-looking verdicts: aborting the batch loses every
    /// other plugin's answer, and treating the master as defining no grass turns "unknown" into a
    /// confident `false`.
    #[test]
    fn an_unreadable_master_fails_only_its_own_dependents() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();

        std::fs::write(dir.join("broken.esm"), b"not a plugin, but long enough to look like one").unwrap();
        let broken_master = write_plugin(
            dir,
            "broken-dependent.esp",
            vec![header(&["broken.esm"]), placements((0, 0), "master_grass", BULK_PLACEMENTS)],
        );
        let missing_master = write_plugin(
            dir,
            "missing-dependent.esp",
            vec![header(&["absent.esm"]), placements((1, 0), "master_grass", BULK_PLACEMENTS)],
        );
        let unrelated = write_grass_plugin(dir, "unrelated.esp");

        assert_eq!(
            classify_grass_plugins(&[broken_master, missing_master, unrelated], &[dir.to_path_buf()]),
            [false, false, true]
        );
    }

    /// The last data directory holding a master name wins, matching the rest of the codebase.
    #[test]
    fn a_higher_priority_data_dir_supplies_the_master() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("base");
        let overlay = temp.path().join("overlay");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(&overlay).unwrap();

        // Same filename in both layers; only the overlay's copy defines the placed static.
        write_plugin(&base, "master.esm", vec![header(&[]), grass_static("other_grass")]);
        write_plugin(&overlay, "master.esm", vec![header(&[]), grass_static("master_grass")]);
        let dependent = write_plugin(
            &base,
            "dependent.esp",
            vec![header(&["master.esm"]), placements((0, 0), "master_grass", BULK_PLACEMENTS)],
        );

        assert!(is_grass_plugin(&dependent, &[base.clone(), overlay.clone()]));
        assert!(!is_grass_plugin(&dependent, &[overlay, base]));
    }

    /// `mast_index` names the source of the reference, not of the base object's definition, so
    /// excluding non-zero indices drops overrides and moved placements that groundcover carries.
    /// Deleted references stay excluded: they place nothing.
    #[test]
    fn gate_b_counts_master_addressed_placements_and_skips_deleted_ones() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();

        write_plugin(dir, "master.esm", vec![header(&[]), grass_static("master_grass")]);

        let mut cell = Cell::default();
        cell.data.grid = (0, 0);
        for refr_index in 1..=BULK_PLACEMENTS {
            let reference = Reference {
                mast_index: 1,
                refr_index,
                id: "master_grass".to_owned(),
                translation: [refr_index as f32 * 128.0, 0.0, 0.0],
                ..Reference::default()
            };
            cell.references.insert((1, refr_index), reference);
        }
        let overrides = write_plugin(dir, "overrides.esp", vec![header(&["master.esm"]), TES3Object::Cell(cell)]);

        let mut deleted_cell = Cell::default();
        deleted_cell.data.grid = (1, 0);
        for refr_index in 1..=BULK_PLACEMENTS {
            let reference = Reference {
                mast_index: 1,
                refr_index,
                id: "master_grass".to_owned(),
                deleted: Some(true),
                ..Reference::default()
            };
            deleted_cell.references.insert((1, refr_index), reference);
        }
        let deletions = write_plugin(
            dir,
            "deletions.esp",
            vec![header(&["master.esm"]), TES3Object::Cell(deleted_cell)],
        );

        assert_eq!(
            classify_grass_plugins(&[overrides, deletions], &[dir.to_path_buf()]),
            [true, false]
        );
    }
}
