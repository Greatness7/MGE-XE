use super::*;
use crate::{StaticOverride, StaticOverrides};
use itertools::Itertools;

fn make_args() -> UsageFilterOptions {
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

fn make_reference(id: &str, translation: Vec3) -> DistantReference<'static> {
    DistantReference {
        id: Cow::Owned(id.to_string()),
        deleted: false,
        persistent: false,
        translation,
        rotation: Vec3::ZERO,
        scale: 1.0,
        vis_index: 0,
    }
}

fn make_usage_with_references(
    object_id: &str,
    mesh: &'static str,
    count: u32,
    object_override: impl FnOnce(&mut ObjectDefinition<'static>),
) -> UsageInfo<'static> {
    let mut usage = UsageInfo::default();
    usage.use_test_reference_source();
    let mut definition = ObjectDefinition {
        mesh,
        ignore_by_default: false,
        force_mesh_generation: false,
        vis_index: 0,
        kind: ObjectKind::Static,
        script_id: UString::new(""),
        script_disabled: false,
        disable_target: false,
    };
    object_override(&mut definition);
    usage.objects.insert(object_id.to_string(), definition);

    let exterior = usage.exterior_references_mut();
    for index in 0..count {
        exterior.insert(
            StableRefKey::test(index + 1),
            make_reference(object_id, Vec3::new(index as f32 * 64.0, index as f32 * 32.0, index as f32)),
        );
    }

    usage
}

#[test]
fn release_post_fingerprint_fields_drops_metadata_and_preserves_world_cells() {
    let mut usage = make_usage_with_references("object", "object.nif", 1, |_| {});
    usage.forced_meshes.insert("forced.nif");
    usage.door_meshes.insert("door.nif");
    usage.mesh_scale_maximums.insert("object.nif", 1.0);
    usage
        .interior_metadata
        .insert(UString::new("Interior"), InteriorMetadata::default());
    usage.disable_scripts.insert(UString::new("disable_script"));
    usage.disable_targets.insert(UString::new("disable_target"));
    usage.script_disable.self_disabling_scripts = 1;
    usage.terrain_cells.insert(
        (0, 0),
        crate::TerrainCell {
            grid: (0, 0),
            heights: Box::new([[0.0; 65]; 65]),
            normals: crate::TerrainNormals::Default,
            colors: crate::TerrainColors::Default,
            texture_indices: Box::new([[0; 16]; 16]),
            texture_table: crate::TerrainTextureTable::default(),
        },
    );

    usage.release_post_fingerprint_fields();

    assert_eq!(usage.reference_sources, ReferenceSources::default());
    assert_eq!(usage.objects.capacity(), 0);
    assert_eq!(usage.forced_meshes.capacity(), 0);
    assert_eq!(usage.door_meshes.capacity(), 0);
    assert_eq!(usage.mesh_scale_maximums.capacity(), 0);
    assert_eq!(usage.interior_metadata.capacity(), 0);
    assert_eq!(usage.disable_scripts.capacity(), 0);
    assert_eq!(usage.disable_targets.capacity(), 0);
    assert_eq!(usage.script_disable, ScriptDisableReport::default());
    assert_eq!(usage.exterior_references_count(), 1);
    assert!(usage.terrain_cells.contains_key(&(0, 0)));
}

type TestGrassRef<'a> = (u32, u32, &'a str, f32, bool);

type TestGrassCell<'a> = ((i32, i32), Vec<TestGrassRef<'a>>);

