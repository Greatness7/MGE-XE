//! Tests for material planning, blend patterns, patch albedo, and texture dedupe
//! (`package::material`).

use std::sync::Arc;

use super::*;

fn make_cell_with_table<'a>(
    grid: (i32, i32),
    texture_table: &[(u16, &'a str)],
    raw_index_at: impl Fn(usize, usize) -> u16,
) -> crate::texture::TerrainCell<'a> {
    let mut resolved: IndexMap<u16, &'a str> = Default::default();
    for &(index, path) in texture_table {
        resolved.insert(index, path);
    }

    let mut texture_indices = Box::new([[0u16; 16]; 16]);
    for patch_y in 0..16 {
        for patch_x in 0..16 {
            let parent_index = (patch_x / 4) + 4 * (patch_y / 4);
            let shape_index = (patch_x % 4) + 4 * (patch_y % 4);
            texture_indices[parent_index][shape_index] = raw_index_at(patch_x, patch_y);
        }
    }

    crate::texture::TerrainCell {
        grid,
        heights: Box::new([[0.0; 65]; 65]),
        normals: vec![Vec3::Z; 65 * 65],
        colors: vec![Vec4::new(1.0, 1.0, 1.0, 0.0); 65 * 65],
        texture_indices,
        texture_table: Arc::new(resolved),
    }
}

fn make_test_image(rows: [[[u8; 4]; 2]; 2]) -> RgbaImage {
    let mut image = RgbaImage::new(2, 2);
    for (y, row) in rows.into_iter().enumerate() {
        for (x, pixel) in row.into_iter().enumerate() {
            image.put_pixel(x as u32, y as u32, Rgba(pixel));
        }
    }
    image
}

fn plan_materials_for_tests<'a>(
    terrain_cells: &crate::texture::TerrainCells<'a>,
    default_key: &'a str,
    colors: &[(&'a str, [u8; 4])],
) -> (TerrainTextureIds<'a>, TerrainMaterialPlan, TerrainControlRegion) {
    let cache = make_texture_cache(default_key, colors);
    let texture_ids = collect_terrain_texture_ids(&cache, TextureDedupeMode::Off).unwrap();
    let region = terrain_control_region(terrain_cells);
    let material_plan = build_terrain_material_plan(terrain_cells, &texture_ids, region).unwrap();
    (texture_ids, material_plan, region)
}

fn patch_material_at(
    material_plan: &TerrainMaterialPlan,
    region: TerrainControlRegion,
    cell: (i32, i32),
    patch: (usize, usize),
) -> PatchMaterial {
    let x = u32::try_from(cell.0 - region.origin_cell[0]).unwrap() * 16 + patch.0 as u32;
    let y = u32::try_from(cell.1 - region.origin_cell[1]).unwrap() * 16 + patch.1 as u32;
    material_plan.patch_materials[y as usize * region.material_size_xy[0] as usize + x as usize]
}

fn find_patch_plan<'a>(
    cell_plan: &'a crate::landscape_plan::LandscapeCellPlan<'a>,
    patch: (usize, usize),
) -> &'a crate::landscape_plan::LandscapePatchPlan<'a> {
    cell_plan
        .patches
        .iter()
        .find(|candidate| candidate.patch_x == patch.0 && candidate.patch_y == patch.1)
        .expect("patch not found in cell plan")
}

fn expected_material_path<'a>(texture: Option<&'a str>, default_key: &'a str) -> &'a str {
    match texture {
        Some(texture) if !is_default_texture_name(texture) => texture,
        _ => default_key,
    }
}

