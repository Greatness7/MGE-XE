use std::sync::Arc;

use super::*;
use crate::IndexMap;
use itertools::Itertools;

fn make_heights(fill: f32) -> Box<[[f32; 65]; 65]> {
    Box::new([[fill; 65]; 65])
}

fn make_cell(grid: (i32, i32), normal: Vec3, color: Vec4) -> crate::texture::TerrainCell<'static> {
    crate::texture::TerrainCell {
        grid,
        heights: make_heights(0.0),
        normals: vec![normal; 65 * 65],
        colors: vec![color; 65 * 65],
        texture_indices: Box::new([[0; 16]; 16]),
        texture_table: Arc::new(IndexMap::default()),
    }
}

fn insert_uniform_cell(
    terrain_cells: &mut crate::texture::TerrainCells<'static>,
    grid: (i32, i32),
    normal: Vec3,
    color: Vec4,
    height: f32,
) {
    let mut cell = make_cell(grid, normal, color);
    cell.heights = make_heights(height);
    terrain_cells.insert(grid, cell);
}

fn default_color() -> Vec4 {
    Vec4::new(1.0, 1.0, 1.0, 0.0)
}

fn insert_default_uniform_cell(terrain_cells: &mut crate::texture::TerrainCells<'static>, grid: (i32, i32), height: f32) {
    insert_uniform_cell(terrain_cells, grid, Vec3::Z, default_color(), height);
}

fn populate_default_sampled_block(
    terrain_cells: &mut crate::texture::TerrainCells<'static>,
    start: (i32, i32),
    mesh_chunk_cells_per_side: usize,
    height: f32,
) {
    let span = mesh_chunk_cells_per_side as i32;
    for cell_y in start.1..=start.1 + span {
        for cell_x in start.0..=start.0 + span {
            insert_default_uniform_cell(terrain_cells, (cell_x, cell_y), height);
        }
    }
}

fn make_region(min_x: i32, max_x: i32, min_y: i32, max_y: i32) -> TerrainAtlasRegion {
    TerrainAtlasRegion {
        min_x,
        max_x,
        min_y,
        max_y,
        offset_x: 0,
        offset_y: 0,
    }
}

fn collect_default_cells(terrain_cells: &crate::texture::TerrainCells<'_>) -> HashSet<(i32, i32)> {
    terrain_cells
        .iter()
        .filter_map(|(&grid, cell)| cell.is_default().then_some(grid))
        .collect()
}

/// Target-key set covering every populated cell, reproducing the pre-scoping
/// whole-world smoothing input so the field values a test inspects are unchanged.
fn all_target_keys(terrain_cells: &crate::texture::TerrainCells<'_>) -> HashSet<(i32, i32)> {
    terrain_cells.keys().copied().collect()
}

/// Builds a minimal single-patch work item at an absolute start cell. Only its
/// `key` participates in target-key derivation; the rest satisfies the struct.
fn work_item_starting_at(start_cell_x: i32, start_cell_y: i32) -> TerrainMeshWorkItem {
    let span = MESH_CHUNK_CELLS_PER_SIDE as i32;
    let region = make_region(start_cell_x, start_cell_x + span - 1, start_cell_y, start_cell_y + span - 1);
    TerrainMeshWorkItem {
        key: TerrainMeshWorkKey {
            start_cell_x,
            start_cell_y,
            cells_per_side: MESH_CHUNK_CELLS_PER_SIDE as u32,
        },
        dependencies: Vec::new(),
        region,
        patch_x: 0,
        patch_y: 0,
        cell_rect: WorkCellRect::owned_by(&region, start_cell_x, start_cell_y),
    }
}

fn dense_vertex(verts: &[DenseVertex], vertices_per_edge: usize, ix: usize, iy: usize) -> DenseVertex {
    verts[iy * vertices_per_edge + ix]
}

/// Test-local helper that calls [`build_dense_mesh_chunk_vertices_into`] with a
/// fresh `Vec`, taking the region's minimum cell coordinates directly.
fn build_test_dense_vertices<'a>(
    terrain_cells: &TerrainCells<'a>,
    smoothed_normals: &HashMap<(i32, i32), Vec<Vec3>>,
    region_min_cell_x: i32,
    region_min_cell_y: i32,
    patch_x: usize,
    patch_y: usize,
    mesh_chunk_cells_per_side: usize,
) -> Vec<DenseVertex> {
    let mut vertices = Vec::new();
    build_dense_mesh_chunk_vertices_into(
        terrain_cells,
        smoothed_normals,
        region_min_cell_x,
        region_min_cell_y,
        patch_x,
        patch_y,
        mesh_chunk_cells_per_side,
        &mut vertices,
    );
    vertices
}

fn bits3(value: Vec3) -> [u32; 3] {
    value.to_array().map(f32::to_bits)
}

fn bits4(value: [f32; 4]) -> [u32; 4] {
    value.map(f32::to_bits)
}

#[test]
fn smoothed_simplifier_normals_blend_across_cell_borders() {
    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    terrain_cells.insert((0, 0), make_cell((0, 0), Vec3::X, Vec4::ONE));
    terrain_cells.insert((1, 0), make_cell((1, 0), Vec3::Y, Vec4::ONE));

    let smoothed = build_smoothed_simplifier_normals(&terrain_cells, &all_target_keys(&terrain_cells));
    let left_edge = smoothed[&(0, 0)][32 * 65 + 64];
    let right_edge = smoothed[&(1, 0)][32 * 65];

    assert!(left_edge.x > 0.2 && left_edge.y > 0.2);
    assert!(right_edge.x > 0.2 && right_edge.y > 0.2);
    assert!(left_edge.x > left_edge.y);
    assert!(right_edge.y > right_edge.x);
}

