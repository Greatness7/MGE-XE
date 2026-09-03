use std::borrow::Cow;

use super::*;
use crate::mge_xe::distant_statics::{BoundingBox, StaticType, UV_BOUND_PALETTE_CAP};
use crate::model::{SubsetTexture, UvBound, Vertex};
use glam::{Vec2, Vec4};
use tes3::nif::NiBound;

fn merge_exterior_references<'a>(
    distant_statics: &mut DistantStatics,
    usage_info: &mut UsageInfo<'a>,
    config: StaticMeshSimplifierConfig,
    max_group_radius: f32,
) -> MergeSimplificationMetrics {
    let plan = plan_exterior_merge_groups(distant_statics, usage_info, max_group_radius);
    apply_merge_usage(&plan, usage_info);
    build_merge_geometry_unpacked(&plan, &CellFilter::All, distant_statics, config, None)
}

fn make_static() -> DistantStatic {
    let mut ds = DistantStatic::default();
    ds.bounding_sphere = NiBound {
        center: [0.0, 0.0, 0.0].into(),
        radius: 64.0,
    };
    ds.bounding_box = BoundingBox {
        min: [-16.0, -16.0, -16.0].into(),
        max: [16.0, 16.0, 16.0].into(),
    };
    ds
}

/// [`make_static`] carrying real geometry, with identical bounds so grouping is unchanged.
///
/// Tests that inspect the merged record itself need this: a merge whose members contribute no
/// subsets produces an empty record, and empty records are dropped rather than emitted.
fn make_merge_source() -> DistantStatic {
    DistantStatic {
        subsets: make_grid_static(32.0, 0.0, 1.0).subsets,
        ..make_static()
    }
}

fn make_static_with_type(static_type: StaticType) -> DistantStatic {
    let mut ds = make_merge_source();
    ds.static_type = static_type;
    ds
}

fn make_grid_static(extent_xy: f32, extent_z: f32, uv_scale: f32) -> DistantStatic {
    const SIDE: u16 = 8;
    let mut vertices = Vec::with_capacity(SIDE as usize * SIDE as usize);
    for row in 0..SIDE {
        for col in 0..SIDE {
            let u = col as f32 / (SIDE - 1) as f32;
            let v = row as f32 / (SIDE - 1) as f32;
            vertices.push(crate::Vertex {
                position: Vec3::new((u - 0.5) * extent_xy, (v - 0.5) * extent_xy, v * extent_z),
                normal: Vec3::new(0.0, -extent_z, extent_xy).normalize_or_zero(),
                uv: Vec2::new(u, v) * uv_scale,
                color: Vec4::new(u, v, 1.0 - u, 1.0),
                ..crate::Vertex::default()
            });
        }
    }

    let mut triangles = Vec::new();
    for row in 0..(SIDE - 1) {
        for col in 0..(SIDE - 1) {
            let top_left = row * SIDE + col;
            let top_right = top_left + 1;
            let bottom_left = top_left + SIDE;
            let bottom_right = bottom_left + 1;
            triangles.push([top_left, top_right, bottom_left]);
            triangles.push([top_right, bottom_right, bottom_left]);
        }
    }

    let subset = Subset {
        vertices,
        triangles,
        texture: crate::SubsetTexture::AtlasPage(0),
        ..Subset::default()
    };
    let mut ds = DistantStatic {
        subsets: vec![subset],
        ..DistantStatic::default()
    };
    ds.update_bounds();
    ds
}

fn make_alpha_grid_static(extent_xy: f32, extent_z: f32, uv_scale: f32) -> DistantStatic {
    let mut ds = make_grid_static(extent_xy, extent_z, uv_scale);
    for subset in &mut ds.subsets {
        subset.has_alpha = true;
    }
    ds
}

/// Builds a heightmap holding one flat cell, covering that cell's world XY square only.
fn flat_terrain(grid: (i32, i32), height: f32) -> crate::usage::TerrainCells<'static> {
    let mut terrain_cells = crate::usage::TerrainCells::default();
    terrain_cells.insert(
        grid,
        crate::usage::TerrainCell {
            grid,
            heights: Box::new([[height; 65]; 65]),
            normals: Default::default(),
            colors: Default::default(),
            texture_indices: Box::new([[0; 16]; 16]),
            texture_table: std::sync::Arc::default(),
        },
    );
    terrain_cells
}