fn assert_patch_matches_planner<'a>(
    terrain_cells: &crate::texture::TerrainCells<'a>,
    texture_ids: &TerrainTextureIds<'a>,
    material_plan: &TerrainMaterialPlan,
    region: TerrainControlRegion,
    grid: (i32, i32),
    patch: (usize, usize),
    default_key: &'a str,
) {
    let cell_plan = plan_landscape_cell_with_sampler(grid, |cell, patch_x, patch_y| {
        sample_patch_texture_identity(terrain_cells, texture_ids, default_key, cell, patch_x, patch_y)
    });
    let expected = find_patch_plan(&cell_plan, patch);
    let material = patch_material_at(material_plan, region, grid, patch);
    let has_decal = expected.texturing.decal_texture.is_some();
    let base_path = texture_ids.ordered_paths[material.base_id as usize];
    let decal_path = texture_ids.ordered_paths[material.decal_id as usize];
    assert_eq!(
        base_path,
        expected_material_path(expected.texturing.base_texture, default_key)
    );
    assert_eq!(
        decal_path,
        if has_decal {
            expected_material_path(expected.texturing.decal_texture, default_key)
        } else {
            base_path
        }
    );
    assert_eq!((material.flags & FLAG_HAS_DECAL) != 0, has_decal);
    assert_eq!(
        (material.flags & FLAG_USES_DEFAULT_TEXTURE) != 0,
        base_path == default_key || (has_decal && decal_path == default_key)
    );
    assert_eq!(
        material_plan.patterns[material.pattern_id as usize],
        expected.texturing.alpha_grid
    );
}

fn build_patch_albedo_for_tests<'a>(
    cache: &TerrainTextureCache<'a>,
    texture_ids: &TerrainTextureIds<'a>,
    material_plan: &TerrainMaterialPlan,
    region: TerrainControlRegion,
) -> RgbaImage {
    build_patch_albedo_image(
        cache,
        texture_ids,
        region.material_size_xy,
        &material_plan.patch_materials,
        &rasterize_blend_patterns(&material_plan.patterns, material_plan.pattern_atlas.logical_tile_size),
    )
}

fn expected_patch_albedo_from_planner<'a>(
    terrain_cells: &crate::texture::TerrainCells<'a>,
    cache: &TerrainTextureCache<'a>,
    texture_ids: &TerrainTextureIds<'a>,
    material_plan: &TerrainMaterialPlan,
    grid: (i32, i32),
    patch: (usize, usize),
    default_key: &'a str,
) -> Rgba<u8> {
    let cell_plan = plan_landscape_cell_with_sampler(grid, |cell, patch_x, patch_y| {
        sample_patch_texture_identity(terrain_cells, texture_ids, default_key, cell, patch_x, patch_y)
    });
    let expected = find_patch_plan(&cell_plan, patch);
    let unassigned = build_unassigned_patch_material(&expected.texturing, texture_ids);
    let pattern_id = u8::try_from(
        material_plan
            .patterns
            .binary_search(&unassigned.alpha_grid)
            .expect("planner alpha grid must exist in pattern table"),
    )
    .expect("pattern ID fits in u8");
    let material = PatchMaterial {
        base_id: unassigned.base_id,
        decal_id: unassigned.decal_id,
        pattern_id,
        flags: unassigned.flags,
    };
    let patch_grid = patch_sample_grid(material_plan.pattern_atlas.logical_tile_size);
    let pattern_images = rasterize_blend_patterns(&material_plan.patterns, material_plan.pattern_atlas.logical_tile_size);
    average_patch_material_albedo(cache, texture_ids, material, &pattern_images, &patch_grid)
}

fn assert_patch_albedo_matches_planner<'a>(
    terrain_cells: &crate::texture::TerrainCells<'a>,
    cache: &TerrainTextureCache<'a>,
    texture_ids: &TerrainTextureIds<'a>,
    material_plan: &TerrainMaterialPlan,
    region: TerrainControlRegion,
    grid: (i32, i32),
    patch: (usize, usize),
    default_key: &'a str,
) {
    assert_patch_matches_planner(terrain_cells, texture_ids, material_plan, region, grid, patch, default_key);
    let patch_albedo = build_patch_albedo_for_tests(cache, texture_ids, material_plan, region);
    let texel_x = u32::try_from(grid.0 - region.origin_cell[0]).unwrap() * 16 + patch.0 as u32;
    let texel_y = u32::try_from(grid.1 - region.origin_cell[1]).unwrap() * 16 + patch.1 as u32;
    assert_eq!(
        *patch_albedo.get_pixel(texel_x, texel_y),
        expected_patch_albedo_from_planner(terrain_cells, cache, texture_ids, material_plan, grid, patch, default_key)
    );
}

