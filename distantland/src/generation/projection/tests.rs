use std::borrow::Cow;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use glam::Vec3;
use tempfile::tempdir;

use super::*;
use crate::statics::metadata::apply_plugin_metadata_with_identity;
use crate::terrain::texture::{TerrainCell, TerrainColors, TerrainNormals};
use crate::test_support::{
    BASELINE_WORLD_V1, BASELINE_WORLD_V1_GRASSLIST, build_hermetic_fixture, dialogue_only_plugin, hermetic_generation_job,
};
use crate::vfs::VfsLoadOptions;
use crate::{DistantReference, IndexMap, ObjectDefinition, OverridesBuilder, StableRefKey, UsageFilterOptions, Vfs};

fn height_only_cell(grid: (i32, i32), fill: f32) -> TerrainCell<'static> {
    TerrainCell {
        grid,
        heights: Box::new([[fill; 65]; 65]),
        normals: Default::default(),
        colors: Default::default(),
        texture_indices: Box::new([[0; 16]; 16]),
        texture_table: Arc::new(IndexMap::default()),
    }
}

fn capture_job(job: &crate::GenerationJob) -> Projections {
    let vfs = Vfs::load(&VfsLoadOptions {
        morrowind_ini: job.morrowind_ini.clone(),
        data_dirs: job.data_dirs.clone(),
        plugins: job.plugins.clone(),
    })
    .unwrap();
    let grass_plugins =
        crate::vfs::resolve_selected_plugins(job.grass_plugins.as_deref().unwrap_or_default(), vfs.data_dirs()).unwrap();
    let overrides = StaticOverrides::default();
    let usage_options = UsageFilterOptions::from(&job.settings);
    let (usage, _, _, _, mut projections) =
        UsageInfo::setup_with_grass_plugins_and_capture(&vfs, &grass_plugins, &usage_options, &overrides, |usage| {
            Projections::capture(usage, &job.settings, &overrides)
        })
        .unwrap();
    projections.capture_dedicated_grass(&usage, &grass_plugins, &job.settings, &overrides);
    projections
}

/// Canonical bytes of one settings projection, matching what `fingerprint_statics_global` folds
/// into `statics_global`.
fn settings_canonical_bytes(settings: &UsageSettingsProjection) -> Vec<u8> {
    let mut writer = CanonicalWriter::new();
    settings.write_canonical(&mut writer);
    writer.into_bytes()
}

