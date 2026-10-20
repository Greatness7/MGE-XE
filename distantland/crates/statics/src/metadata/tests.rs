use uncased::UncasedStr;

use super::*;
use crate::overrides::StaticOverrides;

/// Parses metadata text and applies it to a fresh single-source builder.
fn parse_and_apply(text: &str) -> StaticOverrides {
    let metadata = parse_distantland_section(text).expect("parse").expect("section");
    let mut builder = OverridesBuilder::new();
    builder.begin_source("<test metadata>");
    metadata.apply(&mut builder);
    builder.finish()
}

#[test]
fn metadata_path_strips_real_extension() {
    assert_eq!(plugin_metadata_path(Path::new("Foo.esp")), Path::new("Foo-metadata.toml"));
    assert_eq!(
        plugin_metadata_path(Path::new("My.Mod.esp")),
        Path::new("My.Mod-metadata.toml")
    );
    assert_eq!(
        plugin_metadata_path(Path::new("mod.omwaddon")),
        Path::new("mod-metadata.toml")
    );
    assert_eq!(
        plugin_metadata_path(&Path::new("dir").join("Foo.esm")),
        Path::new("dir").join("Foo-metadata.toml")
    );
}

#[test]
fn discovery_returns_existing_files_in_plugin_order() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("A.esp");
    let b = dir.path().join("B.esp");
    let c = dir.path().join("C.esp");
    let a_meta = dir.path().join("A-metadata.toml");
    let b_meta = dir.path().join("B-metadata.toml");
    fs::write(&a_meta, "").unwrap();
    fs::write(&b_meta, "").unwrap();

    let found = discover_plugin_metadata(&[b, a, c]);

    assert_eq!(found, vec![b_meta, a_meta]);
}

#[test]
fn missing_section_is_none() {
    assert!(parse_distantland_section("[package]\nname = \"Foo\"\n").unwrap().is_none());
}

#[test]
fn other_tool_sections_are_ignored() {
    let text = "[tools.mwse]\nlua-mod = \"foo\"\n\n[tools.mge-xe.distantland]\nexclude_objects = [\"x\"]\n";
    let metadata = parse_distantland_section(text).unwrap().unwrap();
    assert_eq!(metadata.exclude_objects, vec!["x"]);
}

#[test]
fn utf8_bom_is_tolerated() {
    let text = "\u{feff}[tools.mge-xe.distantland]\nexclude_objects = [\"x\"]\n";
    assert!(parse_distantland_section(text).unwrap().is_some());
}

#[test]
fn syntax_error_is_reported() {
    assert!(parse_distantland_section("not [valid toml").is_err());
}

#[test]
fn malformed_section_is_reported() {
    assert!(parse_distantland_section("[tools.mge-xe]\ndistantland = 5\n").is_err());
}

#[test]
fn apply_plugin_metadata_skips_invalid_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Bad-metadata.toml");
    fs::write(&path, "not [valid toml").unwrap();

    let mut builder = OverridesBuilder::new();
    let identity = apply_plugin_metadata_with_identity(&path, &mut builder).unwrap();

    assert_eq!(builder.finish(), StaticOverrides::default());
    assert_eq!(identity.path, path);
    assert_eq!(
        identity.content,
        distantland_foundation::identity::ContentIdentity::from_bytes(b"not [valid toml")
    );
}

#[test]
fn unknown_fields_are_ignored() {
    let text = r#"
[tools.mge-xe.distantland]
future_field = true

[tools.mge-xe.distantland.statics]
'foo.nif' = { type = "tree", future = 1 }
"#;
    let overrides = parse_and_apply(text);
    assert_eq!(overrides.mesh_overrides["foo.nif"].static_type, StaticType::StaticTree);
}