/// Builds two grid statics straddling `z == 0`, well inside terrain cell `(0, 0)`.
fn straddling_world() -> (DistantStatics, UsageInfo<'static>) {
    let mut distant_statics: DistantStatics = Default::default();
    distant_statics.insert("wall_a.nif".to_string(), make_grid_static(64.0, 256.0, 1.0));
    distant_statics.insert("wall_b.nif".to_string(), make_grid_static(64.0, 256.0, 1.0));

    let mut usage: UsageInfo<'static> = UsageInfo::default();
    usage.exterior_references_mut().extend([
        (
            StableRefKey::test(1),
            reference("wall_a.nif", Vec3::new(1000.0, 1000.0, -128.0)),
        ),
        (
            StableRefKey::test(2),
            reference("wall_b.nif", Vec3::new(1100.0, 1000.0, -128.0)),
        ),
    ]);
    (distant_statics, usage)
}

fn reference(id: &'static str, translation: Vec3) -> DistantReference<'static> {
    DistantReference {
        id: Cow::Borrowed(id),
        deleted: false,
        persistent: false,
        translation,
        rotation: Vec3::ZERO,
        scale: 1.0,
        vis_index: 0,
    }
}

#[test]
fn dynamic_references_are_not_batched() {
    let mut distant_statics: DistantStatics = Default::default();
    distant_statics.insert("a.nif".to_string(), make_static());
    distant_statics.insert("b.nif".to_string(), make_static());
    distant_statics.insert("dyn.nif".to_string(), make_static());

    let mut usage: UsageInfo<'static> = UsageInfo::default();
    usage.exterior_references_mut().extend([
        (
            StableRefKey::test(1),
            DistantReference {
                id: Cow::Borrowed("a.nif"),
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
                id: Cow::Borrowed("b.nif"),
                deleted: false,
                persistent: false,
                translation: Vec3::new(32.0, 0.0, 0.0),
                rotation: Vec3::ZERO,
                scale: 1.0,
                vis_index: 0,
            },
        ),
        (
            StableRefKey::test(3),
            DistantReference {
                id: Cow::Borrowed("dyn.nif"),
                deleted: false,
                persistent: false,
                translation: Vec3::new(64.0, 0.0, 0.0),
                rotation: Vec3::ZERO,
                scale: 1.0,
                vis_index: 4,
            },
        ),
    ]);

    merge_exterior_references(
        &mut distant_statics,
        &mut usage,
        StaticMeshSimplifierConfig::default(),
        crate::DEFAULT_MERGE_GROUP_RADIUS,
    );

    let exterior = usage.exterior_references().unwrap();
    assert!(
        exterior
            .values()
            .any(|reference| reference.id.as_ref() == "dyn.nif" && reference.vis_index == 4)
    );
    assert!(
        exterior
            .values()
            .any(|reference| reference.id.as_ref().starts_with("CELL (0, 0) GROUP") && reference.vis_index == 0)
    );
}

#[test]
fn merged_statics_remain_auto_even_when_sources_are_buildings() {
    let mut distant_statics: DistantStatics = Default::default();
    distant_statics.insert(
        "building_a.nif".to_string(),
        make_static_with_type(StaticType::StaticBuilding),
    );
    distant_statics.insert(
        "building_b.nif".to_string(),
        make_static_with_type(StaticType::StaticBuilding),
    );

    let mut usage: UsageInfo<'static> = UsageInfo::default();
    usage.exterior_references_mut().extend([
        (
            StableRefKey::test(1),
            DistantReference {
                id: Cow::Borrowed("building_a.nif"),
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
                id: Cow::Borrowed("building_b.nif"),
                deleted: false,
                persistent: false,
                translation: Vec3::new(32.0, 0.0, 0.0),
                rotation: Vec3::ZERO,
                scale: 1.0,
                vis_index: 0,
            },
        ),
    ]);

    merge_exterior_references(
        &mut distant_statics,
        &mut usage,
        StaticMeshSimplifierConfig::default(),
        crate::DEFAULT_MERGE_GROUP_RADIUS,
    );

    let merged_id = usage
        .exterior_references()
        .and_then(|references| {
            references
                .values()
                .find(|reference| reference.id.as_ref().starts_with("CELL (0, 0) GROUP"))
                .map(|reference| reference.id.to_string())
        })
        .expect("merged reference");
    let merged_static = distant_statics.get(merged_id.as_str()).expect("merged static");
    assert!(matches!(merged_static.static_type, StaticType::StaticAuto));
    assert!(merged_static.horizon_footprint_eligible);
}

#[test]
fn merged_static_bounds_use_transformed_component_sphere_centers() {
    let mut source = make_grid_static(8.0, 0.0, 1.0);
    source.bounding_sphere = NiBound {
        center: Vec3::new(5.0, 2.0, 0.0),
        radius: 64.0,
    };

    let mut distant_statics: DistantStatics = Default::default();
    distant_statics.insert("offset.nif".to_string(), source);

    let mut usage: UsageInfo<'static> = UsageInfo::default();
    usage.exterior_references_mut().extend([
        (StableRefKey::test(1), reference("offset.nif", Vec3::ZERO)),
        (StableRefKey::test(2), reference("offset.nif", Vec3::new(64.0, 0.0, 0.0))),
    ]);

    let metrics = merge_exterior_references(
        &mut distant_statics,
        &mut usage,
        StaticMeshSimplifierConfig::default(),
        crate::DEFAULT_MERGE_GROUP_RADIUS,
    );

    assert_eq!(metrics.group_count, 1);
    let merged_id = usage
        .exterior_references()
        .and_then(|references| {
            references
                .values()
                .find(|reference| reference.id.as_ref().starts_with("CELL (0, 0) GROUP"))
                .map(|reference| reference.id.to_string())
        })
        .expect("merged reference");
    let merged_static = distant_statics.get(merged_id.as_str()).expect("merged static");

    assert!((merged_static.bounding_sphere.center - Vec3::new(5.0, 2.0, 0.0)).length() < 1e-5);
    assert!((merged_static.bounding_sphere.radius - 96.0).abs() < 1e-5);
}

#[test]
fn merged_groups_are_applied_in_stable_cell_group_order() {
    let mut distant_statics: DistantStatics = Default::default();
    distant_statics.insert("west_a.nif".to_string(), make_static());
    distant_statics.insert("west_b.nif".to_string(), make_static());
    distant_statics.insert("east_a.nif".to_string(), make_static());
    distant_statics.insert("east_b.nif".to_string(), make_static());

    let mut usage: UsageInfo<'static> = UsageInfo::default();
    usage.exterior_references_mut().extend([
        (
            StableRefKey::test(1),
            DistantReference {
                id: Cow::Borrowed("east_a.nif"),
                deleted: false,
                persistent: false,
                translation: Vec3::new(8192.0, 0.0, 0.0),
                rotation: Vec3::ZERO,
                scale: 1.0,
                vis_index: 0,
            },
        ),
        (
            StableRefKey::test(2),
            DistantReference {
                id: Cow::Borrowed("east_b.nif"),
                deleted: false,
                persistent: false,
                translation: Vec3::new(8224.0, 0.0, 0.0),
                rotation: Vec3::ZERO,
                scale: 1.0,
                vis_index: 0,
            },
        ),
        (
            StableRefKey::test(3),
            DistantReference {
                id: Cow::Borrowed("west_a.nif"),
                deleted: false,
                persistent: false,
                translation: Vec3::ZERO,
                rotation: Vec3::ZERO,
                scale: 1.0,
                vis_index: 0,
            },
        ),
        (
            StableRefKey::test(4),
            DistantReference {
                id: Cow::Borrowed("west_b.nif"),
                deleted: false,
                persistent: false,
                translation: Vec3::new(32.0, 0.0, 0.0),
                rotation: Vec3::ZERO,
                scale: 1.0,
                vis_index: 0,
            },
        ),
    ]);

    merge_exterior_references(
        &mut distant_statics,
        &mut usage,
        StaticMeshSimplifierConfig::default(),
        crate::DEFAULT_MERGE_GROUP_RADIUS,
    );

    let exterior = usage.exterior_references().expect("merged exterior references");
    assert_eq!(
        exterior.get(&StableRefKey::synthetic(1)).unwrap().id.as_ref(),
        "CELL (0, 0) GROUP (0)"
    );
    assert_eq!(
        exterior.get(&StableRefKey::synthetic(2)).unwrap().id.as_ref(),
        "CELL (1, 0) GROUP (0)"
    );
}

#[test]
fn error_bucket_preserves_zero_and_rounds_positive_errors_down() {
    assert_eq!(error_bucket(0.0), ZERO_ERROR_BUCKET);
    assert_eq!(bucket_error(ZERO_ERROR_BUCKET), 0.0);

    for error in [1e-12, 1e-6, 1e-3, 0.25, 1.0, 409.6] {
        let bucketed = bucket_error(error_bucket(error));
        assert!(bucketed > 0.0);
        assert!(bucketed <= error);
    }
}

#[test]
fn merge_value_distribution_reports_expected_percentiles() {
    let distribution = MergeValueDistribution::from_values((1..=100).map(|value| value as f32).collect());

    assert_eq!(distribution.min, 1.0);
    assert_eq!(distribution.p50, 50.0);
    assert_eq!(distribution.p95, 95.0);
    assert_eq!(distribution.max, 100.0);
}

#[test]
fn heterogeneous_group_targets_are_capped_per_subset() {
    let mut distant_statics: DistantStatics = Default::default();
    distant_statics.insert("large.nif".to_string(), make_grid_static(4096.0, 0.0, 1.0));
    distant_statics.insert("small.nif".to_string(), make_grid_static(128.0, 0.0, 1.0));

    let mut usage: UsageInfo<'static> = UsageInfo::default();
    usage.exterior_references_mut().extend([
        (StableRefKey::test(1), reference("large.nif", Vec3::ZERO)),
        (StableRefKey::test(2), reference("small.nif", Vec3::new(64.0, 0.0, 0.0))),
    ]);

    let config = StaticMeshSimplifierConfig {
        target_error: 0.05,
        merge_error_multiplier: 1.0,
        ..StaticMeshSimplifierConfig::default()
    };
    let metrics = merge_exterior_references(&mut distant_statics, &mut usage, config, crate::DEFAULT_MERGE_GROUP_RADIUS);

    assert_eq!(metrics.group_count, 1);
    assert_eq!(metrics.member_subset_count, 2);
    assert!(metrics.group_to_member_extent_ratio.max >= 32.0);
    assert!(metrics.requested_relative_target.max > 1.0);
    assert_eq!(metrics.effective_relative_target.max, config.target_error);
    assert_eq!(metrics.second_pass_subset_count, 0);
    assert_eq!(
        metrics.member_triangle_count_before_second_pass,
        metrics.member_triangle_count_after_second_pass
    );
}

#[test]
fn compatible_sources_with_different_uv_scales_use_the_same_relative_cap() {
    let mut distant_statics: DistantStatics = Default::default();
    distant_statics.insert("uv_small.nif".to_string(), make_grid_static(128.0, 0.0, 0.25));
    distant_statics.insert("uv_large.nif".to_string(), make_grid_static(128.0, 0.0, 16.0));

    let mut usage: UsageInfo<'static> = UsageInfo::default();
    usage.exterior_references_mut().extend([
        (StableRefKey::test(1), reference("uv_small.nif", Vec3::ZERO)),
        (StableRefKey::test(2), reference("uv_large.nif", Vec3::new(4096.0, 0.0, 0.0))),
    ]);

    let config = StaticMeshSimplifierConfig {
        target_error: 0.05,
        merge_error_multiplier: 2.0,
        ..StaticMeshSimplifierConfig::default()
    };
    let metrics = merge_exterior_references(&mut distant_statics, &mut usage, config, crate::DEFAULT_MERGE_GROUP_RADIUS);

    assert_eq!(metrics.member_subset_count, 2);
    assert_eq!(metrics.capped_subset_count, 2);
    assert_eq!(metrics.second_pass_subset_count, 2);
    assert!(metrics.effective_relative_target.max <= config.target_error * config.merge_error_multiplier);
}

#[test]
fn alpha_members_do_not_request_merge_lods() {
    let mut distant_statics: DistantStatics = Default::default();
    distant_statics.insert("alpha_a.nif".to_string(), make_alpha_grid_static(128.0, 0.0, 1.0));
    distant_statics.insert("alpha_b.nif".to_string(), make_alpha_grid_static(128.0, 0.0, 1.0));

    let mut usage: UsageInfo<'static> = UsageInfo::default();
    usage.exterior_references_mut().extend([
        (StableRefKey::test(1), reference("alpha_a.nif", Vec3::ZERO)),
        (StableRefKey::test(2), reference("alpha_b.nif", Vec3::new(4096.0, 0.0, 0.0))),
    ]);

    let config = StaticMeshSimplifierConfig {
        target_error: 0.05,
        merge_error_multiplier: 100.0,
        ..StaticMeshSimplifierConfig::default()
    };
    let metrics = merge_exterior_references(&mut distant_statics, &mut usage, config, crate::DEFAULT_MERGE_GROUP_RADIUS);

    assert_eq!(metrics.member_subset_count, 2);
    assert_eq!(metrics.lod_cache_request_count, 0);
    assert_eq!(metrics.lod_cache_entry_count, 0);
    assert_eq!(metrics.second_pass_subset_count, 0);
    assert_eq!(metrics.requested_relative_target, MergeValueDistribution::default());
    assert_eq!(metrics.effective_relative_target, MergeValueDistribution::default());
    assert_eq!(
        metrics.member_triangle_count_before_second_pass,
        metrics.member_triangle_count_after_second_pass
    );
}

#[test]
fn same_mesh_members_reuse_lod_cache_entries() {
    let mut distant_statics: DistantStatics = Default::default();
    distant_statics.insert("shared.nif".to_string(), make_grid_static(128.0, 0.0, 1.0));

    let mut usage: UsageInfo<'static> = UsageInfo::default();
    usage.exterior_references_mut().extend([
        (StableRefKey::test(1), reference("shared.nif", Vec3::ZERO)),
        (StableRefKey::test(2), reference("shared.nif", Vec3::new(4096.0, 0.0, 0.0))),
    ]);

    let config = StaticMeshSimplifierConfig {
        target_error: 0.05,
        merge_error_multiplier: 2.0,
        ..StaticMeshSimplifierConfig::default()
    };
    let metrics = merge_exterior_references(&mut distant_statics, &mut usage, config, crate::DEFAULT_MERGE_GROUP_RADIUS);

    assert_eq!(metrics.group_count, 1);
    assert_eq!(metrics.member_count, 2);
    assert_eq!(metrics.lod_cache_request_count, 2);
    assert_eq!(metrics.lod_cache_entry_count, 1);
    assert_eq!(metrics.lod_cache_reuse_count, 1);
    assert!(metrics.second_pass_subset_count >= 1);
    assert_eq!(metrics.emitted_merged_static_count, 1);
    assert!(metrics.merged_triangle_count > 0);
    assert!(metrics.member_triangle_count_after_second_pass < metrics.member_triangle_count_before_second_pass);

    let merged_id = usage
        .exterior_references()
        .and_then(|references| {
            references
                .values()
                .find(|reference| reference.id.as_ref().starts_with("CELL (0, 0) GROUP"))
                .map(|reference| reference.id.to_string())
        })
        .expect("merged reference");
    assert!(distant_statics.contains_key(merged_id.as_str()));
}

#[test]
fn tall_group_members_do_not_exceed_the_relative_cap() {
    let mut distant_statics: DistantStatics = Default::default();
    distant_statics.insert("tower.nif".to_string(), make_grid_static(128.0, 8192.0, 1.0));
    distant_statics.insert("small.nif".to_string(), make_grid_static(128.0, 0.0, 1.0));

    let mut usage: UsageInfo<'static> = UsageInfo::default();
    usage.exterior_references_mut().extend([
        (StableRefKey::test(1), reference("tower.nif", Vec3::ZERO)),
        (StableRefKey::test(2), reference("small.nif", Vec3::new(128.0, 0.0, 0.0))),
    ]);

    let config = StaticMeshSimplifierConfig {
        target_error: 0.05,
        merge_error_multiplier: 1.5,
        ..StaticMeshSimplifierConfig::default()
    };
    let metrics = merge_exterior_references(&mut distant_statics, &mut usage, config, crate::DEFAULT_MERGE_GROUP_RADIUS);

    assert!(metrics.group_extent.max >= 8192.0);
    assert!(metrics.requested_relative_target.max > 1.0);
    assert!(metrics.effective_relative_target.max <= config.target_error * config.merge_error_multiplier);
}

#[test]
fn radius_sweep_changes_group_count_without_changing_the_relative_cap() {
    let mut source_statics: DistantStatics = Default::default();
    source_statics.insert("grid.nif".to_string(), make_grid_static(64.0, 0.0, 1.0));
    let mut source_usage: UsageInfo<'static> = UsageInfo::default();
    source_usage.exterior_references_mut().extend([
        (StableRefKey::test(1), reference("grid.nif", Vec3::new(0.0, 0.0, 0.0))),
        (StableRefKey::test(2), reference("grid.nif", Vec3::new(128.0, 0.0, 0.0))),
        (StableRefKey::test(3), reference("grid.nif", Vec3::new(4096.0, 0.0, 0.0))),
        (StableRefKey::test(4), reference("grid.nif", Vec3::new(4224.0, 0.0, 0.0))),
    ]);
    let config = StaticMeshSimplifierConfig {
        target_error: 0.05,
        merge_error_multiplier: 1.0,
        ..StaticMeshSimplifierConfig::default()
    };

    let mut local_statics = source_statics.clone();
    let mut local_usage = source_usage.clone();
    let local = merge_exterior_references(&mut local_statics, &mut local_usage, config, 256.0);

    let mut cell_statics = source_statics;
    let mut cell_usage = source_usage;
    let cell = merge_exterior_references(&mut cell_statics, &mut cell_usage, config, crate::DEFAULT_MERGE_GROUP_RADIUS);

    assert!(local.group_count > cell.group_count);
    assert_eq!(local.effective_relative_target.max, config.target_error);
    assert_eq!(cell.effective_relative_target.max, config.target_error);
}

#[test]
fn build_merge_geometry_honors_cell_filter_while_apply_stays_global() {
    fn world() -> (DistantStatics, UsageInfo<'static>) {
        let mut distant_statics: DistantStatics = Default::default();
        distant_statics.insert("west_a.nif".to_string(), make_merge_source());
        distant_statics.insert("west_b.nif".to_string(), make_merge_source());
        distant_statics.insert("east_a.nif".to_string(), make_merge_source());
        distant_statics.insert("east_b.nif".to_string(), make_merge_source());

        let mut usage: UsageInfo<'static> = UsageInfo::default();
        usage.exterior_references_mut().extend([
            (StableRefKey::test(1), reference("west_a.nif", Vec3::ZERO)),
            (StableRefKey::test(2), reference("west_b.nif", Vec3::new(32.0, 0.0, 0.0))),
            (StableRefKey::test(3), reference("east_a.nif", Vec3::new(8192.0, 0.0, 0.0))),
            (StableRefKey::test(4), reference("east_b.nif", Vec3::new(8224.0, 0.0, 0.0))),
        ]);
        (distant_statics, usage)
    }

    let config = StaticMeshSimplifierConfig::default();
    let west_id = "CELL (0, 0) GROUP (0)";
    let east_id = "CELL (1, 0) GROUP (0)";

    // Full path builds both cells' merged statics.
    let (mut full_statics, mut full_usage) = world();
    merge_exterior_references(&mut full_statics, &mut full_usage, config, crate::DEFAULT_MERGE_GROUP_RADIUS);
    assert!(full_statics.contains_key(west_id));
    assert!(full_statics.contains_key(east_id));

    // Partial path plans and applies globally, but builds geometry only for the west cell.
    let (mut partial_statics, mut partial_usage) = world();
    let plan = plan_exterior_merge_groups(&partial_statics, &partial_usage, crate::DEFAULT_MERGE_GROUP_RADIUS);
    apply_merge_usage(&plan, &mut partial_usage);
    let dirty: HashSet<(i32, i32)> = HashSet::from_iter([(0, 0)]);
    build_merge_geometry_unpacked(&plan, &CellFilter::Dirty(dirty), &mut partial_statics, config, None);

    // Only the west cell's synthetic static was built, and it matches the full build.
    assert!(partial_statics.contains_key(west_id));
    assert!(!partial_statics.contains_key(east_id));
    let summarize = |ds: &DistantStatic| {
        let triangles: usize = ds.subsets.iter().map(|subset| subset.triangles.len()).sum();
        let vertices: usize = ds.subsets.iter().map(|subset| subset.vertices.len()).sum();
        (ds.subsets.len(), triangles, vertices)
    };
    assert_eq!(
        summarize(partial_statics.get(west_id).unwrap()),
        summarize(full_statics.get(west_id).unwrap())
    );

    // Usage mutation is global: both synthetic references are present regardless of the filter.
    let usage_ids = |usage: &UsageInfo<'_>| {
        usage
            .exterior_references()
            .unwrap()
            .values()
            .map(|reference| reference.id.to_string())
            .collect::<std::collections::BTreeSet<_>>()
    };
    assert_eq!(usage_ids(&partial_usage), usage_ids(&full_usage));
}