fn grass_plugin_file(
    path: &std::path::Path,
    masters: &[&str],
    definition: Option<(&str, &str)>,
    cells: &[TestGrassCell<'_>],
) {
    use tes3::esp::{Cell, Header, Plugin, Reference, Static, TES3Object};

    let mut header = Header::default();
    for master in masters {
        header.masters.push(((*master).to_owned(), 0));
    }
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
    for (grid, references) in cells {
        let mut cell = Cell::default();
        cell.data.grid = *grid;
        for &(mast_index, refr_index, id, x, deleted) in references {
            cell.references.insert(
                (mast_index, refr_index),
                Reference {
                    mast_index,
                    refr_index,
                    id: id.to_owned(),
                    translation: [x, 0.0, 0.0],
                    deleted: deleted.then_some(true),
                    ..Reference::default()
                },
            );
        }
        objects.push(cell.into());
    }
    Plugin { objects }.save_path(path).unwrap();
}

fn grass_test_vfs(data_dir: &std::path::Path, active_plugins: Vec<std::path::PathBuf>) -> Vfs {
    Vfs {
        ini_path: data_dir.join("Morrowind.ini"),
        data_dirs: vec![data_dir.to_path_buf()],
        active_plugins,
        archives: vec![],
        maps: crate::vfs::directory_map::build_directory_map(&[data_dir.to_path_buf()]).unwrap(),
    }
}

/// A later grass plugin overrides and deletes an earlier one's placements, and may place references
/// to grass statics the earlier one defines.
///
/// This is the behaviour the dedicated grass list lost when it stopped resolving master-addressed
/// and deleted references. Identity is scoped per cell, so the base plugin reusing `refr_index` 1 in
/// two cells yields two independent placements rather than one collision.
#[test]
fn grass_list_applies_cross_plugin_overrides_deletes_and_definitions() {
    use crate::vfs::directory_map::build_directory_map;

    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().to_path_buf();
    std::fs::create_dir_all(data_dir.join("Meshes/grass")).unwrap();
    std::fs::write(data_dir.join("Meshes/grass/blade.nif"), b"").unwrap();

    let base_path = data_dir.join("base-grass.esp");
    let patch_path = data_dir.join("patch-grass.esp");

    // `refr_index` 1 is reused across two cells, exactly as groundcover generators emit it.
    grass_plugin_file(
        &base_path,
        &[],
        Some(("grass_blade", "grass\\blade.nif")),
        &[
            (
                (0, 0),
                vec![(0, 1, "grass_blade", 10.0, false), (0, 2, "grass_blade", 20.0, false)],
            ),
            ((1, 0), vec![(0, 1, "grass_blade", 30.0, false)]),
        ],
    );
    // Deletes the first, repositions the second, and places a new one using the base's static.
    grass_plugin_file(
        &patch_path,
        &["base-grass.esp"],
        None,
        &[(
            (0, 0),
            vec![
                (1, 1, "grass_blade", 10.0, true),
                (1, 2, "grass_blade", 99.0, false),
                (0, 1, "grass_blade", 40.0, false),
            ],
        )],
    );

    let vfs = Vfs {
        ini_path: data_dir.join("Morrowind.ini"),
        data_dirs: vec![data_dir.clone()],
        active_plugins: vec![],
        archives: vec![],
        maps: build_directory_map(std::slice::from_ref(&data_dir)).unwrap(),
    };

    let grass_paths = vec![base_path, patch_path];
    let reference_sources = ReferenceSources::from_plugin_lists(&[], &grass_paths);
    let main_objects = HashMap::new();
    let loaded = load_grass_plugins(
        &vfs,
        &grass_paths,
        &main_objects,
        &make_args(),
        &StaticOverrides::default(),
        &reference_sources,
    )
    .unwrap();

    let mut placements: Vec<u32> = loaded
        .usage_info
        .exterior_references()
        .unwrap()
        .values()
        .map(|reference| reference.translation.x.to_bits())
        .collect();
    placements.sort_unstable();

    // 10.0 was deleted; 20.0 was overridden to 99.0; 30.0 is a different cell so it survives
    // despite sharing `refr_index` 1; 40.0 is the patch's own new placement.
    let mut expected = vec![30.0_f32.to_bits(), 40.0_f32.to_bits(), 99.0_f32.to_bits()];
    expected.sort_unstable();
    assert_eq!(placements, expected);
}

#[test]
fn grass_list_definition_overrides_active_main_definition() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path();
    std::fs::create_dir_all(data_dir.join("Meshes/grass")).unwrap();
    std::fs::write(data_dir.join("Meshes/grass/main.nif"), b"").unwrap();
    std::fs::write(data_dir.join("Meshes/grass/override.nif"), b"").unwrap();

    let master_path = data_dir.join("master.esm");
    let grass_path = data_dir.join("dependent-grass.esp");
    grass_plugin_file(&master_path, &[], Some(("grass_blade", "grass\\main.nif")), &[]);
    grass_plugin_file(
        &grass_path,
        &["master.esm"],
        Some(("grass_blade", "grass\\override.nif")),
        &[((0, 0), vec![(0, 1, "grass_blade", 10.0, false)])],
    );

    let vfs = grass_test_vfs(data_dir, vec![master_path]);
    let (usage, _, _, warnings, ()) = UsageInfo::setup_with_grass_plugins_and_capture(
        &vfs,
        &[grass_path],
        &make_args(),
        &StaticOverrides::default(),
        |_| (),
    )
    .unwrap();

    assert!(warnings.is_empty());
    let meshes = usage
        .exterior_references()
        .unwrap()
        .values()
        .map(|reference| reference.id.as_ref())
        .collect_vec();
    assert_eq!(meshes, ["grass\\override.nif"]);
}

#[test]
fn unresolved_master_and_local_reference_keys_do_not_collide() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path();
    std::fs::create_dir_all(data_dir.join("Meshes/grass")).unwrap();
    std::fs::write(data_dir.join("Meshes/grass/blade.nif"), b"").unwrap();

    let grass_path = data_dir.join("dependent-grass.esp");
    grass_plugin_file(
        &grass_path,
        &["absent.esm"],
        Some(("grass_blade", "grass\\blade.nif")),
        &[(
            (0, 0),
            vec![(0, 1, "grass_blade", 10.0, false), (1, 1, "grass_blade", 20.0, false)],
        )],
    );

    let vfs = grass_test_vfs(data_dir, vec![]);
    let grass_paths = vec![grass_path];
    let reference_sources = ReferenceSources::from_plugin_lists(&[], &grass_paths);
    let loaded = load_grass_plugins(
        &vfs,
        &grass_paths,
        &HashMap::new(),
        &make_args(),
        &StaticOverrides::default(),
        &reference_sources,
    )
    .unwrap();

    let mut placements = loaded
        .usage_info
        .exterior_references()
        .unwrap()
        .values()
        .map(|reference| reference.translation.x.to_bits())
        .collect_vec();
    placements.sort_unstable();
    assert_eq!(placements, [10.0_f32.to_bits(), 20.0_f32.to_bits()]);
    assert_eq!(loaded.warnings[0].code, "grass_plugin_master_unselected");
}