#[test]
fn structured_static_fields_map_to_override() {
    let text = r#"
[tools.mge-xe.distantland.statics]
'Data Files\Meshes\Foo\Rock.NIF' = { type = "very_far", reduction = 50 }
'foo/fern.nif' = { type = "grass", grass_density = 40 }
'foo\marker.nif' = { ignore = true }
'foo\shack.nif' = { type = "building", ignore_script = true }
"#;
    let overrides = parse_and_apply(text);

    let rock = &overrides.mesh_overrides["foo\\rock.nif"];
    assert_eq!(rock.static_type, StaticType::StaticVeryFar);
    assert_eq!(rock.simplify, Some(0.5));

    let fern = &overrides.mesh_overrides["foo\\fern.nif"];
    assert_eq!(fern.static_type, StaticType::StaticGrass);
    assert!((fern.density - 0.4).abs() < f32::EPSILON);

    assert!(overrides.mesh_overrides["foo\\marker.nif"].ignore);

    let shack = &overrides.mesh_overrides["foo\\shack.nif"];
    assert_eq!(shack.static_type, StaticType::StaticBuilding);
    assert!(shack.no_script);
}

#[test]
fn percentages_are_clamped() {
    let text = r#"
[tools.mge-xe.distantland.statics]
'a.nif' = { type = "grass", grass_density = 150 }
'b.nif' = { reduction = -5 }
"#;
    let overrides = parse_and_apply(text);
    assert!((overrides.mesh_overrides["a.nif"].density - 1.0).abs() < f32::EPSILON);
    assert_eq!(overrides.mesh_overrides["b.nif"].simplify, Some(0.0));
}

#[test]
fn exclude_wins_when_object_listed_in_both() {
    let text = r#"
[tools.mge-xe.distantland]
include_objects = ["Foo_Ref"]
exclude_objects = ["foo_ref"]
"#;
    let overrides = parse_and_apply(text);
    assert!(!overrides.names["foo_ref"]);
}

#[test]
fn interior_lookups_are_case_insensitive() {
    let text = r#"
[tools.mge-xe.distantland]
include_interiors = ["Foo Cavern"]
exclude_interiors = ["Bad Cell"]
"#;
    let overrides = parse_and_apply(text);
    assert_eq!(overrides.interiors.get(UncasedStr::new("FOO CAVERN")), Some(&true));
    assert_eq!(overrides.interiors.get(UncasedStr::new("bad cell")), Some(&false));
}

#[test]
fn dynamic_visibility_groups_round_trip() {
    let text = r#"
[[tools.mge-xe.distantland.dynamic_visibility]]
kind = "journal"
script = "Foo_Script"
journal = "Foo_Journal"
ranges = [[50, 100], [200, 200]]

[[tools.mge-xe.distantland.dynamic_visibility]]
kind = "unique_object"
object = "Foo_Lighthouse"
linked_objects = ["Foo_Lamp"]
"#;
    let overrides = parse_and_apply(text);
    let data = &overrides.dynamic_vis;

    assert_eq!(data.groups.len(), 2);
    assert_eq!(data.groups[0].index, 1);
    assert_eq!(
        data.groups[0].kind,
        DynamicVisKind::Journal {
            journal_id: "foo_journal".to_owned(),
            ranges: SmallVec::from_slice(&[(50, 101), (200, 201)]),
        }
    );
    assert_eq!(data.scripts["foo_script"], 1);

    assert_eq!(data.groups[1].index, 2);
    assert_eq!(
        data.groups[1].kind,
        DynamicVisKind::UniqueObject {
            source_id: "foo_lighthouse".to_owned(),
            linked_ids: vec!["foo_lighthouse".to_owned(), "foo_lamp".to_owned()],
        }
    );
    assert_eq!(data.unique_objects["foo_lighthouse"], 2);
    assert_eq!(data.unique_objects["foo_lamp"], 2);
}

#[test]
fn ranges_beyond_max_are_truncated() {
    let ranges: Vec<[i32; 2]> = (0..9).map(|i| [i, i + 1]).collect();
    assert_eq!(convert_ranges(&ranges, "<test>").len(), MAX_RANGES);
}

#[test]
fn configured_toml_sources_are_strict_and_apply_inclusively() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("global.toml");
    fs::write(
        &path,
        r#"
[[tools.mge-xe.distantland.dynamic_visibility]]
kind = "global"
script = "test_script"
global = "test_global"
ranges = [[1, 1]]
"#,
    )
    .unwrap();

    let mut builder = OverridesBuilder::new();
    let identity = apply_override_source_with_identity(&path, &mut builder).unwrap();
    let overrides = builder.finish();

    assert_eq!(identity.path, path);
    assert!(matches!(
        &overrides.dynamic_vis.groups[0].kind,
        DynamicVisKind::Global { ranges, .. } if ranges.as_slice() == [(1, 2)]
    ));

    let invalid = dir.path().join("invalid.toml");
    fs::write(&invalid, "[other-tool]").unwrap();
    assert!(apply_override_source_with_identity(&invalid, &mut OverridesBuilder::new()).is_err());
}