#[test]
fn same_cell_emits_multiple_groups_in_stable_group_idx_order() {
    let mut distant_statics: DistantStatics = Default::default();
    distant_statics.insert("a.nif".to_string(), make_merge_source());
    distant_statics.insert("b.nif".to_string(), make_merge_source());
    distant_statics.insert("c.nif".to_string(), make_merge_source());
    distant_statics.insert("d.nif".to_string(), make_merge_source());

    // Two tight pairs far apart in the same exterior cell so a small radius yields two groups.
    let mut usage: UsageInfo<'static> = UsageInfo::default();
    usage.exterior_references_mut().extend([
        (StableRefKey::test(4), reference("d.nif", Vec3::new(4128.0, 0.0, 0.0))),
        (StableRefKey::test(1), reference("a.nif", Vec3::ZERO)),
        (StableRefKey::test(3), reference("c.nif", Vec3::new(4096.0, 0.0, 0.0))),
        (StableRefKey::test(2), reference("b.nif", Vec3::new(32.0, 0.0, 0.0))),
    ]);

    merge_exterior_references(&mut distant_statics, &mut usage, StaticMeshSimplifierConfig::default(), 256.0);

    let exterior = usage.exterior_references().expect("merged exterior references");
    assert_eq!(exterior.len(), 2);
    assert_eq!(
        exterior.get(&StableRefKey::synthetic(1)).unwrap().id.as_ref(),
        "CELL (0, 0) GROUP (0)"
    );
    assert_eq!(
        exterior.get(&StableRefKey::synthetic(2)).unwrap().id.as_ref(),
        "CELL (0, 0) GROUP (1)"
    );
    assert!(distant_statics.contains_key("CELL (0, 0) GROUP (0)"));
    assert!(distant_statics.contains_key("CELL (0, 0) GROUP (1)"));
}