#[test]
fn smoothed_simplifier_normals_missing_neighbors_use_origin_clamped_samples() {
    let mut cell = make_cell((0, 0), Vec3::X, Vec4::ONE);
    cell.normals[0] = Vec3::X;
    cell.normals[1] = Vec3::Y;
    cell.normals[65] = Vec3::Z;
    cell.normals[66] = Vec3::NEG_X;

    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    terrain_cells.insert((0, 0), cell);

    let smoothed = build_smoothed_simplifier_normals(&terrain_cells, &all_target_keys(&terrain_cells));
    let sampled = smoothed[&(0, 0)][0];
    let expected =
        ((Vec3::X * 4.0 + Vec3::Y * 2.0 + Vec3::Z * 2.0 + Vec3::NEG_X) / 9.0).normalize_or(DEFAULT_FALLBACK_NORMAL);

    assert!((sampled - expected).length() <= f32::EPSILON);
}

#[test]
fn target_keys_cover_full_span_including_positive_boundary() {
    let work = [work_item_starting_at(0, 0)];
    let keys = smoothed_normal_target_keys(&work);

    // Side S=4 samples inclusive keys 0..=4 on each axis: a 5x5 block.
    let expected: HashSet<(i32, i32)> = (0..=4).flat_map(|y| (0..=4).map(move |x| (x, y))).collect();
    assert_eq!(keys, expected);
    // The sampled +X/+Y boundary key must be present even though the owned
    // (clipped) rectangle stops one cell short of it.
    assert!(keys.contains(&(4, 4)));
}

#[test]
fn target_keys_dedup_overlapping_work() {
    // Adjacent chunks share their boundary column; the deduplicated set must not
    // grow it into two distinct entries.
    let work = [work_item_starting_at(0, 0), work_item_starting_at(4, 0)];
    let keys = smoothed_normal_target_keys(&work);

    let expected: HashSet<(i32, i32)> = (0..=4).flat_map(|y| (0..=8).map(move |x| (x, y))).collect();
    assert_eq!(keys, expected);
    // Union of x-ranges 0..=4 and 4..=8 is 0..=8 (9 columns), not 10.
    assert_eq!(keys.len(), 9 * 5);
}

#[test]
fn target_keys_handle_negative_coordinates() {
    let work = [work_item_starting_at(-4, -2)];
    let keys = smoothed_normal_target_keys(&work);

    let expected: HashSet<(i32, i32)> = (-2..=2).flat_map(|y| (-4..=0).map(move |x| (x, y))).collect();
    assert_eq!(keys, expected);
    assert!(keys.contains(&(-4, -2))); // origin
    assert!(keys.contains(&(0, 2))); // +X/+Y boundary
}

#[test]
fn build_smoothed_skips_absent_target_keys() {
    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    terrain_cells.insert((0, 0), make_cell((0, 0), Vec3::X, Vec4::ONE));

    let mut targets = HashSet::new();
    targets.insert((0, 0));
    targets.insert((9, 9)); // never populated

    let smoothed = build_smoothed_simplifier_normals(&terrain_cells, &targets);
    assert!(smoothed.contains_key(&(0, 0)));
    assert!(!smoothed.contains_key(&(9, 9)));
}

#[test]
fn build_smoothed_is_bounded_and_matches_full_field_for_targeted_cell() {
    // Three populated cells; only (0, 0) is targeted. (1, 0) is its neighbor and
    // must still contribute to the field even though it is not itself a target;
    // (50, 50) is unrelated and must be excluded entirely.
    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    terrain_cells.insert((0, 0), make_cell((0, 0), Vec3::X, Vec4::ONE));
    terrain_cells.insert((1, 0), make_cell((1, 0), Vec3::Y, Vec4::ONE));
    terrain_cells.insert((50, 50), make_cell((50, 50), Vec3::Z, Vec4::ONE));

    let full = build_smoothed_simplifier_normals(&terrain_cells, &all_target_keys(&terrain_cells));

    let mut targets = HashSet::new();
    targets.insert((0, 0));
    let bounded = build_smoothed_simplifier_normals(&terrain_cells, &targets);

    // Bounded smoothing computes only the targeted cell, excluding unrelated
    // populated cells (and even the untargeted neighbor).
    assert_eq!(bounded.len(), 1);
    assert!(bounded.contains_key(&(0, 0)));
    assert!(!bounded.contains_key(&(1, 0)));
    assert!(!bounded.contains_key(&(50, 50)));

    // The targeted cell's field is bit-identical to the whole-world computation,
    // including the cross-border blend from its still-present neighbor (1, 0).
    let bounded_bits: Vec<[u32; 3]> = bounded[&(0, 0)].iter().map(|n| bits3(*n)).collect();
    let full_bits: Vec<[u32; 3]> = full[&(0, 0)].iter().map(|n| bits3(*n)).collect();
    assert_eq!(bounded_bits, full_bits);
}

