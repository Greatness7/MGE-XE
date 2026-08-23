use super::*;
use crate::abi::{SIZE_OF_TERRAIN_VERT, TerrainFileHeader, TerrainMeshHeader, TerrainMeshLayout, parse_terrain_occlusion};
use distantland::mge_xe::terrain_occlusion as generator_occlusion;

fn test_params() -> HorizonParams {
    HorizonParams {
        bin_count: 64,
        ring_count: 8,
        ring_step: 1024.0,
        r_near: 512.0,
        bias_z: 0.0,
        bias_obj_z: 0.0,
        march_step: 512.0,
        hierarchical_march: false,
    }
}

fn sphere(x: f32, y: f32, z: f32, radius: f32) -> BoundingSphere {
    BoundingSphere {
        center: D3dxVector3 { x, y, z },
        radius,
    }
}

fn aabb(min: (f32, f32, f32), max: (f32, f32, f32)) -> BoundingBox {
    let mut box_value = BoundingBox::default();
    box_value.set(
        D3dxVector3 {
            x: min.0,
            y: min.1,
            z: min.2,
        },
        D3dxVector3 {
            x: max.0,
            y: max.1,
            z: max.2,
        },
    );
    box_value
}

fn matches_any_corner(point: (f32, f32), corners: &[(f32, f32)]) -> bool {
    corners
        .iter()
        .any(|&(x, y)| (x - point.0).abs() < 1e-3 && (y - point.1).abs() < 1e-3)
}

fn assert_circle_contains_footprint(bounds: &HorizonMeshBounds) {
    let Some((center, radius)) = bounds.footprint_circle() else {
        panic!("missing footprint circle");
    };
    for &point in &bounds.footprint_xy[..bounds.vertex_count as usize] {
        let dx = point.0 - center.0;
        let dy = point.1 - center.1;
        let distance = (dx * dx + dy * dy).sqrt();
        assert!(
            distance <= radius + 1.0e-3,
            "point {point:?} outside circle {center:?} r={radius}"
        );
    }
}

#[test]
fn polygon_edge_distance_uses_nearest_edge_not_a_corner() {
    // CCW square offset along +x; the closest point to the origin is on the x = 10 edge,
    // not any corner.
    let square = [(10.0, -5.0), (20.0, -5.0), (20.0, 5.0), (10.0, 5.0)];

    let distance = min_distance_sq_to_polygon_edges(&square).sqrt();

    assert!((distance - 10.0).abs() < 0.0001, "{distance}");
}

#[test]
fn footprint_of_axis_aligned_box_is_its_rectangle() {
    let bounds = HorizonMeshBounds::from_box(aabb((-10.0, -20.0, 0.0), (30.0, 40.0, 100.0)));

    assert_eq!(bounds.vertex_count, 4);
    assert!((bounds.max_z - 100.0).abs() < 1e-3, "{}", bounds.max_z);

    let footprint = &bounds.footprint_xy[..bounds.vertex_count as usize];
    for corner in [(-10.0, -20.0), (30.0, -20.0), (30.0, 40.0), (-10.0, 40.0)] {
        assert!(matches_any_corner(corner, footprint), "missing corner {corner:?}");
    }
}

#[test]
fn footprint_circle_of_axis_aligned_rectangle_is_half_diagonal() {
    let bounds = HorizonMeshBounds::from_box(aabb((-10.0, -20.0, 0.0), (30.0, 40.0, 100.0)));

    let Some((center, radius)) = bounds.footprint_circle() else {
        panic!("valid rectangle should have a footprint circle");
    };
    let expected_radius = (20.0_f32 * 20.0 + 30.0 * 30.0).sqrt();

    assert!((center.0 - 10.0).abs() < 1.0e-3, "{center:?}");
    assert!((center.1 - 10.0).abs() < 1.0e-3, "{center:?}");
    assert!((radius - expected_radius).abs() < 1.0e-3, "{radius}");
    assert_circle_contains_footprint(&bounds);
}

#[test]
fn rotated_box_footprint_is_hull_subset_of_projected_corners() {
    // Basis vectors tilted off every world axis so the XY shadow is a full hexagon.
    let box_value = BoundingBox {
        center: D3dxVector3 {
            x: 5000.0,
            y: 1000.0,
            z: 200.0,
        },
        vx: D3dxVector3 {
            x: 100.0,
            y: 40.0,
            z: 30.0,
        },
        vy: D3dxVector3 {
            x: -30.0,
            y: 90.0,
            z: 50.0,
        },
        vz: D3dxVector3 {
            x: 20.0,
            y: -25.0,
            z: 120.0,
        },
    };
    let bounds = HorizonMeshBounds::from_box(box_value);

    // A non-degenerate parallelepiped shadow is a quad or a hexagon.
    assert!(
        bounds.vertex_count == 4 || bounds.vertex_count == 6,
        "{}",
        bounds.vertex_count
    );

    let corners = [
        box_value.center + box_value.vx + box_value.vy + box_value.vz,
        box_value.center + box_value.vx + box_value.vy - box_value.vz,
        box_value.center + box_value.vx - box_value.vy + box_value.vz,
        box_value.center + box_value.vx - box_value.vy - box_value.vz,
        box_value.center - box_value.vx + box_value.vy + box_value.vz,
        box_value.center - box_value.vx + box_value.vy - box_value.vz,
        box_value.center - box_value.vx - box_value.vy + box_value.vz,
        box_value.center - box_value.vx - box_value.vy - box_value.vz,
    ];
    let projected: Vec<(f32, f32)> = corners.iter().map(|c| (c.x, c.y)).collect();
    for &vertex in &bounds.footprint_xy[..bounds.vertex_count as usize] {
        assert!(
            matches_any_corner(vertex, &projected),
            "hull vertex {vertex:?} is not a corner"
        );
    }

    let max_z = corners.iter().map(|c| c.z).fold(f32::NEG_INFINITY, f32::max);
    assert!((bounds.max_z - max_z).abs() < 1e-3, "{}", bounds.max_z);
}

#[test]
fn stored_footprint_circle_contains_every_hull_vertex() {
    let bounds = HorizonMeshBounds::from_box(BoundingBox {
        center: D3dxVector3 {
            x: 5000.0,
            y: 1000.0,
            z: 200.0,
        },
        vx: D3dxVector3 {
            x: 100.0,
            y: 40.0,
            z: 30.0,
        },
        vy: D3dxVector3 {
            x: -30.0,
            y: 90.0,
            z: 50.0,
        },
        vz: D3dxVector3 {
            x: 20.0,
            y: -25.0,
            z: 120.0,
        },
    });

    assert!(bounds.vertex_count >= 3);
    assert_circle_contains_footprint(&bounds);
}