#[test]
fn active_content_master_warns_only_for_ignored_deletes() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path();
    std::fs::create_dir_all(data_dir.join("Meshes/grass")).unwrap();
    std::fs::write(data_dir.join("Meshes/grass/blade.nif"), b"").unwrap();

    let content_path = data_dir.join("content.esm");
    let grass_path = data_dir.join("dependent-grass.esp");
    grass_plugin_file(&content_path, &[], None, &[]);
    grass_plugin_file(
        &grass_path,
        &["content.esm"],
        Some(("grass_blade", "grass\\blade.nif")),
        &[(
            (0, 0),
            vec![(1, 1, "grass_blade", 10.0, false), (1, 2, "grass_blade", 20.0, true)],
        )],
    );

    let vfs = grass_test_vfs(data_dir, vec![content_path.clone()]);
    let grass_paths = vec![grass_path];
    let sources = ReferenceSources::from_plugin_lists(&[content_path], &grass_paths);
    let loaded = load_grass_plugins(
        &vfs,
        &grass_paths,
        &HashMap::new(),
        &make_args(),
        &StaticOverrides::default(),
        &sources,
    )
    .unwrap();

    assert_eq!(loaded.warnings.len(), 1);
    assert_eq!(loaded.warnings[0].code, "grass_plugin_content_master_delete_ignored");
    assert!(loaded.warnings[0].message.contains("1 delete reference(s)"));
    assert!(!loaded.warnings[0].message.contains("kept"));
}

/// The main load order requires plugin-unique reference indices and no longer works around
/// groundcover files that restart numbering in every cell. Such a plugin loses placements; it
/// belongs in `grass_plugins`, where identity is scoped per cell and the collision cannot occur.
#[test]
fn loader_collapses_reused_exterior_reference_indices_instead_of_reassigning_them() {
    use crate::vfs::directory_map::build_directory_map;
    use tes3::esp::{Cell, Header, Plugin, Reference};

    let temp = tempfile::tempdir().unwrap();
    let plugin_path = temp.path().join("Groundcover.esp");

    let mut first_cell = Cell::default();
    first_cell.data.grid = (0, 0);
    first_cell.references.insert(
        (0, 1),
        Reference {
            id: "grass_a".to_string(),
            ..Reference::default()
        },
    );

    let mut second_cell = Cell::default();
    second_cell.data.grid = (1, 0);
    second_cell.references.insert(
        (0, 1),
        Reference {
            id: "grass_b".to_string(),
            ..Reference::default()
        },
    );

    let mut plugin = Plugin {
        objects: vec![Header::default().into(), first_cell.into(), second_cell.into()],
    };
    plugin.save_path(&plugin_path).unwrap();

    let vfs = Vfs {
        ini_path: temp.path().join("Morrowind.ini"),
        data_dirs: vec![temp.path().to_path_buf()],
        active_plugins: vec![plugin_path.clone()],
        archives: vec![],
        maps: build_directory_map(&[temp.path().to_path_buf()]).unwrap(),
    };

    let reference_sources = ReferenceSources::from_load_order(vfs.active_plugins());
    let result = UsageInfo::from_path_with_identity_and_sources(
        &plugin_path,
        &vfs,
        &make_args(),
        &StaticOverrides::default(),
        &reference_sources,
    );
    let expected_bytes = std::fs::read(&plugin_path).unwrap();
    assert_eq!(
        result.identity.unwrap().content,
        distantland_foundation::identity::ContentIdentity::from_bytes(&expected_bytes)
    );

    // Both cells declare `(mast_index 0, refr_index 1)`, so they share one identity and only the
    // later cell's placement survives.
    let usage = result.usage_info.unwrap();
    let references = usage.exterior_references().unwrap();
    assert_eq!(references.keys().copied().collect_vec(), [StableRefKey::test(1)]);
    assert_eq!(
        usage.reference_source_name(StableRefKey::test(1).source()),
        Some("groundcover.esp")
    );
    assert_eq!(
        references.values().map(|reference| reference.id.as_ref()).collect_vec(),
        ["grass_b"]
    );
}

#[test]
fn stable_reference_keys_remain_eight_bytes() {
    assert_eq!(std::mem::size_of::<StableRefKey>(), 8);
}