#[test]
fn build_default_cells_skips_absent_target_keys() {
    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    insert_default_uniform_cell(&mut terrain_cells, (0, 0), 0.0);

    let mut targets = HashSet::new();
    targets.insert((0, 0));
    targets.insert((9, 9)); // never populated

    let default_cells = build_default_cells(&terrain_cells, &targets);
    assert!(default_cells.contains(&(0, 0)));
    assert!(!default_cells.contains(&(9, 9)));
}

#[test]
fn build_default_cells_is_bounded_to_target_keys() {
    // Two default cells and one non-default cell; only (0, 0) and (1, 0) are
    // targeted. (50, 50) is default but unrelated, and a whole-world scan would
    // include it. The bounded result must exclude it entirely.
    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    insert_default_uniform_cell(&mut terrain_cells, (0, 0), 0.0);
    insert_default_uniform_cell(&mut terrain_cells, (50, 50), 0.0);
    insert_uniform_cell(&mut terrain_cells, (1, 0), Vec3::X, Vec4::ONE, 1.0);

    let full = build_default_cells(&terrain_cells, &all_target_keys(&terrain_cells));
    assert_eq!(full.len(), 2);
    assert!(full.contains(&(0, 0)));
    assert!(full.contains(&(50, 50)));

    let mut targets = HashSet::new();
    targets.insert((0, 0));
    targets.insert((1, 0));
    let bounded = build_default_cells(&terrain_cells, &targets);

    assert_eq!(bounded.len(), 1);
    assert!(bounded.contains(&(0, 0)));
    assert!(!bounded.contains(&(1, 0))); // populated but not default
    assert!(!bounded.contains(&(50, 50))); // default but outside target_keys
}

#[test]
fn default_chunk_at_deep_water_returns_none() {
    let region = make_region(0, 3, 0, 3);
    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    populate_default_sampled_block(&mut terrain_cells, (0, 0), MESH_CHUNK_CELLS_PER_SIDE, DEEP_WATER_Z);

    let default_cells = collect_default_cells(&terrain_cells);
    let uniform_height = default_chunk_uniform_height(&terrain_cells, &default_cells, &region, 0, 0)
        .expect("uniform default block should classify");

    assert!(build_default_terrain_mesh(uniform_height, 0.0, 0.0, 0, 0).is_none());
}

#[test]
fn default_chunk_at_surface_returns_flat_quad() {
    let region = make_region(0, 3, 0, 3);
    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    populate_default_sampled_block(&mut terrain_cells, (0, 0), MESH_CHUNK_CELLS_PER_SIDE, 0.0);

    let default_cells = collect_default_cells(&terrain_cells);
    let uniform_height = default_chunk_uniform_height(&terrain_cells, &default_cells, &region, 0, 0)
        .expect("uniform default block should classify");
    let mesh =
        build_default_terrain_mesh(uniform_height, 0.0, 0.0, 0, 0).expect("surface-height default chunk should emit a quad");

    assert_eq!(mesh.vertices.len(), 4);
    assert_eq!(mesh.triangles, vec![[1, 3, 0], [3, 2, 0]]);
    assert!(mesh.vertices.iter().all(|vertex| vertex.position[2] == 0.0));
}

#[test]
fn default_quad_has_correct_bounds() {
    let mesh = build_default_terrain_mesh(32.0, -LAND_CELL_SIZE, 2.0 * LAND_CELL_SIZE, 1, 1)
        .expect("surface-height default chunk should emit a quad");
    let bounds = mesh_chunk_bounds(-LAND_CELL_SIZE, 2.0 * LAND_CELL_SIZE, 1, 1, MESH_CHUNK_CELLS_PER_SIDE);
    let expected_min = Vec3::new(bounds.left, bounds.bottom, 32.0);
    let expected_max = Vec3::new(bounds.right, bounds.top, 32.0);
    let expected_center = (expected_min + expected_max) * 0.5;
    let expected_radius = [
        expected_min,
        Vec3::new(bounds.right, bounds.bottom, 32.0),
        Vec3::new(bounds.left, bounds.top, 32.0),
        expected_max,
    ]
    .into_iter()
    .fold(0.0f32, |acc, position| acc.max(position.distance_squared(expected_center)))
    .sqrt();

    assert_eq!(mesh.bounding_box.min, expected_min);
    assert_eq!(mesh.bounding_box.max, expected_max);
    assert_eq!(mesh.bounding_sphere.center, expected_center);
    assert_eq!(mesh.bounding_sphere.radius, expected_radius);
}

#[test]
fn default_quad_has_correct_normals_and_colors() {
    let mesh = build_default_terrain_mesh(0.0, 0.0, 0.0, 0, 0).expect("surface-height default chunk should emit a quad");

    for vertex in &mesh.vertices {
        assert_eq!(vertex.normal, [128, 128, 255, 255]);
        assert_eq!(vertex.color, [255, 255, 255, 255]);
    }
}

#[test]
fn neighbor_edge_disqualifies_chunk() {
    let region = make_region(0, 3, 0, 3);
    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    populate_default_sampled_block(&mut terrain_cells, (0, 0), MESH_CHUNK_CELLS_PER_SIDE, 0.0);
    insert_default_uniform_cell(&mut terrain_cells, (4, 2), 128.0);
    let default_cells = collect_default_cells(&terrain_cells);

    assert_eq!(
        default_chunk_uniform_height(&terrain_cells, &default_cells, &region, 0, 0),
        None
    );
}