#[test]
fn material_texel_packs_base_and_decal_ids_as_rgba_bytes() {
    assert_eq!(pack_material_texel(0x1234, 0xabcd), [0x34, 0x12, 0xcd, 0xab]);
}

#[test]
fn material_flags_texel_packs_pattern_and_flag_bytes() {
    assert_eq!(
        pack_material_flags_texel(7, FLAG_USES_DEFAULT_TEXTURE | FLAG_HAS_DECAL),
        [7, FLAG_USES_DEFAULT_TEXTURE | FLAG_HAS_DECAL, 0, 255]
    );
}

#[test]
fn distinct_blend_patterns_are_sorted_deterministically() {
    let mut pattern_a = EMPTY_BLEND_ALPHA_GRID;
    pattern_a[0][0] = 1;
    let mut pattern_b = EMPTY_BLEND_ALPHA_GRID;
    pattern_b[4][4] = 2;
    let patterns = collect_distinct_blend_patterns([pattern_b, EMPTY_BLEND_ALPHA_GRID, pattern_a, pattern_b]).unwrap();
    assert_eq!(patterns, vec![EMPTY_BLEND_ALPHA_GRID, pattern_b, pattern_a]);
}

#[test]
fn distinct_blend_patterns_reject_more_than_u8_range() {
    let mut patterns = Vec::with_capacity(257);
    for id in 0_u16..257 {
        let mut grid = EMPTY_BLEND_ALPHA_GRID;
        grid[0][0] = (id & 0x00ff) as u8;
        grid[0][1] = (id >> 8) as u8;
        patterns.push(grid);
    }

    let error = collect_distinct_blend_patterns(patterns).unwrap_err().to_string();
    assert!(error.contains("Too many distinct terrain blend patterns"));
}

#[test]
fn blend_pattern_atlas_uses_tight_row_count() {
    let atlas = choose_blend_pattern_atlas(10).expect("ten pattern atlas should fit");
    assert_eq!(atlas.tiles_per_row, 4);
    assert_eq!(atlas.atlas_width, 144);
    assert_eq!(atlas.atlas_height, 108);

    let image = build_blend_patterns_image(
        &rasterize_blend_patterns(&[EMPTY_BLEND_ALPHA_GRID; 10], atlas.logical_tile_size),
        atlas,
    );
    assert_eq!((image.width(), image.height()), (144, 108));
}

#[test]
fn blend_pattern_sampling_uses_native_triangle_split() {
    let mut grid = EMPTY_BLEND_ALPHA_GRID;
    grid[1][0] = 255;
    assert_eq!(sample_alpha_grid(&grid, 0.75, 0.25), 0);
    assert!(sample_alpha_grid(&grid, 0.25, 0.75) > 0);
}

#[test]
fn blend_pattern_writer_clamps_gutters_to_edge_values() {
    let mut grid = EMPTY_BLEND_ALPHA_GRID;
    grid[4][4] = 255;
    let atlas_spec = choose_blend_pattern_atlas(1).expect("single pattern atlas should fit");
    let atlas = build_blend_patterns_image(&rasterize_blend_patterns(&[grid], atlas_spec.logical_tile_size), atlas_spec);
    assert_eq!(
        *atlas.get_pixel(DEFAULT_PATTERN_GUTTER_SIZE, DEFAULT_PATTERN_GUTTER_SIZE),
        *atlas.get_pixel(0, 0)
    );
    assert_eq!(
        *atlas.get_pixel(atlas.width() - 1, atlas.height() - 1),
        *atlas.get_pixel(
            atlas.width() - DEFAULT_PATTERN_GUTTER_SIZE - 1,
            atlas.height() - DEFAULT_PATTERN_GUTTER_SIZE - 1
        )
    );
}