#[test]
fn master_overrides_share_the_declared_master_filename_source() {
    use crate::vfs::directory_map::build_directory_map;
    use tes3::esp::{Cell, Header, Plugin, Reference};

    let temp = tempfile::tempdir().unwrap();
    let master_path = temp.path().join("Master.esm");
    let override_path = temp.path().join("Override.esp");

    let mut master_cell = Cell::default();
    master_cell.data.grid = (0, 0);
    master_cell.references.insert(
        (0, 7),
        Reference {
            id: "master_object".to_owned(),
            ..Reference::default()
        },
    );
    let mut master = Plugin {
        objects: vec![Header::default().into(), master_cell.into()],
    };
    master.save_path(&master_path).unwrap();

    let mut override_header = Header::default();
    override_header.masters.push(("MASTER.ESM".to_owned(), 0));
    let mut override_cell = Cell::default();
    override_cell.data.grid = (0, 0);
    override_cell.references.insert(
        (1, 7),
        Reference {
            mast_index: 1,
            refr_index: 7,
            id: "master_object".to_owned(),
            deleted: Some(true),
            ..Reference::default()
        },
    );
    let mut override_plugin = Plugin {
        objects: vec![override_header.into(), override_cell.into()],
    };
    override_plugin.save_path(&override_path).unwrap();

    let vfs = Vfs {
        ini_path: temp.path().join("Morrowind.ini"),
        data_dirs: vec![temp.path().to_path_buf()],
        active_plugins: vec![master_path.clone(), override_path.clone()],
        archives: vec![],
        maps: build_directory_map(&[temp.path().to_path_buf()]).unwrap(),
    };
    let args = make_args();
    let overrides = StaticOverrides::default();
    let reference_sources = ReferenceSources::from_load_order(vfs.active_plugins());
    let load = |path| {
        UsageInfo::from_path_with_identity_and_sources(path, &vfs, &args, &overrides, &reference_sources)
            .usage_info
            .unwrap()
    };
    let master_usage = load(&master_path);
    let override_usage = load(&override_path);

    let (master_key, _) = master_usage.exterior_references().unwrap().first().unwrap();
    let (override_key, override_reference) = override_usage.exterior_references().unwrap().first().unwrap();
    assert_eq!(master_key, override_key);
    assert_eq!(master_usage.reference_source_name(master_key.source()), Some("master.esm"));
    assert!(override_reference.deleted);
}

#[test]
fn script_match_wins_over_unique_object_match() {
    let args = make_args();
    let mut overrides = StaticOverrides::default();
    overrides.dynamic_vis.scripts.insert("script".to_string(), 1);
    overrides.dynamic_vis.unique_objects.insert("thing".to_string(), 2);

    let definition =
        UsageInfo::build_object_definition("thing", "script", "mesh.nif", ObjectKind::Static, &args, &overrides).unwrap();

    assert_eq!(definition.vis_index, 1);
    assert!(definition.force_mesh_generation);
    assert!(!definition.ignore_by_default);
    assert_eq!(definition.kind, ObjectKind::Static);
}

#[test]
fn door_objects_are_included_like_statics_regardless_of_include_misc() {
    let mut args = make_args();
    args.include_misc = false;
    let overrides = StaticOverrides::default();

    // Doors are kept even when `include_misc` is off, just like statics.
    let door = UsageInfo::build_object_definition("door1", "", "door.nif", ObjectKind::Door, &args, &overrides).unwrap();
    assert!(!door.ignore_by_default);
    assert_eq!(door.kind, ObjectKind::Door);

    // Other misc objects still honor `include_misc`.
    let misc = UsageInfo::build_object_definition("misc1", "", "misc.nif", ObjectKind::Misc, &args, &overrides).unwrap();
    assert!(misc.ignore_by_default);
    assert_eq!(misc.kind, ObjectKind::Misc);
}

#[test]
fn mesh_override_replaces_the_include_misc_check() {
    let mut args = make_args();
    args.include_misc = false;
    let mut overrides = StaticOverrides::default();

    // A `= far` entry carries `ignore: false`, which forces the light in despite `include_misc`.
    overrides.mesh_overrides.insert(
        "l\\light_torch_01.nif".to_owned(),
        StaticOverride {
            static_type: StaticType::StaticFar,
            ..StaticOverride::default()
        },
    );
    let light =
        UsageInfo::build_object_definition("light1", "", "l\\light_torch_01.nif", ObjectKind::Misc, &args, &overrides)
            .unwrap();
    assert!(!light.ignore_by_default);

    // An `ignore` entry still excludes, and so does the kind check when no entry exists.
    overrides.mesh_overrides.insert(
        "l\\light_torch_01.nif".to_owned(),
        StaticOverride {
            ignore: true,
            ..StaticOverride::default()
        },
    );
    let ignored =
        UsageInfo::build_object_definition("light1", "", "l\\light_torch_01.nif", ObjectKind::Misc, &args, &overrides)
            .unwrap();
    assert!(ignored.ignore_by_default);
}

#[test]
fn object_definition_retains_effective_script_id() {
    let args = make_args();
    let mut overrides = StaticOverrides::default();

    let scripted =
        UsageInfo::build_object_definition("thing", "disable_me", "mesh.nif", ObjectKind::Activator, &args, &overrides)
            .unwrap();
    assert_eq!(scripted.script_id, "disable_me");
    // Classification is deferred: nothing is decided until the whole load order has merged.
    assert!(!scripted.script_disabled);

    overrides.mesh_overrides.insert(
        "mesh.nif".to_owned(),
        StaticOverride {
            no_script: true,
            ..StaticOverride::default()
        },
    );
    let scriptless =
        UsageInfo::build_object_definition("thing", "disable_me", "mesh.nif", ObjectKind::Activator, &args, &overrides)
            .unwrap();
    assert!(scriptless.script_id.is_empty());
}

/// Builds a one-object `UsageInfo` whose object carries `script_id`, then classifies it against
/// `disable_scripts`.
fn classify_one(
    script_id: &str,
    disable_scripts: &[&str],
    adjust: impl FnOnce(&mut ObjectDefinition<'static>),
) -> ObjectDefinition<'static> {
    let args = make_args();
    let overrides = StaticOverrides::default();
    let mut definition =
        UsageInfo::build_object_definition("thing", script_id, "mesh.nif", ObjectKind::Activator, &args, &overrides)
            .unwrap();
    adjust(&mut definition);

    let mut usage: UsageInfo<'static> = UsageInfo::default();
    usage.objects.insert("thing".to_owned(), definition);
    usage.disable_scripts = disable_scripts.iter().map(|id| UString::new((*id).to_owned())).collect();
    usage.classify_script_disables();
    usage.objects.remove("thing").unwrap()
}