#[test]
fn edge_of_region_chunk_matches_builder() {
    let region = make_region(0, 2, 0, 2);
    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    for cell_y in 0..=2 {
        for cell_x in 0..=2 {
            insert_default_uniform_cell(&mut terrain_cells, (cell_x, cell_y), 0.0);
        }
    }
    let smoothed_normals = HashMap::default();
    let vertices_per_edge = dense_vertices_per_edge(MESH_CHUNK_CELLS_PER_SIDE);
    let vertices = build_test_dense_vertices(&terrain_cells, &smoothed_normals, 0, 0, 0, 0, MESH_CHUNK_CELLS_PER_SIDE);
    let default_cells = collect_default_cells(&terrain_cells);

    assert!(mesh_chunk_contains_populated_cells(
        &terrain_cells,
        WorkCellRect::owned_by(&region, region.min_x, region.min_y)
    ));
    assert_eq!(
        default_chunk_uniform_height(&terrain_cells, &default_cells, &region, 0, 0),
        None
    );
    assert_eq!(
        dense_vertex(&vertices, vertices_per_edge, vertices_per_edge - 1, vertices_per_edge - 1).position[2],
        DEEP_WATER_Z
    );
}

#[test]
fn mixed_default_and_nondefault_chunk_takes_normal_path() {
    let region = make_region(0, 3, 0, 3);
    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    populate_default_sampled_block(&mut terrain_cells, (0, 0), MESH_CHUNK_CELLS_PER_SIDE, 0.0);

    let mut nondefault = make_cell((2, 2), Vec3::Z, default_color());
    nondefault.heights = make_heights(0.0);
    nondefault.colors[7] = Vec4::new(0.5, 1.0, 1.0, 0.0);
    terrain_cells.insert((2, 2), nondefault);
    let default_cells = collect_default_cells(&terrain_cells);

    assert_eq!(
        default_chunk_uniform_height(&terrain_cells, &default_cells, &region, 0, 0),
        None
    );
}

#[test]
fn mixed_default_and_missing_chunk() {
    let region = make_region(0, 3, 0, 3);

    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    populate_default_sampled_block(&mut terrain_cells, (0, 0), MESH_CHUNK_CELLS_PER_SIDE, 0.0);
    // Missing center cell makes the dense sample grid non-uniform (surface vs deep water).
    terrain_cells.shift_remove(&(4, 4));
    let default_cells = collect_default_cells(&terrain_cells);
    assert_eq!(
        default_chunk_uniform_height(&terrain_cells, &default_cells, &region, 0, 0),
        None
    );

    let mut deep_water_cells: crate::texture::TerrainCells<'static> = Default::default();
    populate_default_sampled_block(&mut deep_water_cells, (0, 0), MESH_CHUNK_CELLS_PER_SIDE, DEEP_WATER_Z);
    // Missing cell falls back to DEEP_WATER_Z, matching the remaining default cells.
    deep_water_cells.shift_remove(&(4, 4));
    let deep_water_default_cells = collect_default_cells(&deep_water_cells);
    assert_eq!(
        default_chunk_uniform_height(&deep_water_cells, &deep_water_default_cells, &region, 0, 0),
        Some(DEEP_WATER_Z)
    );
}

#[test]
fn differing_uniform_heights_take_normal_path() {
    let region = make_region(0, 3, 0, 3);
    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    populate_default_sampled_block(&mut terrain_cells, (0, 0), MESH_CHUNK_CELLS_PER_SIDE, 0.0);
    insert_default_uniform_cell(&mut terrain_cells, (1, 1), 64.0);
    let default_cells = collect_default_cells(&terrain_cells);

    assert_eq!(
        default_chunk_uniform_height(&terrain_cells, &default_cells, &region, 0, 0),
        None
    );
}

#[test]
fn default_quad_covers_full_extent() {
    let mesh = build_default_terrain_mesh(64.0, LAND_CELL_SIZE, -LAND_CELL_SIZE, 0, 0)
        .expect("surface-height default chunk should emit a quad");
    let bounds = mesh_chunk_bounds(LAND_CELL_SIZE, -LAND_CELL_SIZE, 0, 0, MESH_CHUNK_CELLS_PER_SIDE);
    let expected_positions = [
        Vec3::new(bounds.left, bounds.bottom, 64.0),
        Vec3::new(bounds.right, bounds.bottom, 64.0),
        Vec3::new(bounds.left, bounds.top, 64.0),
        Vec3::new(bounds.right, bounds.top, 64.0),
    ];
    let positions = mesh.vertices.iter().map(|vertex| vertex.position).collect_vec();
    let mut referenced_indices = mesh.triangles.iter().flatten().copied().collect_vec();
    referenced_indices.sort_unstable();
    referenced_indices.dedup();

    for expected in expected_positions {
        assert!(positions.contains(&expected));
    }
    assert_eq!(referenced_indices, vec![0, 1, 2, 3]);
}