#[test]
fn configured_toml_source_sets_the_water_names_and_plugin_metadata_replaces_them() {
    let dir = tempfile::tempdir().unwrap();
    let global = dir.path().join("global.toml");
    fs::write(
        &global,
        r#"
[tools.mge-xe.distantland]
exclude_objects = ["x"]

[tools.mge-xe.distantland.water]
version = 1
surface_names = ["WaterVolume"]
body_names = ["WaterBody"]
plain_words = ["plain"]
sky_only_words = ["skyonly"]
"#,
    )
    .unwrap();

    let silent = dir.path().join("Silent-metadata.toml");
    fs::write(&silent, "[tools.mge-xe.distantland]\nexclude_objects = [\"y\"]\n").unwrap();
    let pond = dir.path().join("Pond-metadata.toml");
    fs::write(
        &pond,
        "[tools.mge-xe.distantland.water]\nversion = 1\nsurface_names = [\"Pond\"]\n",
    )
    .unwrap();
    let merged = |plugin_metadata: &[&Path]| {
        let mut builder = OverridesBuilder::new();
        apply_override_source_with_identity(&global, &mut builder).unwrap();
        for path in plugin_metadata {
            apply_plugin_metadata_with_identity(path, &mut builder).unwrap();
        }
        builder.finish()
    };

    // A plugin metadata file without the table leaves the names of the global file.
    let names = merged(&[&silent]).water_names;
    assert_eq!(names.surface, ["watervolume"]);
    assert_eq!(names.body, ["waterbody"]);
    assert_eq!(names.plain_words, ["plain"]);
    assert_eq!(names.sky_only_words, ["skyonly"]);

    // A plugin metadata file with the table replaces the whole table.
    let names = merged(&[&silent, &pond]).water_names;
    assert_eq!(names.surface, ["pond"]);
    assert!(names.body.is_empty());
    assert!(names.plain_words.is_empty());
    assert!(names.sky_only_words.is_empty());
}

#[test]
fn metadata_matches_equivalent_ovr() {
    let ovr = b"\
meshes\\foo\\rock.nif = very_far reduction_50
foo\\fern.nif = grass_40
[names]
special_ref = enable
chargen boat = disable
[interiors]
Foo Cavern = enable
Bad Cell = disable
[dynamic_vis]
foo_script = journal foo_journal 50-100
foo_glob = global fooGlobal 1-1
foo_lighthouse = unique_object foo_lamp
";
    let from_ovr = crate::parse_overrides_texts(&[ovr.as_slice()]).unwrap();

    let toml_text = r#"
[tools.mge-xe.distantland]
include_objects = ["special_ref"]
exclude_objects = ["chargen boat"]
include_interiors = ["Foo Cavern"]
exclude_interiors = ["Bad Cell"]

[tools.mge-xe.distantland.statics]
'meshes\foo\rock.nif' = { type = "very_far", reduction = 50 }
'foo\fern.nif' = { type = "grass", grass_density = 40 }

[[tools.mge-xe.distantland.dynamic_visibility]]
kind = "journal"
script = "foo_script"
journal = "foo_journal"
ranges = [[50, 100]]

[[tools.mge-xe.distantland.dynamic_visibility]]
kind = "global"
script = "foo_glob"
global = "fooGlobal"
ranges = [[1, 1]]

[[tools.mge-xe.distantland.dynamic_visibility]]
kind = "unique_object"
object = "foo_lighthouse"
linked_objects = ["foo_lamp"]
"#;
    let from_metadata = parse_and_apply(toml_text);

    assert_eq!(from_ovr, from_metadata);
}