#[test]
fn classify_script_disables_excludes_matching_objects_only() {
    let disabled = classify_one("disable_me", &["disable_me"], |_| ());
    assert!(disabled.script_disabled);
    assert!(disabled.ignore_by_default);

    let untouched = classify_one("harmless", &["disable_me"], |_| ());
    assert!(!untouched.script_disabled);
    assert!(!untouched.ignore_by_default);

    // Matching is case-insensitive, like every other id comparison on the load path.
    let cased = classify_one("Disable_Me", &["DISABLE_ME"], |_| ());
    assert!(cased.script_disabled);

    // A scriptless object can never be classified, whatever the set contains.
    let scriptless = classify_one("", &["disable_me", ""], |_| ());
    assert!(!scriptless.script_disabled);
    assert!(!scriptless.ignore_by_default);
}

#[test]
fn classify_script_disables_never_overrides_a_force_include() {
    // `dynamic_visibility` / `unique_object` and `include_objects` both land as
    // `force_mesh_generation`, and both must survive a script disable.
    let forced = classify_one("disable_me", &["disable_me"], |definition| {
        definition.force_mesh_generation = true;
    });
    assert!(forced.script_disabled, "the classification is still recorded");
    assert!(!forced.ignore_by_default, "but it must not exclude a force-included object");
}

#[test]
fn merge_unions_disable_scripts_across_the_load_order() {
    // The D4 regression: a script declared in one plugin classifying an object declared in
    // another. `objects.extend` makes the later plugin's definition win, so a per-plugin script
    // set would silently drop the classification.
    let args = make_args();
    let overrides = StaticOverrides::default();

    let mut declares_script: UsageInfo<'static> = UsageInfo::default();
    declares_script.disable_scripts.insert(UString::new("disable_me"));

    let mut declares_object: UsageInfo<'static> = UsageInfo::default();
    declares_object.objects.insert(
        "thing".to_owned(),
        UsageInfo::build_object_definition("thing", "disable_me", "mesh.nif", ObjectKind::Activator, &args, &overrides)
            .unwrap(),
    );

    let mut merged = declares_script;
    merged.merge(declares_object);
    merged.classify_script_disables();

    assert!(merged.objects["thing"].script_disabled);
    assert!(merged.objects["thing"].ignore_by_default);
}

#[test]
fn classify_script_disables_flags_arrow_targets_without_excluding_the_object() {
    let target = classify_one("", &[], |_| ());
    assert!(!target.disable_target, "nothing is a target with an empty set");

    let args = make_args();
    let overrides = StaticOverrides::default();
    let mut usage: UsageInfo<'static> = UsageInfo::default();
    usage.objects.insert(
        "thing".to_owned(),
        UsageInfo::build_object_definition("thing", "", "mesh.nif", ObjectKind::Static, &args, &overrides).unwrap(),
    );
    // Ids are compared case-insensitively, as everywhere else on the load path.
    usage.disable_targets.insert(UString::new("THING"));
    usage.classify_script_disables();

    assert!(usage.objects["thing"].disable_target);
    // Rule B is per-reference (D2): the base object itself must stay includable.
    assert!(!usage.objects["thing"].ignore_by_default);
    assert!(!usage.objects["thing"].script_disabled);
}