#[test]
fn seam_sampling_uses_position_owned_cell_so_position_only_dedup_stays_valid() {
    // Two cells that disagree on their uniform normal. The shared east seam (global
    // step LAND_STEPS_PER_CELL) must sample the east cell (1, 0) at local 0, never the
    // west cell (0, 0) at local 64. This is the floor-ownership rule that keeps position-only
    // vertex dedup across chunk seams valid.
    let mut left = make_cell((0, 0), Vec3::X, Vec4::ONE);
    left.normals.fill(Vec3::X);
    let mut right = make_cell((1, 0), Vec3::Y, Vec4::ONE);
    right.normals.fill(Vec3::Y);

    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    terrain_cells.insert((0, 0), left);
    terrain_cells.insert((1, 0), right);

    let smoothed_normals = HashMap::default();
    // A one-cell chunk at region cell (0, 0): its far edge (ix == LAND_STEPS_PER_CELL)
    // lands exactly on the (0, 0)/(1, 0) seam.
    let vertices = build_test_dense_vertices(&terrain_cells, &smoothed_normals, 0, 0, 0, 0, 1);
    let vertices_per_edge = dense_vertices_per_edge(1);

    let interior = dense_vertex(&vertices, vertices_per_edge, LAND_STEPS_PER_CELL - 1, 0);
    assert_eq!(interior.raw_normal, Vec3::X);

    let seam = dense_vertex(&vertices, vertices_per_edge, LAND_STEPS_PER_CELL, 0);
    assert_eq!(seam.raw_normal, Vec3::Y);
}

#[test]
fn sampled_normal_and_color_pack_into_terrain_vertex_layout() {
    let mut cell = make_cell((0, 0), Vec3::Z, Vec4::new(0.25, 0.5, 0.75, 0.0));
    cell.colors.fill(Vec4::new(0.25, 0.5, 0.75, 0.0));
    cell.normals.fill(Vec3::Z);
    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    terrain_cells.insert((0, 0), cell);

    let smoothed_normals = HashMap::default();
    let vertices = build_test_dense_vertices(&terrain_cells, &smoothed_normals, 0, 0, 0, 0, 1);
    let vertex = dense_vertex(&vertices, dense_vertices_per_edge(1), 1, 1);

    let normal = pack_ubyte4n_bias_normal(vertex.raw_normal);
    let color = pack_vertex_color(Vec4::from_array(vertex.color));
    assert_eq!(normal, [128, 128, 255, 255]);
    assert_eq!(color, pack_d3dcolor_vclr(64, 128, 191, 255));
}

#[test]
fn dense_vertex_layout_matches_simplifier_contract() {
    assert_eq!(size_of::<DenseVertex>(), 52);
    assert_eq!(std::mem::offset_of!(DenseVertex, smoothed_normal), 24);
}

#[test]
fn dense_mesh_chunk_grid_has_expected_default_counts() {
    let terrain_cells = crate::texture::TerrainCells::default();
    let smoothed_normals = HashMap::default();

    let vertices = build_test_dense_vertices(&terrain_cells, &smoothed_normals, 0, 0, 0, 0, MESH_CHUNK_CELLS_PER_SIDE);
    let indices = build_dense_index_grid();

    assert_eq!(dense_vertices_per_edge(MESH_CHUNK_CELLS_PER_SIDE), 257);
    assert_eq!(vertices.len(), 66_049);
    assert_eq!(indices.len() / 3, 131_072);
}

#[test]
fn dense_quad_indices_match_land_parity_and_winding() {
    let indices = build_dense_index_grid();

    assert_eq!(&indices[0..6], &[1, 258, 0, 258, 257, 0]);
    assert_eq!(&indices[6..12], &[258, 2, 259, 1, 2, 258]);
}

#[test]
fn dense_chunk_border_vertices_stay_on_integer_land_grid() {
    let terrain_cells = crate::texture::TerrainCells::default();
    let smoothed_normals = HashMap::default();
    let vertices_per_edge = dense_vertices_per_edge(MESH_CHUNK_CELLS_PER_SIDE);
    let vertices = build_test_dense_vertices(&terrain_cells, &smoothed_normals, -2, 3, 1, 2, MESH_CHUNK_CELLS_PER_SIDE);

    for i in 0..vertices_per_edge {
        for vertex in [
            dense_vertex(&vertices, vertices_per_edge, i, 0),
            dense_vertex(&vertices, vertices_per_edge, i, vertices_per_edge - 1),
            dense_vertex(&vertices, vertices_per_edge, 0, i),
            dense_vertex(&vertices, vertices_per_edge, vertices_per_edge - 1, i),
        ] {
            assert_eq!(vertex.position[0].rem_euclid(LAND_GRID_STEP), 0.0);
            assert_eq!(vertex.position[1].rem_euclid(LAND_GRID_STEP), 0.0);
        }
    }
}

#[test]
fn adjacent_dense_chunks_share_bit_identical_edge_positions() {
    let terrain_cells = crate::texture::TerrainCells::default();
    let smoothed_normals = HashMap::default();
    let vertices_per_edge = dense_vertices_per_edge(MESH_CHUNK_CELLS_PER_SIDE);

    let left = build_test_dense_vertices(&terrain_cells, &smoothed_normals, 0, 0, 0, 0, MESH_CHUNK_CELLS_PER_SIDE);
    let right = build_test_dense_vertices(&terrain_cells, &smoothed_normals, 0, 0, 1, 0, MESH_CHUNK_CELLS_PER_SIDE);

    for iy in 0..vertices_per_edge {
        let left_vertex = dense_vertex(&left, vertices_per_edge, vertices_per_edge - 1, iy);
        let right_vertex = dense_vertex(&right, vertices_per_edge, 0, iy);
        assert_eq!(bits3(left_vertex.position), bits3(right_vertex.position));
    }
}