#[test]
fn non_grouped_eligible_references_remain_in_usage() {
    let mut distant_statics: DistantStatics = Default::default();
    distant_statics.insert("pair_a.nif".to_string(), make_static());
    distant_statics.insert("pair_b.nif".to_string(), make_static());
    distant_statics.insert("alone.nif".to_string(), make_static());

    let alone_key = StableRefKey::test(3);
    let mut usage: UsageInfo<'static> = UsageInfo::default();
    usage.exterior_references_mut().extend([
        // Mergeable pair in cell (0, 0).
        (StableRefKey::test(1), reference("pair_a.nif", Vec3::ZERO)),
        (StableRefKey::test(2), reference("pair_b.nif", Vec3::new(32.0, 0.0, 0.0))),
        // Sole eligible reference in cell (1, 0): never forms a multi-member group.
        (alone_key, reference("alone.nif", Vec3::new(8192.0, 0.0, 0.0))),
    ]);

    merge_exterior_references(
        &mut distant_statics,
        &mut usage,
        StaticMeshSimplifierConfig::default(),
        crate::DEFAULT_MERGE_GROUP_RADIUS,
    );

    let exterior = usage.exterior_references().expect("merged exterior references");
    assert!(exterior.contains_key(&alone_key));
    assert_eq!(exterior.get(&alone_key).unwrap().id.as_ref(), "alone.nif");
    assert!(!exterior.contains_key(&StableRefKey::test(1)));
    assert!(!exterior.contains_key(&StableRefKey::test(2)));
    assert!(exterior.contains_key(&StableRefKey::synthetic(1)));
    assert_eq!(
        exterior.get(&StableRefKey::synthetic(1)).unwrap().id.as_ref(),
        "CELL (0, 0) GROUP (0)"
    );
}