#[test]
fn degenerate_box_has_no_footprint_and_is_never_culled() {
    let bounds = HorizonMeshBounds::from_box(BoundingBox::default());
    assert_eq!(bounds.vertex_count, 0);
    assert!(bounds.footprint_circle().is_none());
    assert_eq!(bounds.footprint_radius, 0.0);

    let field = field_from_vertices(&[(4096.0, 0.0, 3000.0)]);
    let table = HorizonTable::build(&field, D3dxVector3::default(), test_params());

    assert!(!horizon_culled_bounds(&bounds, &table));
}

#[test]
fn generated_footprint_translates_local_xy_and_max_z() {
    let footprint = HorizonFootprint {
        max_z: 12.0,
        vertex_count: 3,
        footprint_xy: [[0.0, 0.0], [10.0, 0.0], [0.0, 5.0], [0.0; 2], [0.0; 2], [0.0; 2]],
        ..HorizonFootprint::default()
    };

    let bounds = HorizonMeshBounds::from_generated_footprint(
        &footprint,
        D3dxVector3 {
            x: 100.0,
            y: 200.0,
            z: 300.0,
        },
    )
    .expect("valid footprint");

    assert_eq!(bounds.vertex_count, 3);
    assert_eq!(bounds.max_z, 312.0);
    assert_eq!(bounds.footprint_xy[0], (100.0, 200.0));
    assert_eq!(bounds.footprint_xy[1], (110.0, 200.0));
    assert_eq!(bounds.footprint_xy[2], (100.0, 205.0));
}

#[test]
fn generated_footprint_rejects_invalid_or_degenerate_polygons() {
    let too_few = HorizonFootprint {
        max_z: 12.0,
        vertex_count: 2,
        footprint_xy: [[0.0, 0.0], [10.0, 0.0], [0.0; 2], [0.0; 2], [0.0; 2], [0.0; 2]],
        ..HorizonFootprint::default()
    };
    assert!(HorizonMeshBounds::from_generated_footprint(&too_few, D3dxVector3::default()).is_none());

    let collinear = HorizonFootprint {
        max_z: 12.0,
        vertex_count: 3,
        footprint_xy: [[0.0, 0.0], [1.0, 1.0], [2.0, 2.0], [0.0; 2], [0.0; 2], [0.0; 2]],
        ..HorizonFootprint::default()
    };
    assert!(HorizonMeshBounds::from_generated_footprint(&collinear, D3dxVector3::default()).is_none());

    let non_finite = HorizonFootprint {
        max_z: f32::NAN,
        vertex_count: 3,
        footprint_xy: [[0.0, 0.0], [10.0, 0.0], [0.0, 5.0], [0.0; 2], [0.0; 2], [0.0; 2]],
        ..HorizonFootprint::default()
    };
    assert!(HorizonMeshBounds::from_generated_footprint(&non_finite, D3dxVector3::default()).is_none());
}

#[test]
fn generated_footprint_area_check_is_translation_invariant() {
    // Regression: a small but valid footprint placed far from the world origin must be accepted.
    // Measuring its area on the translated world-space points in f32 suffers catastrophic
    // cancellation (the offset dwarfs the footprint), so the area check runs on the local
    // coordinates, where the area is identical but precise.
    let footprint = HorizonFootprint {
        max_z: 20.0,
        vertex_count: 3,
        footprint_xy: [[0.0, 0.0], [12.0, 0.0], [0.0, 8.0], [0.0; 2], [0.0; 2], [0.0; 2]],
        ..HorizonFootprint::default()
    };
    let translation = D3dxVector3 {
        x: 500_000.0,
        y: -300_000.0,
        z: 40.0,
    };

    // The true (local) area is 48. Measured in far-from-origin world space it is corrupted well
    // beyond a unit. That is the precise failure the local-space area check sidesteps.
    let world = [
        (
            footprint.footprint_xy[0][0] + translation.x,
            footprint.footprint_xy[0][1] + translation.y,
        ),
        (
            footprint.footprint_xy[1][0] + translation.x,
            footprint.footprint_xy[1][1] + translation.y,
        ),
        (
            footprint.footprint_xy[2][0] + translation.x,
            footprint.footprint_xy[2][1] + translation.y,
        ),
    ];
    let world_area = generated_footprint_signed_area(&world);
    assert!(
        (world_area - 48.0).abs() > 1.0,
        "world-space area {world_area} should be corrupted by cancellation"
    );

    let bounds = HorizonMeshBounds::from_generated_footprint(&footprint, translation)
        .expect("valid footprint accepted at large offset");
    assert_eq!(bounds.vertex_count, 3);
    assert_eq!(bounds.footprint_xy[0], (500_000.0, -300_000.0));
    assert_eq!(bounds.footprint_xy[1], (500_012.0, -300_000.0));
    assert_eq!(bounds.footprint_xy[2], (500_000.0, -299_992.0));
    assert!((bounds.max_z - 60.0).abs() < 1e-3, "{}", bounds.max_z);
}

#[test]
fn footprint_circle_is_translation_stable_far_from_origin() {
    let footprint = HorizonFootprint {
        max_z: 20.0,
        vertex_count: 3,
        footprint_xy: [[0.0, 0.0], [12.0, 0.0], [0.0, 8.0], [0.0; 2], [0.0; 2], [0.0; 2]],
        ..HorizonFootprint::default()
    };
    let translation = D3dxVector3 {
        x: 500_000.0,
        y: -300_000.0,
        z: 40.0,
    };

    let bounds = HorizonMeshBounds::from_generated_footprint(&footprint, translation)
        .expect("valid footprint accepted at large offset");
    let Some((center, radius)) = bounds.footprint_circle() else {
        panic!("valid generated footprint should have a circle");
    };
    let expected_radius = (6.0_f32 * 6.0 + 4.0 * 4.0).sqrt();

    assert!((center.0 - 500_006.0).abs() < 1.0, "{center:?}");
    assert!((center.1 + 299_996.0).abs() < 1.0, "{center:?}");
    assert!((radius - expected_radius).abs() < 1.0e-2, "{radius}");
    assert_circle_contains_footprint(&bounds);
}