#[test]
fn deep_water_dense_chunk_uses_documented_fallback_attributes() {
    let terrain_cells = crate::texture::TerrainCells::default();
    let smoothed_normals = HashMap::default();

    let vertices = build_test_dense_vertices(&terrain_cells, &smoothed_normals, 0, 0, 0, 0, MESH_CHUNK_CELLS_PER_SIDE);
    let vertex = vertices[0];

    assert_eq!(vertex.position[2], DEEP_WATER_Z);
    assert_eq!(vertex.raw_normal, DEFAULT_FALLBACK_NORMAL);
    assert_eq!(vertex.smoothed_normal, DEFAULT_FALLBACK_NORMAL);
    assert_eq!(vertex.color, DEFAULT_FALLBACK_COLOR.to_array());
}

#[test]
fn adjacent_dense_chunks_share_bit_identical_edge_attributes() {
    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    for y in 0..=4 {
        for x in 0..=8 {
            let normal = if x < 4 { Vec3::X } else { Vec3::Y };
            let color = if x < 4 {
                Vec4::new(1.0, 0.0, 0.0, 1.0)
            } else {
                Vec4::new(0.0, 1.0, 0.0, 1.0)
            };
            insert_uniform_cell(&mut terrain_cells, (x, y), normal, color, x as f32);
        }
    }
    let smoothed_normals = build_smoothed_simplifier_normals(&terrain_cells, &all_target_keys(&terrain_cells));
    let vertices_per_edge = dense_vertices_per_edge(MESH_CHUNK_CELLS_PER_SIDE);

    let left = build_test_dense_vertices(&terrain_cells, &smoothed_normals, 0, 0, 0, 0, MESH_CHUNK_CELLS_PER_SIDE);
    let right = build_test_dense_vertices(&terrain_cells, &smoothed_normals, 0, 0, 1, 0, MESH_CHUNK_CELLS_PER_SIDE);

    for iy in 0..vertices_per_edge {
        let left_vertex = dense_vertex(&left, vertices_per_edge, vertices_per_edge - 1, iy);
        let right_vertex = dense_vertex(&right, vertices_per_edge, 0, iy);
        assert_eq!(left_vertex.position[2].to_bits(), right_vertex.position[2].to_bits());
        assert_eq!(bits3(left_vertex.raw_normal), bits3(right_vertex.raw_normal));
        assert_eq!(bits3(left_vertex.smoothed_normal), bits3(right_vertex.smoothed_normal));
        assert_eq!(bits4(left_vertex.color), bits4(right_vertex.color));
    }
}

#[test]
fn dense_vertex_reads_grid_samples_directly() {
    // A single populated cell with a distinct value at one interior grid vertex. The
    // dense builder must read that vertex straight from the grids: the exact stored
    // height, the raw normal WITHOUT re-normalizing (the stored vector is deliberately
    // non-unit), the smoothed normal likewise un-renormalized, and the color clamped
    // with alpha forced to 1.
    let index = 5 * 65 + 3;
    let mut cell = make_cell((0, 0), Vec3::new(9.0, 9.0, 9.0), Vec4::new(0.1, 0.1, 0.1, 0.0));
    cell.heights[5][3] = 123.0;
    cell.normals[index] = Vec3::new(2.0, 0.0, 0.0);
    cell.colors[index] = Vec4::new(0.25, 0.5, 0.75, 0.0);

    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    terrain_cells.insert((0, 0), cell);

    let mut field = vec![Vec3::Z; 65 * 65];
    field[index] = Vec3::new(0.0, 3.0, 0.0);
    let mut smoothed_normals = HashMap::default();
    smoothed_normals.insert((0, 0), field);

    let vertices = build_test_dense_vertices(&terrain_cells, &smoothed_normals, 0, 0, 0, 0, 1);
    let vertex = dense_vertex(&vertices, dense_vertices_per_edge(1), 3, 5);

    assert_eq!(vertex.position[2], 123.0);
    assert_eq!(vertex.raw_normal, Vec3::new(2.0, 0.0, 0.0));
    assert_eq!(vertex.smoothed_normal, Vec3::new(0.0, 3.0, 0.0));
    assert_eq!(vertex.color, [0.25, 0.5, 0.75, 1.0]);
}

#[test]
fn dense_chunk_missing_north_east_neighbor_uses_fallbacks() {
    // Only the origin cell exists. Its own vertices read real data, but the shared
    // north/east seam addresses the absent (1, 0)/(0, 1) cells and must fall back to
    // the documented deep-water height and default normal/color.
    let mut cell = make_cell((0, 0), Vec3::X, Vec4::new(0.2, 0.4, 0.6, 0.0));
    cell.normals.fill(Vec3::X);
    cell.colors.fill(Vec4::new(0.2, 0.4, 0.6, 0.0));
    cell.heights = make_heights(50.0);

    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    terrain_cells.insert((0, 0), cell);

    let smoothed_normals = HashMap::default();
    let vertices = build_test_dense_vertices(&terrain_cells, &smoothed_normals, 0, 0, 0, 0, 1);
    let vertices_per_edge = dense_vertices_per_edge(1);

    let interior = dense_vertex(&vertices, vertices_per_edge, 10, 10);
    assert_eq!(interior.position[2], 50.0);
    assert_eq!(interior.raw_normal, Vec3::X);

    let east = dense_vertex(&vertices, vertices_per_edge, LAND_STEPS_PER_CELL, 10);
    assert_eq!(east.position[2], DEEP_WATER_Z);
    assert_eq!(east.raw_normal, DEFAULT_FALLBACK_NORMAL);
    assert_eq!(east.color, DEFAULT_FALLBACK_COLOR.to_array());

    let north = dense_vertex(&vertices, vertices_per_edge, 10, LAND_STEPS_PER_CELL);
    assert_eq!(north.position[2], DEEP_WATER_Z);
    assert_eq!(north.raw_normal, DEFAULT_FALLBACK_NORMAL);
    assert_eq!(north.color, DEFAULT_FALLBACK_COLOR.to_array());
}