/// Merges [`straddling_world`] under `cull` and returns the synthetic static, the run metrics, and
/// the synthetic reference's translation (which merged vertices are relative to).
fn build_straddling_merge(cull: Option<SubterrainCull<'_>>) -> (DistantStatic, MergeSimplificationMetrics, Vec3) {
    const MERGED_ID: &str = "CELL (0, 0) GROUP (0)";

    let (mut distant_statics, mut usage) = straddling_world();
    let plan = plan_exterior_merge_groups(&distant_statics, &usage, crate::DEFAULT_MERGE_GROUP_RADIUS);
    apply_merge_usage(&plan, &mut usage);
    let metrics = build_merge_geometry_unpacked(
        &plan,
        &CellFilter::All,
        &mut distant_statics,
        StaticMeshSimplifierConfig::default(),
        cull,
    );
    let center = usage
        .exterior_references()
        .unwrap()
        .values()
        .find(|reference| reference.id.as_ref() == MERGED_ID)
        .expect("merge applied a synthetic reference")
        .translation;
    (
        distant_statics.shift_remove(MERGED_ID).expect("merged static was built"),
        metrics,
        center,
    )
}

fn total_triangles(ds: &DistantStatic) -> usize {
    ds.subsets.iter().map(|subset| subset.triangles.len()).sum()
}