fn layout_from_vertices(vertices: &[(f32, f32, f32)], origin: [f32; 2], size: [f32; 2]) -> (TerrainFileLayout, Vec<u8>) {
    let mut bytes = Vec::new();
    for &(x, y, z) in vertices {
        let vertex = TerrainVertex {
            position: D3dxVector3 { x, y, z },
            normal: [128, 128, 255, 0],
            color: 0,
        };
        bytes.extend_from_slice(bytemuck::bytes_of(&vertex));
    }
    let header = TerrainFileHeader {
        world_origin: origin,
        world_size: size,
        vertex_stride: SIZE_OF_TERRAIN_VERT,
        mesh_count: 1,
        ..TerrainFileHeader::default()
    };
    let mesh_header = TerrainMeshHeader {
        vertex_count: vertices.len() as u32,
        triangle_count: 0,
        ..TerrainMeshHeader::default()
    };
    let vertex_data_size = bytes.len();
    (
        TerrainFileLayout {
            header,
            meshes: vec![TerrainMeshLayout {
                header: mesh_header,
                vertex_data_offset: 0,
                vertex_data_size,
                index_data_offset: vertex_data_size,
                index_data_size: 0,
            }],
        },
        bytes,
    )
}

fn field_from_vertices(vertices: &[(f32, f32, f32)]) -> TerrainHeightField {
    let (layout, bytes) = layout_from_vertices(vertices, [-8192.0, -8192.0], [16384.0, 16384.0]);
    TerrainHeightField::build_from_layout(&layout, &bytes, 512.0).unwrap()
}

fn land_cell(base: f32) -> [[f32; 65]; 65] {
    let mut heights = [[0.0; 65]; 65];
    for (j, row) in heights.iter_mut().enumerate() {
        for (i, height) in row.iter_mut().enumerate() {
            *height = base + i as f32 * 2.0 + j as f32 * 3.0;
        }
    }
    heights
}

fn layout_from_land_cells(
    cells: &[((i32, i32), &[[f32; 65]; 65])],
    origin_cell: [i32; 2],
    cell_size_xy: [u32; 2],
) -> (TerrainFileLayout, Vec<u8>) {
    let mut vertices = Vec::new();
    for &(grid, heights) in cells {
        for (j, row) in heights.iter().enumerate() {
            let y = grid.1 as f32 * 8192.0 + j as f32 * 128.0;
            for (i, &z) in row.iter().enumerate() {
                let x = grid.0 as f32 * 8192.0 + i as f32 * 128.0;
                vertices.push((x, y, z));
            }
        }
    }

    let origin = [origin_cell[0] as f32 * 8192.0, origin_cell[1] as f32 * 8192.0];
    let size = [cell_size_xy[0] as f32 * 8192.0, cell_size_xy[1] as f32 * 8192.0];
    let (mut layout, bytes) = layout_from_vertices(&vertices, origin, size);
    layout.header.origin_cell = origin_cell;
    layout.header.cell_size_xy = cell_size_xy;
    layout.header.cell_size = 8192.0;
    layout.header.patch_size = 512.0;
    (layout, bytes)
}

fn field_from_generated_occlusion(
    cells: &[((i32, i32), &[[f32; 65]; 65])],
    layout: &TerrainFileLayout,
    origin_cell: [i32; 2],
    cell_size_xy: [u32; 2],
) -> TerrainHeightField {
    let file = generator_occlusion::build_terrain_occlusion(
        cells.iter().map(|(grid, heights)| (*grid, *heights)),
        origin_cell,
        cell_size_xy,
        512.0,
    )
    .unwrap();
    let bytes = generator_occlusion::serialize_terrain_occlusion_file(&file).unwrap();
    let parsed = parse_terrain_occlusion(&bytes).unwrap();
    TerrainHeightField::from_occlusion(parsed, &layout.header).unwrap()
}

fn assert_f32_slices_bitwise_equal(left: &[f32], right: &[f32]) {
    assert_eq!(left.len(), right.len());
    for (index, (&a, &b)) in left.iter().zip(right.iter()).enumerate() {
        assert_eq!(a.to_bits(), b.to_bits(), "float mismatch at index {index}: {a} != {b}");
    }
}

fn assert_height_fields_bitwise_equal(left: &TerrainHeightField, right: &TerrainHeightField) {
    assert_eq!(left.origin.x.to_bits(), right.origin.x.to_bits());
    assert_eq!(left.origin.y.to_bits(), right.origin.y.to_bits());
    assert_eq!(left.size.x.to_bits(), right.size.x.to_bits());
    assert_eq!(left.size.y.to_bits(), right.size.y.to_bits());
    assert_eq!(left.spacing.to_bits(), right.spacing.to_bits());
    assert_eq!((left.nx, left.ny), (right.nx, right.ny));
    assert_eq!(left.covered_cells, right.covered_cells);
    assert_eq!(left.global_max_z.to_bits(), right.global_max_z.to_bits());
    assert_f32_slices_bitwise_equal(&left.max_z, &right.max_z);
    assert_eq!(left.levels.len(), right.levels.len());
    for (level_index, (left_level, right_level)) in left.levels.iter().zip(&right.levels).enumerate() {
        assert_eq!(
            left_level.spacing.to_bits(),
            right_level.spacing.to_bits(),
            "level {level_index}"
        );
        assert_eq!(
            (left_level.nx, left_level.ny),
            (right_level.nx, right_level.ny),
            "level {level_index}"
        );
        assert_f32_slices_bitwise_equal(&left_level.max_z, &right_level.max_z);
    }
}

fn assert_occlusion_matches_layout_for_land_cells(
    cells: &[((i32, i32), &[[f32; 65]; 65])],
    origin_cell: [i32; 2],
    cell_size_xy: [u32; 2],
) {
    let (layout, bytes) = layout_from_land_cells(cells, origin_cell, cell_size_xy);
    let from_layout = TerrainHeightField::build_from_layout(&layout, &bytes, 512.0).unwrap();
    let from_asset = field_from_generated_occlusion(cells, &layout, origin_cell, cell_size_xy);

    assert_height_fields_bitwise_equal(&from_layout, &from_asset);

    let eye = D3dxVector3 {
        x: layout.header.world_origin[0] + layout.header.world_size[0] * 0.5,
        y: layout.header.world_origin[1] + layout.header.world_size[1] * 0.5,
        z: 250.0,
    };
    assert_tables_bitwise_equal(
        &HorizonTable::build(&from_layout, eye, test_params()),
        &HorizonTable::build(&from_asset, eye, test_params()),
    );

    let mut hierarchical_params = test_params();
    hierarchical_params.hierarchical_march = true;
    assert_tables_bitwise_equal(
        &HorizonTable::build(&from_layout, eye, hierarchical_params),
        &HorizonTable::build(&from_asset, eye, hierarchical_params),
    );
}

/// Small deterministic LCG so pyramid/hierarchical-march tests are randomized but reproducible.
struct Lcg(u64);

impl Lcg {
    fn next_u32(&mut self) -> u32 {
        // Same constants as Numerical Recipes' 64-bit LCG; only used for test data generation.
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }

    fn next_f32(&mut self, min: f32, max: f32) -> f32 {
        let t = self.next_u32() as f32 / u32::MAX as f32;
        min + t * (max - min)
    }
}

/// Builds a `TerrainHeightField` directly from a caller-supplied `nx x ny` height grid
/// (row-major, `EMPTY_HEIGHT` for uncovered cells), bypassing vertex scanning so pyramid tests
/// can exercise controlled/random grids.
fn field_with_grid(nx: u32, ny: u32, spacing: f32, heights: &[f32]) -> TerrainHeightField {
    assert_eq!(heights.len(), (nx * ny) as usize);
    let mut field = TerrainHeightField {
        origin: D3dxVector2 { x: 0.0, y: 0.0 },
        size: D3dxVector2 {
            x: (nx.max(1) - 1) as f32 * spacing + 1.0,
            y: (ny.max(1) - 1) as f32 * spacing + 1.0,
        },
        spacing,
        nx,
        ny,
        max_z: heights.to_vec(),
        covered_cells: heights.iter().filter(|&&h| h != EMPTY_HEIGHT).count(),
        global_max_z: heights.iter().copied().fold(EMPTY_HEIGHT, f32::max),
        levels: Vec::new(),
    };
    field.build_pyramid();
    field
}

#[test]
fn occlusion_asset_matches_runtime_field_for_one_cell_land_fixture() {
    let cell = land_cell(10.0);
    let cells = vec![((0, 0), &cell)];

    assert_occlusion_matches_layout_for_land_cells(&cells, [0, 0], [1, 1]);
}

#[test]
fn occlusion_asset_matches_runtime_field_for_two_cell_equal_seam_fixture() {
    let left = land_cell(20.0);
    let mut right = land_cell(200.0);
    for j in 0..65 {
        right[j][0] = left[j][64];
    }
    let cells = vec![((0, 0), &left), ((1, 0), &right)];

    assert_occlusion_matches_layout_for_land_cells(&cells, [0, 0], [2, 1]);
}

#[test]
fn occlusion_asset_rejects_terrain_mismatch_and_nan_heights() {
    let cell = land_cell(30.0);
    let cells = vec![((0, 0), &cell)];
    let (layout, _) = layout_from_land_cells(&cells, [0, 0], [1, 1]);
    let file = generator_occlusion::build_terrain_occlusion(
        cells.iter().map(|(grid, heights)| (*grid, *heights)),
        [0, 0],
        [1, 1],
        512.0,
    )
    .unwrap();
    let bytes = generator_occlusion::serialize_terrain_occlusion_file(&file).unwrap();

    let mut mismatched_layout = layout.clone();
    mismatched_layout.header.origin_cell = [1, 0];
    let parsed = parse_terrain_occlusion(&bytes).unwrap();
    assert_eq!(
        TerrainHeightField::from_occlusion(parsed, &mismatched_layout.header).unwrap_err(),
        OcclusionFormatError::TerrainMismatch("origin_cell")
    );

    let mut nan_base = parse_terrain_occlusion(&bytes).unwrap();
    nan_base.max_z[0] = f32::NAN;
    assert_eq!(
        TerrainHeightField::from_occlusion(nan_base, &layout.header).unwrap_err(),
        OcclusionFormatError::NonFiniteHeight { index: 0 }
    );
}

#[test]
fn occlusion_asset_accepts_sentinel_only_file() {
    let cells: Vec<((i32, i32), &[[f32; 65]; 65])> = Vec::new();
    let (layout, _) = layout_from_land_cells(&cells, [0, 0], [1, 1]);
    let file = generator_occlusion::build_terrain_occlusion(
        cells.iter().map(|(grid, heights)| (*grid, *heights)),
        [0, 0],
        [1, 1],
        512.0,
    )
    .unwrap();
    let bytes = generator_occlusion::serialize_terrain_occlusion_file(&file).unwrap();
    let parsed = parse_terrain_occlusion(&bytes).unwrap();

    let field = TerrainHeightField::from_occlusion(parsed, &layout.header).unwrap();

    assert_eq!(field.covered_cell_count(), 0);
    assert_eq!(field.global_max_z(), None);
    assert!(field.max_z.iter().all(|&height| height == EMPTY_HEIGHT));
    for level in &field.levels {
        assert!(level.max_z.iter().all(|&height| height == EMPTY_HEIGHT));
    }
}

#[test]
fn mip_level_parent_is_ge_every_present_child_and_empty_iff_all_children_empty() {
    let mut rng = Lcg(0xC0FFEE);
    for _ in 0..20 {
        let nx = 1 + (rng.next_u32() % 40);
        let ny = 1 + (rng.next_u32() % 40);
        let heights: Vec<f32> = (0..(nx * ny))
            .map(|_| {
                if rng.next_u32().is_multiple_of(5) {
                    EMPTY_HEIGHT
                } else {
                    rng.next_f32(-1000.0, 5000.0)
                }
            })
            .collect();
        let field = field_with_grid(nx, ny, 512.0, &heights);

        let mut prev_nx = nx;
        let mut prev_ny = ny;
        let mut prev: &[f32] = &field.max_z;
        for level in &field.levels {
            for y in 0..level.ny {
                for x in 0..level.nx {
                    let parent = level.max_z[(y * level.nx + x) as usize];
                    let mut any_present = false;
                    for dy in 0..2u32 {
                        for dx in 0..2u32 {
                            let cx = x * 2 + dx;
                            let cy = y * 2 + dy;
                            if cx < prev_nx && cy < prev_ny {
                                let child = prev[(cy * prev_nx + cx) as usize];
                                if child != EMPTY_HEIGHT {
                                    any_present = true;
                                    assert!(parent >= child, "parent {parent} < child {child}");
                                }
                            }
                        }
                    }
                    assert_eq!(
                        parent == EMPTY_HEIGHT,
                        !any_present,
                        "empty mismatch at level nx={} ny={} ({x},{y})",
                        level.nx,
                        level.ny
                    );
                }
            }
            prev_nx = level.nx;
            prev_ny = level.ny;
            prev = &level.max_z;
        }
    }
}

#[test]
fn mip_levels_reduce_odd_and_even_dimensions_to_one_by_one() {
    let field = field_with_grid(5, 3, 512.0, &[1.0; 15]);
    let dims: Vec<(u32, u32)> = field.levels.iter().map(|level| (level.nx, level.ny)).collect();
    assert_eq!(dims, vec![(3, 2), (2, 1), (1, 1)]);
}