#[test]
fn isolated_patch_material_matches_planner_output() {
    let default_key = "_land_default.dds";
    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    terrain_cells.insert(
        (0, 0),
        make_cell_with_table(
            (0, 0),
            &[(0, "a.dds")],
            |patch_x, patch_y| {
                if patch_x == 5 && patch_y == 5 { 1 } else { 0 }
            },
        ),
    );

    let (texture_ids, material_plan, region) = plan_materials_for_tests(
        &terrain_cells,
        default_key,
        &[(default_key, [0, 0, 0, 255]), ("a.dds", [1, 2, 3, 255])],
    );
    assert_patch_matches_planner(
        &terrain_cells,
        &texture_ids,
        &material_plan,
        region,
        (0, 0),
        (5, 5),
        default_key,
    );
}

#[test]
fn north_over_west_priority_material_matches_planner_output() {
    let default_key = "_land_default.dds";
    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    terrain_cells.insert(
        (0, 0),
        make_cell_with_table(
            (0, 0),
            &[(0, "current.dds"), (1, "north.dds"), (2, "west.dds"), (3, "nw.dds")],
            |patch_x, patch_y| match (patch_x, patch_y) {
                (5, 5) => 1,
                (5, 6) => 2,
                (4, 5) => 3,
                (4, 6) => 4,
                _ => 1,
            },
        ),
    );

    let (texture_ids, material_plan, region) = plan_materials_for_tests(
        &terrain_cells,
        default_key,
        &[
            (default_key, [0, 0, 0, 255]),
            ("current.dds", [1, 2, 3, 255]),
            ("north.dds", [4, 5, 6, 255]),
            ("west.dds", [7, 8, 9, 255]),
            ("nw.dds", [10, 11, 12, 255]),
        ],
    );
    assert_patch_matches_planner(
        &terrain_cells,
        &texture_ids,
        &material_plan,
        region,
        (0, 0),
        (5, 5),
        default_key,
    );
    let material = patch_material_at(&material_plan, region, (0, 0), (5, 5));
    assert_eq!(texture_ids.ordered_paths[material.decal_id as usize], "north.dds");
}

#[test]
fn northwest_only_diagonal_material_matches_planner_output() {
    let default_key = "_land_default.dds";
    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    terrain_cells.insert(
        (0, 0),
        make_cell_with_table((0, 0), &[(0, "base.dds"), (1, "diag.dds")], |patch_x, patch_y| {
            if patch_x == 4 && patch_y == 6 { 2 } else { 1 }
        }),
    );

    let (texture_ids, material_plan, region) = plan_materials_for_tests(
        &terrain_cells,
        default_key,
        &[
            (default_key, [0, 0, 0, 255]),
            ("base.dds", [1, 2, 3, 255]),
            ("diag.dds", [4, 5, 6, 255]),
        ],
    );
    assert_patch_matches_planner(
        &terrain_cells,
        &texture_ids,
        &material_plan,
        region,
        (0, 0),
        (5, 5),
        default_key,
    );
}

#[test]
fn cross_cell_wrapping_material_matches_planner_output() {
    let default_key = "_land_default.dds";
    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    terrain_cells.insert(
        (0, 0),
        make_cell_with_table((0, 0), &[(0, "east.dds"), (1, "west.dds")], |_x, _y| 1),
    );
    terrain_cells.insert(
        (-1, 0),
        make_cell_with_table((-1, 0), &[(0, "east.dds"), (1, "west.dds")], |_x, _y| 2),
    );

    let (texture_ids, material_plan, region) = plan_materials_for_tests(
        &terrain_cells,
        default_key,
        &[
            (default_key, [0, 0, 0, 255]),
            ("east.dds", [1, 2, 3, 255]),
            ("west.dds", [4, 5, 6, 255]),
        ],
    );
    assert_patch_matches_planner(
        &terrain_cells,
        &texture_ids,
        &material_plan,
        region,
        (0, 0),
        (0, 5),
        default_key,
    );
}