fn total_vertices(ds: &DistantStatic) -> usize {
    ds.subsets.iter().map(|subset| subset.vertices.len()).sum()
}

#[test]
fn subterrain_cull_removes_only_fully_buried_triangles() {
    let terrain_cells = flat_terrain((0, 0), 0.0);
    let (uncut, _, _) = build_straddling_merge(None);
    let (culled, metrics, center) = build_straddling_merge(Some(SubterrainCull::new(&terrain_cells, 0.0)));

    assert!(
        total_triangles(&culled) < total_triangles(&uncut),
        "half the geometry is below the ground plane, so the cull must remove some of it"
    );
    assert_eq!(
        metrics.subterrain_culled_triangle_count,
        total_triangles(&uncut) - total_triangles(&culled),
        "reported triangle tally must match the geometry actually removed"
    );
    assert_eq!(
        metrics.subterrain_culled_vertex_count,
        total_vertices(&uncut) - total_vertices(&culled),
        "reported vertex tally must match the geometry actually removed"
    );

    for subset in &culled.subsets {
        assert!(
            subset.components_tile_triangles(),
            "component ranges must still tile the triangle buffer after culling"
        );
        for triangle in &subset.triangles {
            assert!(
                triangle
                    .iter()
                    .any(|&index| subset.vertices[index as usize].position.z + center.z >= 0.0),
                "a triangle with every corner below the threshold survived"
            );
        }
    }
}