#[test]
fn mip_levels_of_an_already_one_by_one_base_grid_are_empty() {
    let field = field_with_grid(1, 1, 512.0, &[42.0]);
    assert!(field.levels.is_empty());
    assert_eq!(field.mip_level_count(), 0);
}

#[test]
fn max_over_aabb_matches_base_cells_covered_by_the_selected_level_including_out_of_bounds() {
    let mut rng = Lcg(0xABCDEF);
    let nx = 17u32;
    let ny = 13u32;
    let spacing = 512.0f32;
    let heights: Vec<f32> = (0..(nx * ny))
        .map(|_| {
            if rng.next_u32().is_multiple_of(6) {
                EMPTY_HEIGHT
            } else {
                rng.next_f32(-500.0, 3000.0)
            }
        })
        .collect();
    let field = field_with_grid(nx, ny, spacing, &heights);
    let world_w = (nx - 1) as f32 * spacing;
    let world_h = (ny - 1) as f32 * spacing;

    for _ in 0..40 {
        // Random AABBs, some fully/partially outside the field rect.
        let min_x = rng.next_f32(-spacing * 3.0, world_w + spacing);
        let max_x = min_x + rng.next_f32(0.0, world_w * 0.5 + spacing);
        let min_y = rng.next_f32(-spacing * 3.0, world_h + spacing);
        let max_y = min_y + rng.next_f32(0.0, world_h * 0.5 + spacing);

        for level in 0..=field.levels.len() {
            let (level_spacing, level_nx, level_ny) = if level == 0 {
                (spacing, nx, ny)
            } else {
                let l = &field.levels[level - 1];
                (l.spacing, l.nx, l.ny)
            };
            let factor = 1u32 << level;

            let clamped_min_x = min_x.max(field.origin.x);
            let clamped_min_y = min_y.max(field.origin.y);
            let clamped_max_x = max_x.min(field.origin.x + field.size.x);
            let clamped_max_y = max_y.min(field.origin.y + field.size.y);

            let expected = if clamped_min_x > clamped_max_x || clamped_min_y > clamped_max_y {
                EMPTY_HEIGHT
            } else {
                let ix0 = ((clamped_min_x - field.origin.x) / level_spacing)
                    .floor()
                    .clamp(0.0, (level_nx - 1) as f32) as u32;
                let iy0 = ((clamped_min_y - field.origin.y) / level_spacing)
                    .floor()
                    .clamp(0.0, (level_ny - 1) as f32) as u32;
                let ix1 = ((clamped_max_x - field.origin.x) / level_spacing)
                    .floor()
                    .clamp(0.0, (level_nx - 1) as f32) as u32;
                let iy1 = ((clamped_max_y - field.origin.y) / level_spacing)
                    .floor()
                    .clamp(0.0, (level_ny - 1) as f32) as u32;

                // Expand the level-cell index range back to base cells and take the true max
                // straight from the base grid. This is independent of the pyramid's own aggregation.
                let mut expected = EMPTY_HEIGHT;
                for iy in (iy0 * factor)..((iy1 + 1) * factor).min(ny) {
                    for ix in (ix0 * factor)..((ix1 + 1) * factor).min(nx) {
                        let h = field.max_z[(iy * nx + ix) as usize];
                        if h != EMPTY_HEIGHT {
                            expected = expected.max(h);
                        }
                    }
                }
                expected
            };

            let actual = field.max_over_aabb(level, min_x, min_y, max_x, max_y);
            assert_eq!(actual, expected, "level={level} aabb=({min_x},{min_y})-({max_x},{max_y})");
        }
    }
}

#[test]
fn clone_preserves_mip_levels() {
    let field = field_from_vertices(&[(1024.0, 0.0, 5.0), (4096.0, 0.0, 9.0), (8192.0, 0.0, 3.0)]);
    let cloned = field.clone();

    assert_eq!(cloned.mip_level_count(), field.mip_level_count());
    assert_eq!(cloned.levels.len(), field.levels.len());
    for (original_level, cloned_level) in field.levels.iter().zip(cloned.levels.iter()) {
        assert_eq!(original_level.nx, cloned_level.nx);
        assert_eq!(original_level.ny, cloned_level.ny);
        assert_eq!(original_level.spacing, cloned_level.spacing);
        assert_eq!(original_level.max_z, cloned_level.max_z);
    }
}

#[test]
fn height_field_builds_per_cell_max_and_reports_empty_queries() {
    let (layout, bytes) = layout_from_vertices(&[(10.0, 10.0, 5.0), (20.0, 20.0, 7.0)], [0.0, 0.0], [1024.0, 1024.0]);
    let field = TerrainHeightField::build_from_layout(&layout, &bytes, 512.0).unwrap();

    assert_eq!(field.covered_cell_count(), 1);
    assert_eq!(field.sample_max_z(10.0, 10.0), Some(7.0));
    assert_eq!(field.sample_max_z(-1.0, 10.0), None);
    assert_eq!(field.sample_max_z(900.0, 900.0), None);
}

#[test]
fn flat_plane_does_not_cull_object_above_plane() {
    let field = field_from_vertices(&[(1024.0, 0.0, 0.0), (4096.0, 0.0, 0.0), (8192.0, 0.0, 0.0)]);
    let eye = D3dxVector3 {
        x: 0.0,
        y: 0.0,
        z: 100.0,
    };
    let table = HorizonTable::build(&field, eye, test_params());

    assert!(!horizon_culled(sphere(4096.0, 0.0, 0.0, 10.0), &table));
}

#[test]
fn ridge_culls_low_object_beyond_but_not_before_it() {
    let field = field_from_vertices(&[(4096.0, 0.0, 2000.0)]);
    let eye = D3dxVector3::default();
    let table = HorizonTable::build(&field, eye, test_params());

    assert!(horizon_culled(sphere(8192.0, 0.0, 0.0, 50.0), &table));
    assert!(!horizon_culled(sphere(2500.0, 0.0, 0.0, 50.0), &table));
}

#[test]
fn terrain_behind_object_does_not_cull_it() {
    let field = field_from_vertices(&[(4096.0, 0.0, 3000.0)]);
    let eye = D3dxVector3::default();
    let table = HorizonTable::build(&field, eye, test_params());

    assert!(!horizon_culled(sphere(2048.0, 0.0, 0.0, 50.0), &table));
}

#[test]
fn min_over_span_keeps_object_visible_through_gap() {
    let table = HorizonTable {
        eye: D3dxVector3::default(),
        bin_count: 8,
        ring_count: 2,
        ring_step: 1024.0,
        r_near: 0.0,
        bias_obj_z: 0.0,
        bias_z: 0.0,
        max_slope: vec![
            EMPTY_SLOPE,
            EMPTY_SLOPE,
            1.0,
            1.0,
            1.0,
            1.0,
            1.0,
            1.0,
            1.0,
            1.0,
            1.0,
            1.0,
            1.0,
            1.0,
            1.0,
            1.0,
        ],
    };

    assert!(!horizon_culled(sphere(2048.0, 0.0, 0.0, 500.0), &table));
}

