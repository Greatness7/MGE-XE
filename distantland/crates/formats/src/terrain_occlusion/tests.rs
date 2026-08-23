use super::*;

fn minimal_occlusion_file() -> TerrainOcclusionFile {
    TerrainOcclusionFile {
        origin_cell: [1, -2],
        cell_size_xy: [1, 1],
        world_origin: [8192.0, -16384.0],
        world_size: [8192.0, 8192.0],
        base_spacing: 8192.0,
        base_nx: 2,
        base_ny: 2,
        max_z: vec![1.0, EMPTY_OCCLUSION_HEIGHT, 3.0, 4.0],
    }
}

fn minimal_occlusion_file_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"XEOCCL02");
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&1i32.to_le_bytes());
    bytes.extend_from_slice(&(-2i32).to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&8192.0f32.to_le_bytes());
    bytes.extend_from_slice(&(-16384.0f32).to_le_bytes());
    bytes.extend_from_slice(&8192.0f32.to_le_bytes());
    bytes.extend_from_slice(&8192.0f32.to_le_bytes());
    bytes.extend_from_slice(&8192.0f32.to_le_bytes());
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&1.0f32.to_le_bytes());
    bytes.extend_from_slice(&EMPTY_OCCLUSION_HEIGHT.to_le_bytes());
    bytes.extend_from_slice(&3.0f32.to_le_bytes());
    bytes.extend_from_slice(&4.0f32.to_le_bytes());
    bytes
}

fn height_grid(value: f32) -> Box<[[f32; 65]; 65]> {
    Box::new([[value; 65]; 65])
}

fn index(nx: u32, x: u32, y: u32) -> usize {
    (y as usize) * (nx as usize) + (x as usize)
}

#[test]
fn terrain_occlusion_file_matches_fixture_bytes() {
    let file = minimal_occlusion_file();

    let serialized = serialize_terrain_occlusion_file(&file).unwrap();

    // The hand-built fixture is the conformance pin: mgeHost64 parses these bytes with its own
    // C++-mirroring reader, so agreeing with a fixture beats agreeing with a Rust reader.
    assert_eq!(serialized, minimal_occlusion_file_bytes());
}

#[test]
fn builder_bins_single_cell_and_clamps_region_far_edge() {
    let mut cell = height_grid(-100.0);
    cell[0][0] = 10.0;
    cell[0][4] = 20.0;
    cell[64][64] = 30.0;

    let file = build_terrain_occlusion([((0, 0), cell.as_ref())], [0, 0], [1, 1], 512.0).unwrap();
    let base = &file;

    assert_eq!(file.world_origin, [0.0, 0.0]);
    assert_eq!(file.world_size, [8192.0, 8192.0]);
    assert_eq!((base.base_nx, base.base_ny), (17, 17));
    assert_eq!(base.max_z[index(base.base_nx, 0, 0)], 10.0);
    assert_eq!(base.max_z[index(base.base_nx, 1, 0)], 20.0);
    assert_eq!(base.max_z[index(base.base_nx, 16, 16)], 30.0);
}

#[test]
fn builder_preserves_float_binning_for_custom_spacing() {
    let mut cell = height_grid(1.0);
    cell[0][7] = 20.0;
    cell[0][8] = 30.0;

    let file = build_terrain_occlusion([((0, 0), cell.as_ref())], [0, 0], [1, 1], 300.0).unwrap();
    let base = &file;
    let bin_7 = ((7.0 * LAND_VERTEX_SPACING) / 300.0).floor() as u32;
    let bin_8 = ((8.0 * LAND_VERTEX_SPACING) / 300.0).floor() as u32;

    assert_eq!(base.max_z[index(base.base_nx, bin_7, 0)], 20.0);
    assert_eq!(base.max_z[index(base.base_nx, bin_8, 0)], 30.0);
}

#[test]
fn builder_leaves_missing_cells_empty() {
    let cell = height_grid(5.0);

    let file = build_terrain_occlusion([((0, 0), cell.as_ref())], [0, 0], [2, 1], 512.0).unwrap();
    let base = &file;

    assert_eq!((base.base_nx, base.base_ny), (33, 17));
    assert_eq!(base.max_z[index(base.base_nx, 0, 0)], 5.0);
    assert_eq!(base.max_z[index(base.base_nx, 20, 0)], EMPTY_OCCLUSION_HEIGHT);
}

#[test]
fn adjacent_cells_keep_maximum_on_shared_border() {
    let mut left = height_grid(-10.0);
    let mut right = height_grid(-20.0);
    left[0][64] = 5.0;
    right[0][0] = 50.0;

    let file = build_terrain_occlusion([((0, 0), left.as_ref()), ((1, 0), right.as_ref())], [0, 0], [2, 1], 512.0).unwrap();
    let base = &file;

    assert_eq!(base.max_z[index(base.base_nx, 16, 0)], 50.0);
}

#[test]
fn default_style_uniform_cell_covers_every_base_sample() {
    let cell = height_grid(42.0);

    let file = build_terrain_occlusion([((3, -1), cell.as_ref())], [3, -1], [1, 1], 512.0).unwrap();
    let base = &file;

    assert!(base.max_z.iter().all(|&height| height == 42.0));
}