#[test]
fn subterrain_cull_keeps_geometry_no_land_cell_covers() {
    // The heightmap covers cell (1, 0); the statics sit in cell (0, 0) and are therefore unsampled.
    let terrain_cells = flat_terrain((1, 0), 0.0);
    let (uncut, _, _) = build_straddling_merge(None);
    let (culled, metrics, _) = build_straddling_merge(Some(SubterrainCull::new(&terrain_cells, 1e9)));

    assert_eq!(metrics.subterrain_culled_triangle_count, 0);
    assert_eq!(metrics.subterrain_culled_vertex_count, 0);
    assert_eq!(total_triangles(&culled), total_triangles(&uncut));
    assert_eq!(total_vertices(&culled), total_vertices(&uncut));
}

#[test]
fn subterrain_cull_margin_protects_geometry_near_the_ground() {
    let terrain_cells = flat_terrain((0, 0), 0.0);
    // The statics span z in [-128, 128], so a margin past their full depth can bury nothing.
    let (uncut, _, _) = build_straddling_merge(None);
    let (culled, metrics, _) = build_straddling_merge(Some(SubterrainCull::new(&terrain_cells, 256.0)));

    assert_eq!(metrics.subterrain_culled_triangle_count, 0);
    assert_eq!(total_triangles(&culled), total_triangles(&uncut));
}