#[test]
fn near_object_without_complete_nearer_ring_is_never_culled() {
    let table = HorizonTable {
        eye: D3dxVector3::default(),
        bin_count: 8,
        ring_count: 2,
        ring_step: 1024.0,
        r_near: 0.0,
        bias_obj_z: 0.0,
        bias_z: 0.0,
        max_slope: vec![10.0; 16],
    };

    assert!(!horizon_culled(sphere(900.0, 0.0, 0.0, 50.0), &table));
}

#[test]
fn biases_only_shrink_the_culled_set() {
    let field = field_from_vertices(&[(4096.0, 0.0, 2000.0)]);
    let eye = D3dxVector3::default();
    let object = sphere(8192.0, 0.0, 0.0, 50.0);
    let low_bias = HorizonTable::build(&field, eye, test_params());
    let high_terrain_bias = HorizonTable::build(
        &field,
        eye,
        HorizonParams {
            bias_z: 5000.0,
            ..test_params()
        },
    );
    let high_object_bias = HorizonTable::build(
        &field,
        eye,
        HorizonParams {
            bias_obj_z: 5000.0,
            ..test_params()
        },
    );

    assert!(horizon_culled(object, &low_bias));
    assert!(!horizon_culled(object, &high_terrain_bias));
    assert!(!horizon_culled(object, &high_object_bias));
}

#[test]
fn object_top_above_global_max_height_is_never_culled() {
    let field = field_from_vertices(&[(4096.0, 0.0, 100.0)]);
    let eye = D3dxVector3::default();
    let table = HorizonTable::build(&field, eye, test_params());

    assert_eq!(field.global_max_z(), Some(100.0));
    assert!(!horizon_culled(sphere(8192.0, 0.0, 200.0, 10.0), &table));
}

#[test]
fn capped_height_culls_wide_low_merge_that_sphere_misses() {
    // A wide, low merge: the huge horizontal radius lifts the sphere apex far above the
    // horizon, so the plain sphere test cannot cull it, but its true top sits well below.
    let table = HorizonTable {
        eye: D3dxVector3::default(),
        bin_count: 8,
        ring_count: 2,
        ring_step: 1024.0,
        r_near: 0.0,
        bias_obj_z: 0.0,
        bias_z: 0.0,
        max_slope: vec![0.4; 16],
    };
    let blob = sphere(4000.0, 0.0, 0.0, 2000.0);

    // Apex slope = 2000 / (4000 - 2000) = 1.0 > 0.4 horizon -> sphere test keeps it.
    assert!(!horizon_culled(blob, &table));
    // True top 100 -> slope 100 / 2000 = 0.05 < 0.4 -> culled on the cheap path.
    assert!(horizon_culled_capped(blob, 100.0, &table));
    // A genuinely tall merge (top above the horizon) is still kept visible.
    assert!(!horizon_culled_capped(blob, 1000.0, &table));
}

#[test]
fn footprint_circle_culls_tall_object_that_sphere_disc_misses() {
    let table = HorizonTable {
        eye: D3dxVector3::default(),
        bin_count: 8,
        ring_count: 8,
        ring_step: 512.0,
        r_near: 0.0,
        bias_obj_z: 0.0,
        bias_z: 0.0,
        max_slope: vec![0.2; 64],
    };
    let inflated_sphere = sphere(4000.0, 0.0, -1900.0, 3900.0);
    let bounds = HorizonMeshBounds::from_box(aabb((3990.0, -10.0, -3900.0), (4010.0, 10.0, 100.0)));
    let Some((center, radius)) = bounds.footprint_circle() else {
        panic!("valid tall object footprint should have a circle");
    };

    assert!(radius < 15.0, "{radius}");
    assert!(!horizon_culled_capped(inflated_sphere, bounds.max_z, &table));
    assert!(horizon_culled_capped_xy(center, radius, bounds.max_z, &table));
}

#[test]
fn capped_below_eye_top_uses_far_distance_not_near() {
    // Regression: a below-eye top must be evaluated at the FARTHEST point of the disc footprint
    // (its highest elevation angle), not the nearest. Dividing the negative delta by `d_near`
    // over-states how far the top dips below the horizon and culls geometry that is actually
    // visible. Mirror of the sign-aware top-distance choice in `horizon_culled_bounds`.
    let table = HorizonTable {
        eye: D3dxVector3 {
            x: 0.0,
            y: 0.0,
            z: 100.0,
        },
        bin_count: 8,
        ring_count: 2,
        ring_step: 50.0,
        r_near: 0.0,
        bias_obj_z: 0.0,
        bias_z: 0.0,
        max_slope: vec![-0.15; 16],
    };
    // horizontal_distance=200, radius=100 -> d_near=100, d_far=300, ring=1.
    let blob = sphere(200.0, 0.0, 0.0, 100.0);

    // Top 80, eye 100 -> delta -20. Far slope -20/300 = -0.067 > -0.15 horizon -> visible.
    // The buggy `d_near` slope -20/100 = -0.20 < -0.15 would have wrongly culled it.
    assert!(!horizon_culled_capped(blob, 80.0, &table));
    // A genuinely lower top stays culled even at the farthest distance: -80/300 = -0.267 < -0.15.
    assert!(horizon_culled_capped(blob, 20.0, &table));
}

#[test]
fn capped_with_sphere_apex_matches_plain_sphere_test() {
    // Feeding the sphere apex as max_z must reproduce the plain sphere test exactly, so the
    // capped variant is a strict generalization rather than a behavior change.
    let field = field_from_vertices(&[(4096.0, 0.0, 2000.0)]);
    let table = HorizonTable::build(&field, D3dxVector3::default(), test_params());
    for blob in [
        sphere(8192.0, 0.0, 0.0, 50.0),
        sphere(2500.0, 0.0, 0.0, 50.0),
        sphere(6000.0, 1000.0, 100.0, 800.0),
    ] {
        let apex = blob.center.z + blob.radius;
        assert_eq!(horizon_culled(blob, &table), horizon_culled_capped(blob, apex, &table));
    }
}