#[test]
fn dense_chunk_negative_region_minima_read_correct_cells() {
    // Region minimum at a negative cell. Integer cell derivation must land on the
    // correct absolute cell for interior vertices and roll to the (absent) next cell
    // at the seam, all under signed arithmetic.
    let mut cell = make_cell((-2, -1), Vec3::Y, Vec4::ONE);
    cell.normals.fill(Vec3::Y);
    cell.heights = make_heights(7.0);

    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    terrain_cells.insert((-2, -1), cell);

    let smoothed_normals = HashMap::default();
    let vertices = build_test_dense_vertices(&terrain_cells, &smoothed_normals, -2, -1, 0, 0, 1);
    let vertices_per_edge = dense_vertices_per_edge(1);

    let interior = dense_vertex(&vertices, vertices_per_edge, 3, 5);
    assert_eq!(interior.position[0], -2.0 * LAND_CELL_SIZE + 3.0 * LAND_GRID_STEP);
    assert_eq!(interior.position[2], 7.0);
    assert_eq!(interior.raw_normal, Vec3::Y);

    // East seam rolls to cell (-1, -1), which is absent -> fallback.
    let east = dense_vertex(&vertices, vertices_per_edge, LAND_STEPS_PER_CELL, 5);
    assert_eq!(east.position[2], DEEP_WATER_Z);
}

#[test]
fn dense_chunk_beyond_region_max_uses_fallbacks() {
    // A full 4-cell chunk over a region that populates only its first cell (the
    // partial-final-chunk case). Vertices inside the populated cell read real data;
    // vertices in the unpopulated cells past the region extent fall back.
    let mut cell = make_cell((0, 0), Vec3::X, Vec4::ONE);
    cell.normals.fill(Vec3::X);
    cell.heights = make_heights(11.0);

    let mut terrain_cells: crate::texture::TerrainCells<'static> = Default::default();
    terrain_cells.insert((0, 0), cell);

    let smoothed_normals = HashMap::default();
    let vertices = build_test_dense_vertices(&terrain_cells, &smoothed_normals, 0, 0, 0, 0, MESH_CHUNK_CELLS_PER_SIDE);
    let vertices_per_edge = dense_vertices_per_edge(MESH_CHUNK_CELLS_PER_SIDE);

    // Interior of the sole populated cell (0, 0): real data.
    let inside = dense_vertex(&vertices, vertices_per_edge, 10, 10);
    assert_eq!(inside.position[2], 11.0);
    assert_eq!(inside.raw_normal, Vec3::X);

    // Deep inside cell (3, 0), well past the single populated cell: fallback.
    let beyond = dense_vertex(&vertices, vertices_per_edge, 3 * LAND_STEPS_PER_CELL + 10, 10);
    assert_eq!(beyond.position[2], DEEP_WATER_Z);
    assert_eq!(beyond.raw_normal, DEFAULT_FALLBACK_NORMAL);
    assert_eq!(beyond.color, DEFAULT_FALLBACK_COLOR.to_array());
}

#[test]
fn simplified_mesh_assembly_remaps_dense_vertex_indices() {
    let dense_verts = vec![
        DenseVertex {
            position: Vec3::new(0.0, 0.0, 1.0),
            raw_normal: Vec3::Z,
            smoothed_normal: Vec3::Z,
            color: Vec4::ONE.to_array(),
        },
        DenseVertex {
            position: Vec3::new(128.0, 0.0, 2.0),
            raw_normal: Vec3::Z,
            smoothed_normal: Vec3::Z,
            color: Vec4::ONE.to_array(),
        },
        DenseVertex {
            position: Vec3::new(128.0, 128.0, 3.0),
            raw_normal: Vec3::Z,
            smoothed_normal: Vec3::Z,
            color: Vec4::ONE.to_array(),
        },
        DenseVertex {
            position: Vec3::new(0.0, 128.0, 4.0),
            raw_normal: Vec3::Z,
            smoothed_normal: Vec3::Z,
            color: Vec4::ONE.to_array(),
        },
    ];

    let mut remap = Vec::new();
    let mesh = build_simplified_terrain_mesh(&dense_verts, &[0, 1, 2, 0, 2, 3], &mut remap)
        .unwrap()
        .expect("mesh should survive deep-water rejection");

    assert_eq!(mesh.vertices.len(), 4);
    assert_eq!(mesh.triangles.len(), 2);
    assert_eq!(mesh.bounding_box.min, Vec3::new(0.0, 0.0, 1.0));
    assert_eq!(mesh.bounding_box.max, Vec3::new(128.0, 128.0, 4.0));
}