/// A merge contribution can itself carry several bounds, because post-atlas
/// `DistantStatic::merge_subsets` already combined subsets that shared an atlas page. The
/// eligibility test and the append must therefore both work on the full set union; a
/// single-bound insert would let this destination exceed the palette cap.
#[test]
fn merge_subsets_tests_and_appends_the_full_bound_union() {
    let bound = |seed: u32| UvBound {
        min_y: 0.0,
        max_x: 1.0,
        min_x: seed as f32 / 1024.0,
        max_y: 1.0,
    };
    let subset_with_bounds = |bounds: Vec<UvBound>| Subset {
        vertices: vec![Vertex::default(); 3],
        triangles: vec![[0, 1, 2]],
        texture: SubsetTexture::AtlasPage(0),
        uv_bounds: bounds,
        ..Subset::default()
    };

    let source = subset_with_bounds((0..4).map(bound).collect());
    // Three short of the cap, sharing none of the incoming bounds: a single-bound insert would
    // fit (126 <= 128), the true four-bound union does not (129 > 128).
    let destination = subset_with_bounds((100..100 + UV_BOUND_PALETTE_CAP - 3).map(bound).collect());

    let mut merged = vec![destination];
    merge_subsets(
        &mut merged,
        std::slice::from_ref(&source),
        Affine3A::IDENTITY,
        Vec3::ZERO,
        true,
        Vec3::ZERO,
        1.0,
        StaticType::StaticTree,
        None,
    );

    assert_eq!(merged.len(), 2, "the full union must force a new subset");
    assert_eq!(merged[1].uv_bounds.len(), 4, "the append must union every incoming bound");
    assert_eq!(merged[1].triangles.len(), 1);
}