#[test]
fn accept_skips_obb_for_object_clearly_above_the_horizon() {
    let field = field_from_vertices(&[(4096.0, 0.0, 2000.0)]);
    let table = HorizonTable::build(&field, D3dxVector3::default(), test_params());

    // Beyond the ridge but towering above it: provably visible, so the accept fires.
    let tall = sphere(8192.0, 0.0, 6000.0, 50.0);
    assert!(horizon_visible_capped(tall, 6000.0, &table));
    // Subset guarantee: the precise hull test keeps it too, so the accept never skips a real cull.
    let tall_bounds = HorizonMeshBounds::from_box(aabb((8142.0, -50.0, 5950.0), (8242.0, 50.0, 6050.0)));
    assert!(!horizon_culled_bounds(&tall_bounds, &table));
}

#[test]
fn accept_defers_hidden_object_to_the_obb_fallback() {
    let field = field_from_vertices(&[(4096.0, 0.0, 2000.0)]);
    let table = HorizonTable::build(&field, D3dxVector3::default(), test_params());

    // Low and behind the ridge: hidden. The accept must NOT fire, or we would skip a real cull.
    let low = sphere(8192.0, 0.0, 0.0, 50.0);
    assert!(!horizon_visible_capped(low, 0.0, &table));
    let low_bounds = HorizonMeshBounds::from_box(aabb((8142.0, -50.0, -50.0), (8242.0, 50.0, 50.0)));
    assert!(horizon_culled_bounds(&low_bounds, &table));
}

#[test]
fn ridge_culls_low_box_beyond_it_but_not_before_it() {
    let field = field_from_vertices(&[(4096.0, 0.0, 2000.0)]);
    let table = HorizonTable::build(&field, D3dxVector3::default(), test_params());

    let beyond = HorizonMeshBounds::from_box(aabb((8142.0, -50.0, -50.0), (8242.0, 50.0, 50.0)));
    let before = HorizonMeshBounds::from_box(aabb((2450.0, -50.0, -50.0), (2550.0, 50.0, 50.0)));

    assert!(horizon_culled_bounds(&beyond, &table));
    assert!(!horizon_culled_bounds(&before, &table));
}

#[test]
fn terrain_behind_box_does_not_cull_it() {
    let field = field_from_vertices(&[(4096.0, 0.0, 3000.0)]);
    let table = HorizonTable::build(&field, D3dxVector3::default(), test_params());

    let near = HorizonMeshBounds::from_box(aabb((1998.0, -50.0, -50.0), (2098.0, 50.0, 50.0)));

    assert!(!horizon_culled_bounds(&near, &table));
}

#[test]
fn box_top_above_terrain_is_never_culled() {
    let field = field_from_vertices(&[(4096.0, 0.0, 100.0)]);
    let table = HorizonTable::build(&field, D3dxVector3::default(), test_params());

    // Beyond the ridge but towering above all terrain, so its top clears the horizon.
    let tall = HorizonMeshBounds::from_box(aabb((8142.0, -50.0, 150.0), (8242.0, 50.0, 250.0)));

    assert!(!horizon_culled_bounds(&tall, &table));
}

#[test]
fn box_surrounding_the_eye_is_never_culled() {
    let field = field_from_vertices(&[(4096.0, 0.0, 5000.0)]);
    let table = HorizonTable::build(&field, D3dxVector3::default(), test_params());

    // A box straddling the eye spans the full circle, so the angular-span guard keeps it
    // visible even though a tall ridge would otherwise occlude its low top.
    let around = HorizonMeshBounds::from_box(aabb((-5000.0, -5000.0, -50.0), (5000.0, 5000.0, 50.0)));

    assert!(!horizon_culled_bounds(&around, &table));
}

#[test]
fn test_horizon_culled_rect_contract() {
    let field = field_from_vertices(&[(4096.0, 0.0, 2000.0)]);
    let table = HorizonTable::build(&field, D3dxVector3::default(), test_params());

    let min_xy = D3dxVector2 { x: 8000.0, y: -100.0 };
    let max_xy = D3dxVector2 { x: 8100.0, y: 100.0 };

    // 1. Valid far rect below synthetic horizon -> culls
    assert!(horizon_culled_rect(min_xy, max_xy, 50.0, &table));

    // 2. max_z = NEG_INFINITY / NaN / +infinity -> fail open
    assert!(!horizon_culled_rect(min_xy, max_xy, f32::NEG_INFINITY, &table));
    assert!(!horizon_culled_rect(min_xy, max_xy, f32::NAN, &table));
    assert!(!horizon_culled_rect(min_xy, max_xy, f32::INFINITY, &table));

    // 3. Inverted box -> fail open
    let inverted_x = D3dxVector2 { x: 8100.0, y: -100.0 };
    let inverted_x_max = D3dxVector2 { x: 8000.0, y: 100.0 };
    assert!(!horizon_culled_rect(inverted_x, inverted_x_max, 50.0, &table));

    // 4. Zero-width, zero-height, and point boxes -> fail open
    let zero_w_min = D3dxVector2 { x: 8000.0, y: -100.0 };
    let zero_w_max = D3dxVector2 { x: 8000.0, y: 100.0 };
    assert!(!horizon_culled_rect(zero_w_min, zero_w_max, 50.0, &table));

    let zero_h_min = D3dxVector2 { x: 8000.0, y: -100.0 };
    let zero_h_max = D3dxVector2 { x: 8100.0, y: -100.0 };
    assert!(!horizon_culled_rect(zero_h_min, zero_h_max, 50.0, &table));

    let point_min = D3dxVector2 { x: 8000.0, y: 0.0 };
    let point_max = D3dxVector2 { x: 8000.0, y: 0.0 };
    assert!(!horizon_culled_rect(point_min, point_max, 50.0, &table));

    // 5. Eye collinear with a rect edge -> fail open (no panic, no cull)
    // Eye is at (0, 0, 0)
    let collinear_min = D3dxVector2 { x: -100.0, y: 0.0 };
    let collinear_max = D3dxVector2 { x: 100.0, y: 100.0 };
    assert!(!horizon_culled_rect(collinear_min, collinear_max, 50.0, &table));

    // 6. Eye inside -> fail open
    let inside_min = D3dxVector2 { x: -100.0, y: -100.0 };
    let inside_max = D3dxVector2 { x: 100.0, y: 100.0 };
    assert!(!horizon_culled_rect(inside_min, inside_max, 50.0, &table));
}

fn assert_tables_bitwise_equal(linear: &HorizonTable, hierarchical: &HorizonTable) {
    assert_eq!(linear.max_slope.len(), hierarchical.max_slope.len());
    for (i, (&a, &b)) in linear.max_slope.iter().zip(hierarchical.max_slope.iter()).enumerate() {
        assert_eq!(
            a.to_bits(),
            b.to_bits(),
            "slope mismatch at index {i}: linear={a} hierarchical={b}"
        );
    }
}

