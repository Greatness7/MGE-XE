//! Cross-crate coverage for dedicated grass-plugin usage loading.

use std::path::PathBuf;

use distantland::{
    DistantReference, StaticOverrides, UsageFilterOptions, UsageInfo, Vfs, VfsLoadOptions, classify_grass_plugins,
    is_grass_plugin,
};
use distantland_test_support::{
    BASELINE_WORLD_V1_GRASSLIST, BASELINE_WORLD_V1_MAINGRASS, FIXTURE_GRASS_PLUGIN_NAME, FIXTURE_PLUGIN_NAME,
    FIXTURE_SECOND_GRASS_PLUGIN_NAME, build_hermetic_fixture,
};
use itertools::Itertools;

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

    assert_eq!(
        grass_warnings.iter().map(|warning| warning.code.as_str()).collect_vec(),
        ["grass_plugin_master_not_in_list"]
    );
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
    assert!(is_grass_plugin(&fixture.data_dir.join(FIXTURE_GRASS_PLUGIN_NAME)).unwrap());
    // Gate A: an ordinary plugin defines no grass static, so it is rejected without decoding cells.
    assert!(!is_grass_plugin(&fixture.data_dir.join(FIXTURE_PLUGIN_NAME)).unwrap());
    // The batch form answers positionally, which is the whole contract its callers index by. A
    // missing file classifies `false` rather than failing the run alongside it.
    assert_eq!(
        classify_grass_plugins(&[
            fixture.data_dir.join(FIXTURE_PLUGIN_NAME),
            fixture.data_dir.join(FIXTURE_GRASS_PLUGIN_NAME),
            fixture.data_dir.join("absent.esp"),
        ]),
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