#[test]
fn top_left_quirk_material_matches_planner_output() {
    let default_key = "_land_default.dds";
    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    terrain_cells.insert(
        (0, 0),
        make_cell_with_table((0, 0), &[(0, "current.dds")], |patch_x, patch_y| {
            if patch_x == 0 && patch_y == 15 { 1 } else { 0 }
        }),
    );
    terrain_cells.insert(
        (-1, 0),
        make_cell_with_table((-1, 0), &[(0, "west.dds")], |patch_x, patch_y| {
            if patch_x == 15 && patch_y == 15 { 1 } else { 0 }
        }),
    );

    let (texture_ids, material_plan, region) = plan_materials_for_tests(
        &terrain_cells,
        default_key,
        &[
            (default_key, [0, 0, 0, 255]),
            ("current.dds", [1, 2, 3, 255]),
            ("west.dds", [4, 5, 6, 255]),
        ],
    );
    assert_patch_matches_planner(
        &terrain_cells,
        &texture_ids,
        &material_plan,
        region,
        (0, 0),
        (0, 15),
        default_key,
    );
}

#[test]
fn same_raw_index_with_different_cell_tables_produces_distinct_material_ids() {
    let default_key = "_land_default.dds";
    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    terrain_cells.insert((0, 0), make_cell_with_table((0, 0), &[(0, "a.dds")], |_x, _y| 1));
    terrain_cells.insert((-1, 0), make_cell_with_table((-1, 0), &[(0, "b.dds")], |_x, _y| 1));

    let (texture_ids, material_plan, region) = plan_materials_for_tests(
        &terrain_cells,
        default_key,
        &[
            (default_key, [0, 0, 0, 255]),
            ("a.dds", [1, 2, 3, 255]),
            ("b.dds", [4, 5, 6, 255]),
        ],
    );
    assert_patch_matches_planner(
        &terrain_cells,
        &texture_ids,
        &material_plan,
        region,
        (0, 0),
        (0, 5),
        default_key,
    );
    let material = patch_material_at(&material_plan, region, (0, 0), (0, 5));
    assert_eq!(texture_ids.ordered_paths[material.base_id as usize], "a.dds");
    assert_eq!(texture_ids.ordered_paths[material.decal_id as usize], "b.dds");
}

#[test]
fn different_raw_indices_resolving_to_same_path_share_identity() {
    let default_key = "_land_default.dds";
    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    terrain_cells.insert((0, 0), make_cell_with_table((0, 0), &[(0, "same.dds")], |_x, _y| 1));
    terrain_cells.insert((-1, 0), make_cell_with_table((-1, 0), &[(1, "same.dds")], |_x, _y| 2));

    let (texture_ids, material_plan, region) = plan_materials_for_tests(
        &terrain_cells,
        default_key,
        &[(default_key, [0, 0, 0, 255]), ("same.dds", [1, 2, 3, 255])],
    );
    assert_patch_matches_planner(
        &terrain_cells,
        &texture_ids,
        &material_plan,
        region,
        (0, 0),
        (0, 5),
        default_key,
    );
    let material = patch_material_at(&material_plan, region, (0, 0), (0, 5));
    assert_eq!(texture_ids.ordered_paths[material.base_id as usize], "same.dds");
    assert_eq!(texture_ids.ordered_paths[material.decal_id as usize], "same.dds");
    assert_eq!(material.flags & FLAG_HAS_DECAL, 0);
}

#[test]
fn patch_albedo_matches_planner_for_base_only_patch() {
    let default_key = "_land_default.dds";
    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    terrain_cells.insert(
        (0, 0),
        make_cell_with_table(
            (0, 0),
            &[(0, "a.dds")],
            |patch_x, patch_y| {
                if patch_x == 5 && patch_y == 5 { 1 } else { 0 }
            },
        ),
    );

    let cache = make_texture_cache_from_images(
        default_key,
        vec![
            (
                default_key,
                make_test_image([
                    [[10, 20, 30, 255], [40, 50, 60, 255]],
                    [[70, 80, 90, 255], [100, 110, 120, 255]],
                ]),
            ),
            (
                "a.dds",
                make_test_image([[[0, 0, 255, 255], [255, 0, 0, 255]], [[0, 255, 0, 255], [255, 255, 0, 255]]]),
            ),
        ],
    );
    let texture_ids = collect_terrain_texture_ids(&cache, TextureDedupeMode::Off).unwrap();
    let region = terrain_control_region(&terrain_cells);
    let material_plan = build_terrain_material_plan(&terrain_cells, &texture_ids, region).unwrap();

    assert_patch_albedo_matches_planner(
        &terrain_cells,
        &cache,
        &texture_ids,
        &material_plan,
        region,
        (0, 0),
        (5, 5),
        default_key,
    );
}