fn object(mesh: &'static str) -> ObjectDefinition<'static> {
    ObjectDefinition {
        mesh,
        ignore_by_default: false,
        force_mesh_generation: false,
        vis_index: 0,
        kind: ObjectKind::Static,
        script_id: uncased::Uncased::new(""),
        script_disabled: false,
        disable_target: false,
    }
}

fn reference(id: &'static str, translation: Vec3) -> DistantReference<'static> {
    DistantReference {
        id: Cow::Borrowed(id),
        deleted: false,
        persistent: false,
        translation,
        rotation: Vec3::new(0.1, 0.2, 0.3),
        scale: 1.0,
        vis_index: 0,
    }
}

#[test]
fn repeated_fixture_capture_is_identical() {
    let temp = tempdir().unwrap();
    let fixture = build_hermetic_fixture(BASELINE_WORLD_V1, temp.path()).unwrap();
    let job = hermetic_generation_job(&fixture, &temp.path().join("output"));

    let first = capture_job(&job);
    let second = capture_job(&job);

    assert_eq!(first, second);
}

#[test]
fn dialogue_only_plugin_produces_identical_projection() {
    let temp = tempdir().unwrap();
    let fixture = build_hermetic_fixture(BASELINE_WORLD_V1, temp.path()).unwrap();
    let mut job = hermetic_generation_job(&fixture, &temp.path().join("output"));
    let baseline = capture_job(&job);

    let plugin_name = PathBuf::from("fixture-dialogue.esp");
    let mut plugin = dialogue_only_plugin("fixture_inserted_topic");
    plugin.save_path(fixture.data_dir.join(&plugin_name)).unwrap();
    job.plugins.as_mut().unwrap().insert(1, plugin_name);
    let with_dialogue = capture_job(&job);

    assert_eq!(baseline, with_dialogue);
}

#[test]
fn dedicated_grass_globals_are_list_order_independent() {
    let temp = tempdir().unwrap();
    let fixture = build_hermetic_fixture(BASELINE_WORLD_V1_GRASSLIST, temp.path()).unwrap();
    let mut job = hermetic_generation_job(&fixture, &temp.path().join("output"));
    let forward = capture_job(&job);

    job.grass_plugins.as_mut().unwrap().reverse();
    let reverse = capture_job(&job);

    assert!(!forward.grass_placements.is_empty());
    assert_eq!(forward, reverse);
}

/// Interior grass placements are only ever seen by the dedicated capture, so it must carry the
/// cell name: two runs differing only in which interior a placement sits in must not hash alike.
#[test]
fn dedicated_grass_captures_interior_placements_with_their_cell() {
    let temp = tempdir().unwrap();
    let fixture = build_hermetic_fixture(BASELINE_WORLD_V1_GRASSLIST, temp.path()).unwrap();
    let job = hermetic_generation_job(&fixture, &temp.path().join("output"));
    let projections = capture_job(&job);

    let interior_cells: BTreeSet<_> = projections
        .grass_placements
        .iter()
        .filter_map(|placement| placement.interior_cell.as_deref())
        .collect();
    assert_eq!(interior_cells, BTreeSet::from(["Fixture Grass Interior"]));
    assert!(
        projections
            .grass_placements
            .iter()
            .any(|placement| placement.interior_cell.is_none()),
        "exterior placements stay unnamed"
    );

    let mut renamed = projections.clone();
    for placement in &mut renamed.grass_placements {
        if placement.interior_cell.is_some() {
            placement.interior_cell = Some("Elsewhere".to_owned());
        }
    }
    let canonical = |placements: &[GrassPlacementProjection]| {
        let mut writer = CanonicalWriter::new();
        for placement in placements {
            placement.write_canonical(&mut writer);
        }
        writer.into_bytes()
    };
    assert_ne!(canonical(&projections.grass_placements), canonical(&renamed.grass_placements));
}

#[test]
fn boundary_move_changes_only_source_and_destination_buckets() {
    let mut usage = UsageInfo::default();
    usage.use_test_reference_source();
    usage.objects.insert("rock".to_owned(), object("flora\\rock.nif"));
    let key = StableRefKey::test(1);
    usage
        .exterior_references_mut()
        .insert(key, reference("rock", Vec3::new(8191.0, 0.0, 0.0)));
    let settings = GenerationSettings::default();
    let overrides = StaticOverrides::default();
    let before = Projections::capture(&usage, &settings, &overrides);

    usage.exterior_references_mut()[&key].translation.x = 8192.0;
    let after = Projections::capture(&usage, &settings, &overrides);
    let changed: BTreeSet<_> = before
        .exterior_cells
        .keys()
        .chain(after.exterior_cells.keys())
        .copied()
        .filter(|cell| before.exterior_cells.get(cell) != after.exterior_cells.get(cell))
        .collect();

    assert_eq!(changed, BTreeSet::from([(0, 0), (1, 0)]));
    assert_eq!(before.objects, after.objects);
    assert_eq!(before.landscapes, after.landscapes);
    assert_eq!(before.overrides, after.overrides);
}

#[test]
fn object_change_is_local_to_the_object_and_referencing_bucket() {
    let mut usage = UsageInfo::default();
    usage.use_test_reference_source();
    usage.objects.insert("a".to_owned(), object("flora\\a.nif"));
    usage.objects.insert("b".to_owned(), object("flora\\b.nif"));
    usage
        .exterior_references_mut()
        .insert(StableRefKey::test(1), reference("a", Vec3::new(0.0, 0.0, 0.0)));
    usage
        .exterior_references_mut()
        .insert(StableRefKey::test(2), reference("b", Vec3::new(8192.0, 0.0, 0.0)));
    let settings = GenerationSettings::default();
    let overrides = StaticOverrides::default();
    let before = Projections::capture(&usage, &settings, &overrides);

    usage.objects.get_mut("a").unwrap().mesh = "flora\\a_changed.nif";
    let after = Projections::capture(&usage, &settings, &overrides);
    let changed_objects: Vec<_> = before
        .objects
        .keys()
        .filter(|id| before.objects.get(*id) != after.objects.get(*id))
        .cloned()
        .collect();
    let changed_cells: Vec<_> = before
        .exterior_cells
        .keys()
        .filter(|cell| before.exterior_cells.get(*cell) != after.exterior_cells.get(*cell))
        .copied()
        .collect();

    assert_eq!(changed_objects, ["a"]);
    assert_eq!(changed_cells, [(0, 0)]);
}

#[test]
fn landscape_change_is_local_to_one_cell() {
    let mut usage = UsageInfo::default();
    usage.terrain_cells.insert((0, 0), height_only_cell((0, 0), 0.0));
    usage.terrain_cells.insert((1, 0), height_only_cell((1, 0), 1.0));
    let settings = GenerationSettings::default();
    let overrides = StaticOverrides::default();
    let before = Projections::capture(&usage, &settings, &overrides);

    usage.terrain_cells.get_mut(&(1, 0)).unwrap().heights[32][17] = f32::from_bits(0x7fc0_0042);
    let after = Projections::capture(&usage, &settings, &overrides);
    let changed: Vec<_> = before
        .landscapes
        .keys()
        .filter(|cell| before.landscapes.get(*cell) != after.landscapes.get(*cell))
        .copied()
        .collect();

    assert_eq!(changed, [(1, 0)]);
}

#[test]
fn height_mutation_changes_only_height_hash() {
    let mut usage = UsageInfo::default();
    usage.terrain_cells.insert((0, 0), height_only_cell((0, 0), 0.0));
    let settings = GenerationSettings::default();
    let overrides = StaticOverrides::default();
    let before = Projections::capture(&usage, &settings, &overrides)
        .landscapes
        .remove(&(0, 0))
        .expect("cell projection");

    usage.terrain_cells.get_mut(&(0, 0)).unwrap().heights[0][0] = 8.0;
    let after = Projections::capture(&usage, &settings, &overrides)
        .landscapes
        .remove(&(0, 0))
        .expect("cell projection");

    assert_ne!(before.height_hash, after.height_hash);
    assert_eq!(before.terrain_hash, after.terrain_hash);
    assert_eq!(before.material_hash, after.material_hash);
    assert_eq!(before.referenced_textures, after.referenced_textures);
}

#[test]
fn compact_terrain_hashes_match_prechange_interlock() {
    fn hash_hex(cell: &TerrainCell<'_>) -> String {
        hash_terrain_cell(cell).iter().map(|byte| format!("{byte:02x}")).collect()
    }

    let absent = height_only_cell((0, 0), 0.0);
    assert_eq!(
        hash_hex(&absent),
        "ef7a1701e699a1052aa6de48cf2906c8d1f39c0e0460049dca3ed853ec068a2b"
    );

    let mut present = height_only_cell((0, 0), 0.0);
    let mut present_normals = Box::new([[[0, 0, 1]; 65]; 65]);
    present_normals.as_flattened_mut()[0] = [-128, 1, 127];
    present.normals = TerrainNormals::Encoded(present_normals);
    let mut present_colors = Box::new([[[255; 3]; 65]; 65]);
    present_colors.as_flattened_mut()[0] = [0, 128, 255];
    present.colors = TerrainColors::Encoded(present_colors);
    assert_eq!(
        hash_hex(&present),
        "7a04eb435ea78c11d2d5ed64ac0103e4676e571a16faf48b751cc893f473293c"
    );

    let mut encoded_zero = height_only_cell((0, 0), 0.0);
    let mut zero_normals = Box::new([[[0, 0, 1]; 65]; 65]);
    zero_normals.as_flattened_mut()[0] = [0; 3];
    encoded_zero.normals = TerrainNormals::Encoded(zero_normals);
    let mut zero_colors = Box::new([[[255; 3]; 65]; 65]);
    zero_colors.as_flattened_mut()[0] = [0; 3];
    encoded_zero.colors = TerrainColors::Encoded(zero_colors);
    assert_eq!(
        hash_hex(&encoded_zero),
        "99b981dfdbd931d9dd5a39b3e6980680f7c25cd7bf69761740831fe3f35e0416"
    );

    let mut explicit_default = height_only_cell((0, 0), 0.0);
    explicit_default.normals = TerrainNormals::Encoded(Box::new([[[0, 0, 1]; 65]; 65]));
    explicit_default.colors = TerrainColors::Encoded(Box::new([[[255; 3]; 65]; 65]));
    assert_eq!(
        hash_hex(&explicit_default),
        "ef7a1701e699a1052aa6de48cf2906c8d1f39c0e0460049dca3ed853ec068a2b"
    );
}

#[test]
fn metadata_change_updates_only_its_effective_override_partition() {
    let temp = tempdir().unwrap();
    let metadata = temp.path().join("fixture-metadata.toml");
    let write_metadata = |density| {
        std::fs::write(
            &metadata,
            format!(
                "[tools.mge-xe.distantland.statics]\n'flora/fern.nif' = {{ type = \"grass\", grass_density = {density} }}\n"
            ),
        )
        .unwrap();
        let mut builder = OverridesBuilder::new();
        apply_plugin_metadata_with_identity(&metadata, &mut builder);
        builder.finish()
    };
    let usage = UsageInfo::default();
    let settings = GenerationSettings::default();
    let before = Projections::capture(&usage, &settings, &write_metadata(40));
    let after = Projections::capture(&usage, &settings, &write_metadata(60));

    assert_ne!(
        before.overrides.meshes["flora\\fern.nif"],
        after.overrides.meshes["flora\\fern.nif"]
    );
    assert_eq!(before.overrides.names, after.overrides.names);
    assert_eq!(before.overrides.interiors, after.overrides.interiors);
    assert_eq!(before.overrides.dynamic_visibility, after.overrides.dynamic_visibility);
}

#[test]
fn legacy_override_change_updates_only_its_effective_partition() {
    let usage = UsageInfo::default();
    let settings = GenerationSettings::default();
    let before = Projections::capture(&usage, &settings, &StaticOverrides::default());
    let mut overrides = StaticOverrides::default();
    overrides.names.insert("fixture_object".to_owned(), false);
    let after = Projections::capture(&usage, &settings, &overrides);

    assert_ne!(before.overrides.names, after.overrides.names);
    assert_eq!(before.overrides.meshes, after.overrides.meshes);
    assert_eq!(before.overrides.interiors, after.overrides.interiors);
    assert_eq!(before.overrides.dynamic_visibility, after.overrides.dynamic_visibility);
}

/// Every membership setting must reach `statics_global`, and from there `statics_domain_digest`,
/// through `UsageSettingsProjection`. The exhaustive destructure in its `From` forces a new setting
/// to be classified, but classifying one as ignored is still a judgment call, and getting it wrong
/// means a membership setting that silently fails to invalidate. That looks exactly like the
/// feature not working. This pins the ones that must stay captured.
#[test]
fn membership_settings_change_the_captured_settings_projection() {
    fn assert_flip_is_visible(name: &str, flip: impl FnOnce(&mut GenerationSettings)) {
        let usage = UsageInfo::default();
        let overrides = StaticOverrides::default();
        let baseline = GenerationSettings::default();
        let mut flipped = baseline.clone();
        flip(&mut flipped);

        let before = Projections::capture(&usage, &baseline, &overrides);
        let after = Projections::capture(&usage, &flipped, &overrides);
        assert_ne!(
            before.settings, after.settings,
            "flipping {name} must change the settings projection"
        );
        assert_ne!(
            settings_canonical_bytes(&before.settings),
            settings_canonical_bytes(&after.settings),
            "flipping {name} must change the canonical settings bytes"
        );
    }

    assert_flip_is_visible("exclude_script_disable_targets", |settings| {
        settings.exclude_script_disable_targets = !settings.exclude_script_disable_targets;
    });
    assert_flip_is_visible("include_activators", |settings| {
        settings.include_activators = !settings.include_activators;
    });
    assert_flip_is_visible("include_misc", |settings| settings.include_misc = !settings.include_misc);
    assert_flip_is_visible("include_behaves_like_exterior", |settings| {
        settings.include_behaves_like_exterior = !settings.include_behaves_like_exterior;
    });
    assert_flip_is_visible("include_interiors_with_water", |settings| {
        settings.include_interiors_with_water = !settings.include_interiors_with_water;
    });
    assert_flip_is_visible("include_large_interiors", |settings| {
        settings.include_large_interiors = !settings.include_large_interiors;
    });
    assert_flip_is_visible("deep_water_static_cull_depth", |settings| {
        settings.deep_water_static_cull_depth += 1.0;
    });
}