/// Builds one disable-target object with a persistent and a temporary exterior reference, runs
/// `remap_references`, and returns the surviving reference keys.
fn remap_disable_target(exclude: bool, adjust: impl FnOnce(&mut ObjectDefinition<'static>)) -> Vec<u32> {
    let mut args = make_args();
    args.exclude_script_disable_targets = exclude;
    let overrides = StaticOverrides::default();

    let mut usage: UsageInfo<'static> = UsageInfo::default();
    usage.use_test_reference_source();
    usage.objects.insert(
        "thing".to_owned(),
        UsageInfo::build_object_definition("thing", "", "mesh.nif", ObjectKind::Static, &args, &overrides).unwrap(),
    );
    usage.disable_targets.insert(UString::new("thing"));
    usage.classify_script_disables();
    adjust(usage.objects.get_mut("thing").unwrap());

    for (index, persistent) in [(1_u32, true), (2, false)] {
        let mut reference = make_reference("thing", Vec3::ZERO);
        reference.persistent = persistent;
        usage.exterior_references_mut().insert(StableRefKey::test(index), reference);
    }
    usage.remap_references(&args, &overrides);

    let mut surviving: Vec<u32> = usage
        .exterior_references()
        .map(|references| references.keys().map(|key| key.index()).collect())
        .unwrap_or_default();
    surviving.sort_unstable();
    surviving
}

#[test]
fn remap_references_excludes_persistent_disable_targets_only() {
    // `X->Disable` resolves through Morrowind's records handler, which only holds references from
    // a plugin's persistent block. The temporary placement is unreachable and must survive.
    assert_eq!(remap_disable_target(true, |_| ()), [2]);
}

#[test]
fn remap_references_keeps_disable_targets_when_the_setting_is_off() {
    assert_eq!(remap_disable_target(false, |_| ()), [1, 2]);
}

#[test]
fn script_disable_report_records_both_rules() {
    let args = make_args();
    let overrides = StaticOverrides::default();

    let mut usage: UsageInfo<'static> = UsageInfo::default();
    usage.use_test_reference_source();
    usage.objects.insert(
        "scripted".to_owned(),
        UsageInfo::build_object_definition("scripted", "disable_me", "a.nif", ObjectKind::Activator, &args, &overrides)
            .unwrap(),
    );
    usage.objects.insert(
        "targeted".to_owned(),
        UsageInfo::build_object_definition("targeted", "", "b.nif", ObjectKind::Static, &args, &overrides).unwrap(),
    );
    usage.disable_scripts.insert(UString::new("disable_me"));
    // One target resolves to an object, one names something this load order does not declare.
    usage.disable_targets.insert(UString::new("targeted"));
    usage.disable_targets.insert(UString::new("never_declared"));
    usage.classify_script_disables();

    let mut persistent = make_reference("targeted", Vec3::ZERO);
    persistent.persistent = true;
    usage.exterior_references_mut().insert(StableRefKey::test(1), persistent);
    usage
        .exterior_references_mut()
        .insert(StableRefKey::test(2), make_reference("targeted", Vec3::ZERO));
    usage
        .exterior_references_mut()
        .insert(StableRefKey::test(3), make_reference("scripted", Vec3::ZERO));
    usage.remap_references(&args, &overrides);

    let report = &usage.script_disable;
    assert_eq!(report.self_disabling_scripts, 1);
    assert_eq!(report.objects_excluded_by_script, 1);
    assert_eq!(report.disable_targets_named, 2);
    assert_eq!(report.disable_targets_resolved, 1, "`never_declared` resolves to nothing");
    assert_eq!(report.references_excluded_as_disable_targets, 1, "only the persistent one");
    assert_eq!(report.excluded_disable_targets, ["targeted"]);
}

#[test]
fn remap_references_keeps_force_included_disable_targets() {
    // The `dynamic_visibility` / `unique_object` and `include_objects` escape hatches.
    assert_eq!(
        remap_disable_target(true, |definition| definition.force_mesh_generation = true),
        [1, 2]
    );
}

#[test]
fn merge_combines_door_meshes() {
    let mut a: UsageInfo<'static> = UsageInfo::default();
    a.door_meshes.insert("door_a.nif");
    let mut b: UsageInfo<'static> = UsageInfo::default();
    b.door_meshes.insert("door_b.nif");

    a.merge(b);

    assert!(a.door_meshes.contains("door_a.nif"));
    assert!(a.door_meshes.contains("door_b.nif"));
}

#[test]
fn remap_references_keeps_dynamic_named_disable_but_drops_ignored_shared_mesh() {
    let mut usage: UsageInfo<'static> = UsageInfo::default();
    usage.objects.insert(
        "forced".to_string(),
        ObjectDefinition {
            mesh: "shared.nif",
            ignore_by_default: false,
            force_mesh_generation: true,
            vis_index: 0,
            kind: ObjectKind::Static,
            script_id: UString::new(""),
            script_disabled: false,
            disable_target: false,
        },
    );
    usage.objects.insert(
        "ignored".to_string(),
        ObjectDefinition {
            mesh: "shared.nif",
            ignore_by_default: true,
            force_mesh_generation: false,
            vis_index: 0,
            kind: ObjectKind::Static,
            script_id: UString::new(""),
            script_disabled: false,
            disable_target: false,
        },
    );
    usage.objects.insert(
        "dynamic".to_string(),
        ObjectDefinition {
            mesh: "dynamic.nif",
            ignore_by_default: false,
            force_mesh_generation: true,
            vis_index: 9,
            kind: ObjectKind::Static,
            script_id: UString::new(""),
            script_disabled: false,
            disable_target: false,
        },
    );
    usage.exterior_references_mut().extend([
        (
            StableRefKey::test(1),
            DistantReference {
                id: Cow::Owned("forced".to_string()),
                deleted: false,
                persistent: false,
                translation: Vec3::ZERO,
                rotation: Vec3::ZERO,
                scale: 1.0,
                vis_index: 0,
            },
        ),
        (
            StableRefKey::test(2),
            DistantReference {
                id: Cow::Owned("ignored".to_string()),
                deleted: false,
                persistent: false,
                translation: Vec3::ZERO,
                rotation: Vec3::ZERO,
                scale: 1.0,
                vis_index: 0,
            },
        ),
        (
            StableRefKey::test(3),
            DistantReference {
                id: Cow::Owned("dynamic".to_string()),
                deleted: false,
                persistent: false,
                translation: Vec3::ZERO,
                rotation: Vec3::ZERO,
                scale: 1.0,
                vis_index: 0,
            },
        ),
    ]);

    let mut overrides = StaticOverrides::default();
    overrides.names.insert("dynamic".to_string(), false);

    usage.remap_references(&make_args(), &overrides);
    let exterior = usage.exterior_references().unwrap();

    assert_eq!(exterior.len(), 2);
    assert!(exterior.values().all(|reference| reference.id.as_ref() != "ignored"));
    assert!(exterior.values().any(|reference| reference.id.as_ref() == "shared.nif"));
    assert!(
        exterior
            .values()
            .any(|reference| reference.id.as_ref() == "dynamic.nif" && reference.vis_index == 9)
    );
}

#[test]
fn explicit_interior_overrides_beat_heuristics() {
    let mut usage: UsageInfo<'static> = UsageInfo::default();
    usage.cells.insert("\0".to_string(), Default::default());
    usage.cells.insert("Enabled Cell".to_string(), Default::default());
    usage.cells.insert("Disabled Cell".to_string(), Default::default());
    usage.interior_metadata.insert(
        "Disabled Cell".to_string().into(),
        InteriorMetadata {
            behaves_like_exterior: true,
            has_water: true,
            water_height: 0.0,
        },
    );

    let mut overrides = StaticOverrides::default();
    overrides.interiors.insert("enabled cell".into(), true);
    overrides.interiors.insert("disabled cell".into(), false);

    let mut args = make_args();
    args.include_behaves_like_exterior = true;
    args.include_interiors_with_water = true;

    usage.filter_interiors(&args, &overrides);

    assert!(usage.cells.contains_key("Enabled Cell"));
    assert!(!usage.cells.contains_key("Disabled Cell"));
}

#[test]
fn global_grass_density_one_keeps_all_grass_references() {
    let mut usage = make_usage_with_references("grass", "grass\\flora.nif", 8, |_| {});

    usage.remap_references(&make_args(), &StaticOverrides::default());

    let exterior = usage.exterior_references().unwrap();
    assert_eq!(exterior.len(), 8);
    assert!(exterior.values().all(|reference| reference.id.as_ref() == "grass\\flora.nif"));
}

#[test]
fn global_grass_density_zero_removes_non_overridden_grass_references() {
    let mut usage = make_usage_with_references("grass", "grass\\flora.nif", 8, |_| {});
    let mut args = make_args();
    args.grass_density = 0.0;

    usage.remap_references(&args, &StaticOverrides::default());

    assert!(usage.exterior_references().is_none_or(|references| references.is_empty()));
}

#[test]
fn global_grass_density_half_is_deterministic() {
    let mut first = make_usage_with_references("grass", "grass\\flora.nif", 64, |_| {});
    let mut second = make_usage_with_references("grass", "grass\\flora.nif", 64, |_| {});
    let mut args = make_args();
    args.grass_density = 0.5;

    first.remap_references(&args, &StaticOverrides::default());
    second.remap_references(&args, &StaticOverrides::default());

    let mut first_keys: Vec<_> = first.exterior_references().unwrap().keys().copied().collect();
    let mut second_keys: Vec<_> = second.exterior_references().unwrap().keys().copied().collect();
    first_keys.sort_unstable();
    second_keys.sort_unstable();

    assert_eq!(first_keys, second_keys);
    assert!(!first_keys.is_empty());
    assert!(first_keys.len() < 64);
}

#[test]
fn grass_override_density_replaces_global_density() {
    let mut low_global = make_usage_with_references("override_grass", "flora\\override.nif", 64, |_| {});
    let mut high_global = make_usage_with_references("override_grass", "flora\\override.nif", 64, |_| {});

    let mut overrides = StaticOverrides::default();
    overrides.mesh_overrides.insert(
        "flora\\override.nif".to_string(),
        StaticOverride {
            static_type: StaticType::StaticGrass,
            density: 0.5,
            ..StaticOverride::default()
        },
    );

    let mut low_args = make_args();
    low_args.grass_density = 0.0;
    let mut high_args = make_args();
    high_args.grass_density = 1.0;

    low_global.remap_references(&low_args, &overrides);
    high_global.remap_references(&high_args, &overrides);

    let mut low_keys: Vec<_> = low_global.exterior_references().unwrap().keys().copied().collect();
    let mut high_keys: Vec<_> = high_global.exterior_references().unwrap().keys().copied().collect();
    low_keys.sort_unstable();
    high_keys.sort_unstable();

    assert_eq!(low_keys, high_keys);
    assert!(!low_keys.is_empty());
    assert!(low_keys.len() < 64);
}

#[test]
fn non_grass_statics_are_not_affected_by_grass_density() {
    let mut usage = make_usage_with_references("rock", "flora\\rock.nif", 8, |_| {});
    let mut args = make_args();
    args.grass_density = 0.0;

    usage.remap_references(&args, &StaticOverrides::default());

    let exterior = usage.exterior_references().unwrap();
    assert_eq!(exterior.len(), 8);
    assert!(exterior.values().all(|reference| reference.id.as_ref() == "flora\\rock.nif"));
}

#[test]
fn grass_detection_supports_mesh_prefix_and_override_type() {
    let mut usage = UsageInfo::default();
    usage.use_test_reference_source();
    usage.objects.insert(
        "mesh_grass".to_string(),
        ObjectDefinition {
            mesh: "grass\\mesh.nif",
            ignore_by_default: false,
            force_mesh_generation: false,
            vis_index: 0,
            kind: ObjectKind::Static,
            script_id: UString::new(""),
            script_disabled: false,
            disable_target: false,
        },
    );
    usage.objects.insert(
        "override_grass".to_string(),
        ObjectDefinition {
            mesh: "flora\\override.nif",
            ignore_by_default: false,
            force_mesh_generation: false,
            vis_index: 0,
            kind: ObjectKind::Static,
            script_id: UString::new(""),
            script_disabled: false,
            disable_target: false,
        },
    );
    usage
        .exterior_references_mut()
        .insert(StableRefKey::test(1), make_reference("mesh_grass", Vec3::ZERO));
    usage.exterior_references_mut().insert(
        StableRefKey::test(2),
        make_reference("override_grass", Vec3::new(128.0, 0.0, 0.0)),
    );

    let mut overrides = StaticOverrides::default();
    overrides.mesh_overrides.insert(
        "flora\\override.nif".to_string(),
        StaticOverride {
            static_type: StaticType::StaticGrass,
            ..StaticOverride::default()
        },
    );

    let mut args = make_args();
    args.grass_density = 0.0;
    usage.remap_references(&args, &overrides);

    assert!(usage.exterior_references().is_none_or(|references| references.is_empty()));
}

#[test]
fn static_mesh_and_usage_payloads_agree_on_static_count() {
    let statics: crate::PackedDistantStatics = [
        (
            "a.nif".to_owned(),
            crate::mge_xe::distant_statics::PackedDistantStatic::default(),
        ),
        (
            "b.nif".to_owned(),
            crate::mge_xe::distant_statics::PackedDistantStatic::default(),
        ),
    ]
    .into_iter()
    .collect();

    let mesh_bytes = crate::mge_xe::serialize_static_meshes(&statics).unwrap();
    let mesh_count = crate::mge_xe::distant_statics::deserialize_static_meshes(&mesh_bytes)
        .unwrap()
        .len() as u32;
    let ordinals = crate::StaticOrdinalView::from_packed(&statics);
    let usage_bytes = crate::serialize_usage_data(
        &crate::UsageInfo::default(),
        &ordinals,
        &crate::DynamicVisData::default(),
        150.0,
    )
    .unwrap();
    let usage_count = u32::from_le_bytes(usage_bytes[..4].try_into().unwrap());

    assert_eq!(mesh_count, usage_count);
}

#[test]
fn deep_water_filter_removes_statics_but_keeps_grass() {
    let mut usage = UsageInfo::default();
    usage.exterior_references_mut().extend([
        (
            StableRefKey::test(1),
            make_reference("static.nif", Vec3::new(0.0, 0.0, -256.0)),
        ),
        (
            StableRefKey::test(2),
            make_reference("grass.nif", Vec3::new(0.0, 0.0, -256.0)),
        ),
    ]);
    let bounds = BoundingBox {
        min: Vec3::splat(-1.0),
        max: Vec3::splat(1.0),
    };

    usage.discard_deep_water_references(128.0, |key| {
        let static_type = match key {
            "static.nif" => StaticType::StaticAuto,
            "grass.nif" => StaticType::StaticGrass,
            _ => return None,
        };
        Some((static_type, &bounds))
    });

    let exterior = usage.exterior_references().unwrap();
    assert_eq!(exterior.len(), 1);
    assert_eq!(exterior.values().next().unwrap().id, "grass.nif");
}

#[test]
fn deep_water_filter_respects_custom_cull_depth() {
    let bounds = BoundingBox {
        min: Vec3::splat(-1.0),
        max: Vec3::splat(1.0),
    };
    let make_test_usage = || {
        let mut usage = UsageInfo::default();
        usage.exterior_references_mut().extend([
            (
                StableRefKey::test(1),
                make_reference("static.nif", Vec3::new(0.0, 0.0, -256.0)),
            ),
            (
                StableRefKey::test(2),
                make_reference("grass.nif", Vec3::new(0.0, 0.0, -256.0)),
            ),
        ]);
        usage
    };

    let static_info = |key: &str| {
        let static_type = match key {
            "static.nif" => StaticType::StaticAuto,
            "grass.nif" => StaticType::StaticGrass,
            _ => return None,
        };
        Some((static_type, &bounds))
    };

    // With depth 128.0: static at Z=-256 (max Z=-255) is below exterior -128.0 threshold, so culled. Grass is kept.
    let mut usage_shallow = make_test_usage();
    usage_shallow.discard_deep_water_references(128.0, static_info);
    let exterior_shallow = usage_shallow.exterior_references().unwrap();
    assert_eq!(exterior_shallow.len(), 1);
    assert_eq!(exterior_shallow.values().next().unwrap().id, "grass.nif");

    // With depth 300.0: threshold is -300.0, static at Z=-256 (max Z=-255) is kept. Grass is kept.
    let mut usage_deep = make_test_usage();
    usage_deep.discard_deep_water_references(300.0, static_info);
    let exterior_deep = usage_deep.exterior_references().unwrap();
    assert_eq!(exterior_deep.len(), 2);
}

#[test]
fn buried_reference_filter_returns_its_aggregate_stats() {
    let mut usage = UsageInfo::default();
    usage
        .exterior_references_mut()
        .insert(StableRefKey::test(1), make_reference("mesh.nif", Vec3::ZERO));

    let stats = usage.discard_low_visibility_references(
        |_| Some(StaticType::StaticAuto),
        |_, _, stats| {
            stats.refs_considered += 1;
            stats.buried += 1;
            true
        },
    );

    assert_eq!(stats.refs_considered, 1);
    assert_eq!(stats.buried, 1);
    assert_eq!(usage.exterior_references_count(), 0);
}