#[test]
fn patch_albedo_matches_planner_for_blended_patch() {
    let default_key = "_land_default.dds";
    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    terrain_cells.insert(
        (0, 0),
        make_cell_with_table(
            (0, 0),
            &[(0, "current.dds"), (1, "north.dds"), (2, "west.dds"), (3, "nw.dds")],
            |patch_x, patch_y| match (patch_x, patch_y) {
                (5, 5) => 1,
                (5, 6) => 2,
                (4, 5) => 3,
                (4, 6) => 4,
                _ => 1,
            },
        ),
    );

    let cache = make_texture_cache_from_images(
        default_key,
        vec![
            (
                default_key,
                make_test_image([
                    [[16, 24, 32, 255], [48, 56, 64, 255]],
                    [[80, 88, 96, 255], [112, 120, 128, 255]],
                ]),
            ),
            (
                "current.dds",
                make_test_image([[[32, 0, 0, 255], [128, 0, 0, 255]], [[224, 0, 0, 255], [255, 64, 64, 255]]]),
            ),
            (
                "north.dds",
                make_test_image([[[0, 32, 0, 255], [0, 128, 0, 255]], [[0, 224, 0, 255], [64, 255, 64, 255]]]),
            ),
            (
                "west.dds",
                make_test_image([[[0, 0, 32, 255], [0, 0, 128, 255]], [[0, 0, 224, 255], [64, 64, 255, 255]]]),
            ),
            (
                "nw.dds",
                make_test_image([
                    [[32, 32, 0, 255], [128, 128, 0, 255]],
                    [[224, 224, 0, 255], [255, 255, 64, 255]],
                ]),
            ),
        ],
    );
    let texture_ids = collect_terrain_texture_ids(&cache, TextureDedupeMode::Off).unwrap();
    let region = terrain_control_region(&terrain_cells);
    let material_plan = build_terrain_material_plan(&terrain_cells, &texture_ids, region).unwrap();

    assert_patch_albedo_matches_planner(
        &terrain_cells,
        &cache,
        &texture_ids,
        &material_plan,
        region,
        (0, 0),
        (5, 5),
        default_key,
    );
}

#[test]
fn patch_albedo_uses_default_material_average_for_missing_cells() {
    let default_key = "_land_default.dds";
    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    terrain_cells.insert((0, 0), make_cell((0, 0), "base.dds"));
    terrain_cells.insert((2, 0), make_cell((2, 0), "base.dds"));

    let cache = make_texture_cache_from_images(
        default_key,
        vec![
            (
                default_key,
                make_test_image([
                    [[10, 40, 70, 255], [20, 50, 80, 255]],
                    [[30, 60, 90, 255], [40, 70, 100, 255]],
                ]),
            ),
            (
                "base.dds",
                make_test_image([
                    [[200, 0, 0, 255], [200, 10, 10, 255]],
                    [[200, 20, 20, 255], [200, 30, 30, 255]],
                ]),
            ),
        ],
    );
    let texture_ids = collect_terrain_texture_ids(&cache, TextureDedupeMode::Off).unwrap();
    let region = terrain_control_region(&terrain_cells);
    let material_plan = build_terrain_material_plan(&terrain_cells, &texture_ids, region).unwrap();
    let patch_albedo = build_patch_albedo_for_tests(&cache, &texture_ids, &material_plan, region);
    let default_pattern_id = u8::try_from(
        material_plan
            .patterns
            .binary_search(&EMPTY_BLEND_ALPHA_GRID)
            .expect("empty pattern exists"),
    )
    .unwrap();
    let patch_grid = patch_sample_grid(material_plan.pattern_atlas.logical_tile_size);
    let pattern_images = rasterize_blend_patterns(&material_plan.patterns, material_plan.pattern_atlas.logical_tile_size);
    let expected = average_patch_material_albedo(
        &cache,
        &texture_ids,
        PatchMaterial {
            base_id: 0,
            decal_id: 0,
            pattern_id: default_pattern_id,
            flags: FLAG_USES_DEFAULT_TEXTURE,
        },
        &pattern_images,
        &patch_grid,
    );

    assert_eq!(*patch_albedo.get_pixel(16, 0), expected);
    assert_eq!(*patch_albedo.get_pixel(31, 15), expected);
}