#[test]
fn metadata_overrides_ovr_sources() {
    let mut builder = OverridesBuilder::new();
    builder.add_override_text(b"foo\\bar.nif = near").unwrap();

    let text = r#"
[tools.mge-xe.distantland.statics]
'foo\bar.nif' = { type = "very_far" }
"#;
    let metadata = parse_distantland_section(text).unwrap().unwrap();
    builder.begin_source("<metadata>");
    metadata.apply(&mut builder);

    let overrides = builder.finish();
    assert_eq!(
        overrides.mesh_overrides["foo\\bar.nif"].static_type,
        StaticType::StaticVeryFar
    );
}

#[test]
fn duplicate_dynamic_vis_groups_merge_across_sources() {
    let mut builder = OverridesBuilder::new();
    builder
        .add_override_text(b"[dynamic_vis]\nscript_a = journal foo_journal 50-100")
        .unwrap();

    let text = r#"
[[tools.mge-xe.distantland.dynamic_visibility]]
kind = "journal"
script = "script_b"
journal = "foo_journal"
ranges = [[50, 100]]
"#;
    let metadata = parse_distantland_section(text).unwrap().unwrap();
    builder.begin_source("<metadata>");
    metadata.apply(&mut builder);

    let overrides = builder.finish();
    assert_eq!(overrides.dynamic_vis.groups.len(), 1);
    assert_eq!(overrides.dynamic_vis.scripts["script_a"], 1);
    assert_eq!(overrides.dynamic_vis.scripts["script_b"], 1);
}

#[test]
fn water_table_and_water_key_map_to_overrides() {
    let text = r#"
[tools.mge-xe.distantland.water]
version = 1
surface_names = ["WaterVolume"]
body_names = ["WaterBody"]
plain_words = ["plain"]
sky_only_words = ["SkyOnly"]

[tools.mge-xe.distantland.statics]
'x\ex_pond.nif' = { water = true, water_color = [0.25, 0.5, 2.0] }
'wv\not_water.nif' = { water = false }
'foo\rock.nif' = { type = "far" }
"#;
    let overrides = parse_and_apply(text);

    assert_eq!(overrides.water_names.surface, ["watervolume"]);
    assert_eq!(overrides.water_names.body, ["waterbody"]);
    assert_eq!(overrides.water_names.plain_words, ["plain"]);
    assert_eq!(overrides.water_names.sky_only_words, ["skyonly"]);
    assert_eq!(overrides.mesh_overrides["x\\ex_pond.nif"].water, Some(true));
    // A part of a colour is held between 0 and 1.
    assert_eq!(overrides.mesh_overrides["x\\ex_pond.nif"].water_color, Some([0.25, 0.5, 1.0]));
    assert_eq!(overrides.mesh_overrides["wv\\not_water.nif"].water, Some(false));
    assert_eq!(overrides.mesh_overrides["wv\\not_water.nif"].water_color, None);
    assert_eq!(overrides.mesh_overrides["foo\\rock.nif"].water, None);
}

#[test]
fn mask_names_and_the_dry_space_key_map_to_overrides() {
    let text = r#"
[tools.mge-xe.distantland.water]
version = 1
surface_names = ["WaterVolume"]
mask_names = ["WaterMask", " Dry "]

[tools.mge-xe.distantland.statics]
'x\boat.nif' = { dry_space = false }
'x\ship.nif' = { dry_space = true }
'x\raft.nif' = { type = "far" }
"#;
    let overrides = parse_and_apply(text);

    assert_eq!(overrides.water_names.mask, ["watermask", "dry"]);
    assert_eq!(overrides.mesh_overrides["x\\boat.nif"].dry_space, Some(false));
    assert_eq!(overrides.mesh_overrides["x\\ship.nif"].dry_space, Some(true));
    assert_eq!(overrides.mesh_overrides["x\\raft.nif"].dry_space, None);

    // An empty list, and a table without the key, name no dry space.
    let text = r#"
[tools.mge-xe.distantland.water]
version = 1
surface_names = ["WaterVolume"]
mask_names = []
"#;
    assert!(parse_and_apply(text).water_names.mask.is_empty());
    let text = r#"
[tools.mge-xe.distantland.water]
version = 1
surface_names = ["WaterVolume"]
"#;
    assert!(parse_and_apply(text).water_names.mask.is_empty());
}