#[test]
fn simplified_mesh_assembly_skips_all_deep_water_chunks() {
    let dense_verts = vec![
        DenseVertex {
            position: Vec3::new(0.0, 0.0, DEEP_WATER_Z),
            raw_normal: DEFAULT_FALLBACK_NORMAL,
            smoothed_normal: DEFAULT_FALLBACK_NORMAL,
            color: DEFAULT_FALLBACK_COLOR.to_array(),
        },
        DenseVertex {
            position: Vec3::new(128.0, 0.0, DEEP_WATER_Z),
            raw_normal: DEFAULT_FALLBACK_NORMAL,
            smoothed_normal: DEFAULT_FALLBACK_NORMAL,
            color: DEFAULT_FALLBACK_COLOR.to_array(),
        },
        DenseVertex {
            position: Vec3::new(0.0, 128.0, DEEP_WATER_Z),
            raw_normal: DEFAULT_FALLBACK_NORMAL,
            smoothed_normal: DEFAULT_FALLBACK_NORMAL,
            color: DEFAULT_FALLBACK_COLOR.to_array(),
        },
    ];

    let mut remap = Vec::new();
    assert!(
        build_simplified_terrain_mesh(&dense_verts, &[0, 1, 2], &mut remap)
            .unwrap()
            .is_none()
    );
}

#[test]
fn remap_scratch_reuse_produces_correct_second_mesh() {
    let dense_verts = vec![
        DenseVertex {
            position: Vec3::new(0.0, 0.0, 1.0),
            raw_normal: Vec3::Z,
            smoothed_normal: Vec3::Z,
            color: Vec4::ONE.to_array(),
        },
        DenseVertex {
            position: Vec3::new(128.0, 0.0, 2.0),
            raw_normal: Vec3::Z,
            smoothed_normal: Vec3::Z,
            color: Vec4::ONE.to_array(),
        },
        DenseVertex {
            position: Vec3::new(128.0, 128.0, 3.0),
            raw_normal: Vec3::Z,
            smoothed_normal: Vec3::Z,
            color: Vec4::ONE.to_array(),
        },
        DenseVertex {
            position: Vec3::new(0.0, 128.0, 4.0),
            raw_normal: Vec3::Z,
            smoothed_normal: Vec3::Z,
            color: Vec4::ONE.to_array(),
        },
    ];

    let mut remap = Vec::new();

    // First call uses vertices 0,1,2.
    let _first = build_simplified_terrain_mesh(&dense_verts, &[0, 1, 2], &mut remap)
        .unwrap()
        .expect("first call should produce a mesh");

    // Second call uses only vertices 1,2,3. No stale remap state should leak.
    let second = build_simplified_terrain_mesh(&dense_verts, &[1, 2, 3], &mut remap)
        .unwrap()
        .expect("second call should produce a mesh");

    assert_eq!(second.vertices.len(), 3);
    assert_eq!(second.triangles.len(), 1);
    assert_eq!(second.bounding_box.min, Vec3::new(0.0, 0.0, 2.0));
    assert_eq!(second.bounding_box.max, Vec3::new(128.0, 128.0, 4.0));
}

#[test]
fn dense_index_grid_is_invariant_across_chunks() {
    // The grid is invariant across chunks by construction: the helper takes no
    // patch coordinates and derives its extent from MESH_CHUNK_CELLS_PER_SIDE, so
    // only the resulting size is worth pinning.
    let indices = build_dense_index_grid();
    assert_eq!(indices.len(), dense_triangle_count() * 3);
}

fn key(start_cell_x: i32, start_cell_y: i32) -> TerrainMeshWorkKey {
    TerrainMeshWorkKey {
        start_cell_x,
        start_cell_y,
        cells_per_side: MESH_CHUNK_CELLS_PER_SIDE as u32,
    }
}

#[test]
fn duplicate_work_keys_are_rejected_when_assembling() {
    let results = vec![
        TerrainMeshWorkResult {
            key: key(0, 0),
            mesh: None,
        },
        TerrainMeshWorkResult {
            key: key(0, 0),
            mesh: None,
        },
    ];
    let error = assemble_terrain_mesh_set(results).unwrap_err().to_string();
    assert!(error.contains("duplicate terrain mesh work key"), "{error}");
}

#[test]
fn assembly_orders_by_key_and_drops_absent_work_after_association() {
    let mesh = TerrainMesh::default();
    // Deliberately out of key order, and with absent work interleaved, to prove the emitted order
    // is a property of the keys rather than of the order results arrived in.
    let results = vec![
        TerrainMeshWorkResult {
            key: key(4, 0),
            mesh: Some(mesh.clone()),
        },
        TerrainMeshWorkResult {
            key: key(0, 4),
            mesh: None,
        },
        TerrainMeshWorkResult {
            key: key(0, 0),
            mesh: Some(mesh.clone()),
        },
        TerrainMeshWorkResult {
            key: key(-4, 0),
            mesh: Some(mesh),
        },
    ];
    let set = assemble_terrain_mesh_set(results).unwrap();
    assert_eq!(set.emitted_keys, [key(-4, 0), key(0, 0), key(4, 0)]);
    assert_eq!(set.meshes.len(), set.emitted_keys.len());
}