#[test]
fn patch_albedo_dds_uses_dxt1_and_full_mips() {
    let default_key = "_land_default.dds";
    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    terrain_cells.insert((0, 0), make_cell((0, 0), "base.dds"));
    terrain_cells.insert((1, 0), make_cell((1, 0), "base.dds"));

    let cache = make_texture_cache(
        default_key,
        &[(default_key, [12, 34, 56, 255]), ("base.dds", [78, 90, 123, 255])],
    );
    let texture_ids = collect_terrain_texture_ids(&cache, TextureDedupeMode::Off).unwrap();
    let region = terrain_control_region(&terrain_cells);
    let material_plan = build_terrain_material_plan(&terrain_cells, &texture_ids, region).unwrap();
    let patch_albedo = build_patch_albedo_for_tests(&cache, &texture_ids, &material_plan, region);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("terrain_patch_albedo.dds");

    crate::texture::save_bc1_dds_unflipped(patch_albedo, &path, true).unwrap();

    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[0..4], b"DDS ");
    assert_eq!(&bytes[84..88], b"DXT1");

    let mut cursor = std::io::Cursor::new(&bytes);
    let dds = image_dds::ddsfile::Dds::read(&mut cursor).unwrap();
    assert_eq!(dds.get_width(), 32);
    assert_eq!(dds.get_height(), 16);
    assert_eq!(dds.get_num_mipmap_levels(), 6);
}

#[test]
fn exact_dedupe_collapses_identical_terrain_textures() {
    // `from_base` derives `bytes_hash` from the base pixels, so two entries with the same color
    // share a Tier-0 source fingerprint and collapse to one canonical material ID.
    let cache = make_texture_cache("base.dds", &[("a.dds", [10, 20, 30, 255]), ("b.dds", [10, 20, 30, 255])]);
    let ids = collect_terrain_texture_ids(&cache, TextureDedupeMode::Exact).unwrap();

    assert_eq!(ids.ids_by_path["a.dds"], ids.ids_by_path["b.dds"]);
    assert_ne!(
        ids.ids_by_path["a.dds"], 0,
        "aliased original must not silently fall back to default"
    );
    assert_eq!(ids.ordered_paths.len(), 2, "default + one canonical");
}

#[test]
fn off_mode_keeps_identical_terrain_textures_separate() {
    let cache = make_texture_cache("base.dds", &[("a.dds", [10, 20, 30, 255]), ("b.dds", [10, 20, 30, 255])]);
    let ids = collect_terrain_texture_ids(&cache, TextureDedupeMode::Off).unwrap();

    assert_ne!(ids.ids_by_path["a.dds"], ids.ids_by_path["b.dds"]);
    assert_eq!(ids.ordered_paths.len(), 3, "default + a + b");
}