/// Builds both the linear and hierarchical tables for the same `(field, eye, params)` and
/// asserts their `max_slope` vectors are bit-identical. This is the safety contract in
/// `docs/architecture/horizon-culling.md` §5.2/§9.5. `params.hierarchical_march` is ignored (both variants are
/// forced explicitly).
fn assert_hierarchical_matches_linear(field: &TerrainHeightField, eye: D3dxVector3, params: HorizonParams) {
    let linear = HorizonTable::build(
        field,
        eye,
        HorizonParams {
            hierarchical_march: false,
            ..params
        },
    );
    let hierarchical = HorizonTable::build(
        field,
        eye,
        HorizonParams {
            hierarchical_march: true,
            ..params
        },
    );
    assert_tables_bitwise_equal(&linear, &hierarchical);
}

#[test]
fn hierarchical_builder_matches_linear_on_existing_ridge_and_bias_fixtures() {
    let flat = field_from_vertices(&[(1024.0, 0.0, 0.0), (4096.0, 0.0, 0.0), (8192.0, 0.0, 0.0)]);
    let ridge = field_from_vertices(&[(4096.0, 0.0, 2000.0)]);
    let terrain_behind = field_from_vertices(&[(4096.0, 0.0, 3000.0)]);
    let low_global_max = field_from_vertices(&[(4096.0, 0.0, 100.0)]);
    let tall_ridge = field_from_vertices(&[(4096.0, 0.0, 5000.0)]);
    let eye_high = D3dxVector3 {
        x: 0.0,
        y: 0.0,
        z: 100.0,
    };
    let eye_origin = D3dxVector3::default();

    let cases: [(&TerrainHeightField, D3dxVector3, HorizonParams); 7] = [
        (&flat, eye_high, test_params()),
        (&ridge, eye_origin, test_params()),
        (&terrain_behind, eye_origin, test_params()),
        (
            &ridge,
            eye_origin,
            HorizonParams {
                bias_z: 5000.0,
                ..test_params()
            },
        ),
        (
            &ridge,
            eye_origin,
            HorizonParams {
                bias_obj_z: 5000.0,
                ..test_params()
            },
        ),
        (&low_global_max, eye_origin, test_params()),
        (&tall_ridge, eye_origin, test_params()),
    ];

    for (field, eye, params) in cases {
        assert_hierarchical_matches_linear(field, eye, params);
    }
}

#[test]
fn hierarchical_builder_matches_linear_on_randomized_terrain_eyes_and_params() {
    let mut rng = Lcg(0x1234_5678_9abc_def0);
    for _ in 0..6 {
        let nx = 40 + (rng.next_u32() % 40);
        let ny = 40 + (rng.next_u32() % 40);
        let spacing = 512.0;
        let heights: Vec<f32> = (0..(nx * ny))
            .map(|_| {
                if rng.next_u32().is_multiple_of(4) {
                    EMPTY_HEIGHT
                } else {
                    rng.next_f32(-2000.0, 6000.0)
                }
            })
            .collect();
        let field = field_with_grid(nx, ny, spacing, &heights);
        let world_w = (nx - 1) as f32 * spacing;
        let world_h = (ny - 1) as f32 * spacing;
        let center_x = field.origin.x + world_w * 0.5;
        let center_y = field.origin.y + world_h * 0.5;

        let eyes = [
            D3dxVector3 {
                x: center_x,
                y: center_y,
                z: 100.0,
            },
            D3dxVector3 {
                x: field.origin.x + 10.0,
                y: field.origin.y + 10.0,
                z: 50.0,
            },
            D3dxVector3 {
                x: center_x,
                y: center_y,
                z: -5000.0,
            },
            D3dxVector3 {
                x: center_x,
                y: center_y,
                z: 8000.0,
            },
        ];

        // Covers r_near = 0 and > 0, march_step << and >> ring_step, ring_count = 1 and
        // MAX_HORIZON_RINGS, and n = 0 (r_near > r_max).
        let param_variants = [
            HorizonParams {
                bin_count: 32,
                ring_count: 4,
                ring_step: 1024.0,
                r_near: 0.0,
                bias_z: 0.0,
                bias_obj_z: 0.0,
                march_step: 700.0,
                hierarchical_march: false,
            },
            HorizonParams {
                bin_count: 64,
                ring_count: 8,
                ring_step: 512.0,
                r_near: 900.0,
                bias_z: 128.0,
                bias_obj_z: 64.0,
                march_step: 128.0,
                hierarchical_march: false,
            },
            HorizonParams {
                bin_count: 64,
                ring_count: MAX_HORIZON_RINGS,
                ring_step: 256.0,
                r_near: 0.0,
                bias_z: 0.0,
                bias_obj_z: 0.0,
                march_step: 4096.0,
                hierarchical_march: false,
            },
            HorizonParams {
                bin_count: 64,
                ring_count: 1,
                ring_step: 8192.0,
                r_near: 0.0,
                bias_z: 0.0,
                bias_obj_z: 0.0,
                march_step: 512.0,
                hierarchical_march: false,
            },
            HorizonParams {
                bin_count: 64,
                ring_count: 8,
                ring_step: 512.0,
                r_near: 1_000_000.0,
                bias_z: 0.0,
                bias_obj_z: 0.0,
                march_step: 512.0,
                hierarchical_march: false,
            },
        ];

        for eye in eyes {
            for params in param_variants {
                assert_hierarchical_matches_linear(&field, eye, params);
            }
        }
    }
}

#[test]
fn hierarchical_builder_skips_most_of_the_march_on_flat_far_field_with_one_near_ridge() {
    // One dominant near ridge, otherwise no terrain data: once the ridge sets `running`, no
    // farther segment (all EMPTY beyond the covered cell) can raise it, so the hierarchical
    // march should skip the large majority of the linear-equivalent sample count.
    let field = field_from_vertices(&[(1024.0, 0.0, 4000.0)]);
    let eye = D3dxVector3::default();
    let params = HorizonParams {
        bin_count: 512,
        ring_count: 32,
        ring_step: 1536.0,
        r_near: 0.0,
        bias_z: 0.0,
        bias_obj_z: 0.0,
        march_step: 128.0,
        hierarchical_march: true,
    };
    let (_, stats) = HorizonTable::build_with_stats(&field, eye, params);
    let linear_total = params.samples_per_bin() as u64 * params.bin_count as u64;

    assert!(
        (stats.leaf_samples as f64) < 0.3 * linear_total as f64,
        "leaf_samples={} linear_total={linear_total}",
        stats.leaf_samples
    );
    assert!(stats.segments_skipped > 0);
}