#[test]
fn water_table_of_an_unknown_version_is_left_out() {
    let text = r#"
[tools.mge-xe.distantland.water]
version = 99
surface_names = ["WaterVolume"]

[tools.mge-xe.distantland.statics]
'x\ex_pond.nif' = { water = true }
"#;
    let overrides = parse_and_apply(text);

    assert!(overrides.water_names.surface.is_empty());
    // The mesh entries of the same file still count.
    assert_eq!(overrides.mesh_overrides["x\\ex_pond.nif"].water, Some(true));
}

#[test]
fn wv_key_maps_to_the_look_line_of_the_mesh_override() {
    let too_long = "x".repeat(256);
    let text = format!(
        r#"
[tools.mge-xe.distantland.statics]
'x\pond.nif' = {{ water = true, wv = "flow=0,-140 glow=0.3" }}
'x\prefix.nif' = {{ wv = " WV: opacity=vertex  " }}
'x\empty.nif' = {{ wv = "" }}
'x\prefix_only.nif' = {{ wv = "wv:" }}
'x\no_key.nif' = {{ water = true }}
'x\too_long.nif' = {{ water = true, wv = "{too_long}" }}
'x\not_ascii.nif' = {{ water = true, wv = "shader=caf\u00e9" }}
"#
    );
    let overrides = parse_and_apply(&text);
    let look = |mesh: &str| overrides.mesh_overrides[mesh].water_look.as_deref();

    assert_eq!(look("x\\pond.nif"), Some("flow=0,-140 glow=0.3"));
    // The prefix and the white space at the ends are taken away.
    assert_eq!(look("x\\prefix.nif"), Some("opacity=vertex"));
    // An empty look line is a look line: it takes the look line of the mesh away.
    assert_eq!(look("x\\empty.nif"), Some(""));
    assert_eq!(look("x\\prefix_only.nif"), Some(""));
    assert_eq!(look("x\\no_key.nif"), None);
    // A look line that the file format does not take is left out. The rest of the entry stays.
    assert_eq!(look("x\\too_long.nif"), None);
    assert_eq!(look("x\\not_ascii.nif"), None);
    assert_eq!(overrides.mesh_overrides["x\\too_long.nif"].water, Some(true));
    assert_eq!(overrides.mesh_overrides["x\\not_ascii.nif"].water, Some(true));
}

#[test]
fn wv_key_takes_a_look_line_of_255_bytes() {
    let longest = "x".repeat(255);
    let text = format!("[tools.mge-xe.distantland.statics]\n'x\\pond.nif' = {{ wv = \"wv: {longest}\" }}\n");
    let overrides = parse_and_apply(&text);
    assert_eq!(
        overrides.mesh_overrides["x\\pond.nif"].water_look.as_deref(),
        Some(longest.as_str())
    );
}

#[test]
fn a_later_mesh_entry_replaces_the_look_line_with_the_whole_entry() {
    let apply = |builder: &mut OverridesBuilder, label: &str, entry: &str| {
        let text = format!("[tools.mge-xe.distantland.statics]\n'x\\pond.nif' = {{ {entry} }}\n");
        let metadata = parse_distantland_section(&text).expect("parse").expect("section");
        builder.begin_source(label);
        metadata.apply(builder);
    };

    let mut builder = OverridesBuilder::new();
    apply(&mut builder, "A-metadata.toml", "water = true, wv = \"speed=2\"");
    apply(&mut builder, "B-metadata.toml", "water = true, wv = \"speed=3\"");
    let overrides = builder.finish();
    assert_eq!(overrides.mesh_overrides["x\\pond.nif"].water_look.as_deref(), Some("speed=3"));

    // An entry is one value, as for the other keys: a later entry without the key has no look
    // line.
    let mut builder = OverridesBuilder::new();
    apply(&mut builder, "A-metadata.toml", "water = true, wv = \"speed=2\"");
    apply(&mut builder, "B-metadata.toml", "water = true");
    let overrides = builder.finish();
    assert_eq!(overrides.mesh_overrides["x\\pond.nif"].water, Some(true));
    assert_eq!(overrides.mesh_overrides["x\\pond.nif"].water_look, None);
}