#[test]
fn phase1_terrain_does_not_alias_on_decoded_identity() {
    // Identical decoded base pixels but different source-byte fingerprints must stay distinct:
    // Terrain material identity is Tier-0 (source-byte) only; decoded/mip/BC1 identity is deferred.
    let base = RgbaImage::from_pixel(1, 1, Rgba([10, 20, 30, 255]));
    let mut a = TerrainTexture::from_base(base.clone());
    a.bytes_hash = [1u8; 32];
    let mut b = TerrainTexture::from_base(base);
    b.bytes_hash = [2u8; 32];

    let mut images: IndexMap<&str, TerrainTexture> = Default::default();
    images.insert(
        "base.dds",
        TerrainTexture::from_base(RgbaImage::from_pixel(1, 1, Rgba([69, 51, 33, 255]))),
    );
    images.insert("a.dds", a);
    images.insert("b.dds", b);
    let cache = TerrainTextureCache {
        images,
        default_key: "base.dds",
    };

    let ids = collect_terrain_texture_ids(&cache, TextureDedupeMode::Exact).unwrap();
    assert_ne!(ids.ids_by_path["a.dds"], ids.ids_by_path["b.dds"]);
    assert_eq!(ids.ordered_paths.len(), 3, "default + two distinct canonicals");
}

#[test]
fn terrain_default_maps_to_zero_and_unknown_is_absent() {
    // The default texture is ID 0; an originally-known-but-not-loaded path is absent from
    // `ids_by_path`, so material planning's `unwrap_or(0)` intentionally resolves it to default.
    let cache = make_texture_cache("base.dds", &[("a.dds", [10, 20, 30, 255])]);
    let ids = collect_terrain_texture_ids(&cache, TextureDedupeMode::Exact).unwrap();

    assert_eq!(ids.ids_by_path["base.dds"], 0);
    assert!(ids.ids_by_path.get("never-loaded.dds").is_none());
    assert_ne!(ids.ids_by_path["a.dds"], 0);
}

#[test]
fn pattern_ids_survive_slot_remap_across_cells_and_holes() {
    // Grids are interned in first-seen order while the atlas is sorted, so the plan is only
    // correct if every patch's slot is remapped. A uniform cell blends only against its missing
    // neighbours; the checkerboard cell blends on every interior edge, and its north-blend grids
    // sort ahead of the uniform cell's, so first-seen order and atlas order disagree.
    let default_key = "_land_default.dds";
    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    terrain_cells.insert((0, 0), make_cell_with_table((0, 0), &[(1, "a.dds")], |_x, _y| 1));
    terrain_cells.insert(
        (2, 0),
        make_cell_with_table(
            (2, 0),
            &[(1, "a.dds"), (2, "b.dds")],
            |x, y| if (x + y) % 2 == 0 { 1 } else { 2 },
        ),
    );

    let (texture_ids, material_plan, region) = plan_materials_for_tests(
        &terrain_cells,
        default_key,
        &[
            (default_key, [0, 0, 0, 255]),
            ("a.dds", [1, 2, 3, 255]),
            ("b.dds", [4, 5, 6, 255]),
        ],
    );

    assert!(
        material_plan.patterns.len() >= 3,
        "world must produce several distinct blend patterns to exercise the remap, got {}",
        material_plan.patterns.len()
    );

    for cell in [(0, 0), (2, 0)] {
        for patch_y in 0..16 {
            for patch_x in 0..16 {
                assert_patch_matches_planner(
                    &terrain_cells,
                    &texture_ids,
                    &material_plan,
                    region,
                    cell,
                    (patch_x, patch_y),
                    default_key,
                );
            }
        }
    }

    // Cell (1, 0) is a hole inside the region: its patches keep the prefilled default material,
    // whose pattern ID is remapped from slot 0 like any other.
    let empty_pattern_id = u8::try_from(
        material_plan
            .patterns
            .binary_search(&EMPTY_BLEND_ALPHA_GRID)
            .expect("empty pattern exists"),
    )
    .unwrap();
    for patch_y in 0..16 {
        for patch_x in 0..16 {
            assert_eq!(
                patch_material_at(&material_plan, region, (1, 0), (patch_x, patch_y)),
                PatchMaterial {
                    base_id: 0,
                    decal_id: 0,
                    pattern_id: empty_pattern_id,
                    flags: FLAG_USES_DEFAULT_TEXTURE,
                }
            );
        }
    }
}
