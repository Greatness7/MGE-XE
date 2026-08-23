use std::mem::{offset_of, size_of};

use super::*;

fn minimal_terrain_file() -> TerrainFile {
    TerrainFile {
        cell_size: 8192.0,
        patch_size: 512.0,
        origin_cell: [-40, 12],
        cell_size_xy: [2, 1],
        world_origin: Vec2::new(-327680.0, 98304.0),
        world_size: Vec2::new(16384.0, 8192.0),
        atlas_size: 4096,
        logical_tile_size: 256,
        gutter_size: 16,
        physical_tile_size: 288,
        tiles_per_row: 14,
        atlas_max_lod: 2,
        material_size_xy: [32, 16],
        pattern_count: 11,
        pattern_tile_size: 32,
        pattern_gutter_size: 2,
        pattern_physical_size: 36,
        patterns_per_row: 4,
        meshes: vec![TerrainMesh {
            bounding_sphere: BoundingSphere {
                radius: 100.0,
                center: Vec3::new(1.0, 2.0, 3.0),
            },
            bounding_box: BoundingBox {
                min: Vec3::new(-10.0, -20.0, -30.0),
                max: Vec3::new(10.0, 20.0, 30.0),
            },
            vertices: vec![
                TerrainVertex {
                    position: Vec3::new(0.0, 0.0, 0.0),
                    normal: [128, 128, 255, 255],
                    color: [0, 0, 255, 255],
                },
                TerrainVertex {
                    position: Vec3::new(1.0, 0.0, 0.0),
                    normal: [255, 128, 128, 255],
                    color: [0, 255, 0, 255],
                },
                TerrainVertex {
                    position: Vec3::new(0.0, 1.0, 0.0),
                    normal: [128, 255, 128, 255],
                    color: [255, 0, 0, 255],
                },
            ],
            triangles: vec![[0, 1, 2]],
        }],
    }
}

fn minimal_terrain_file_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();

    bytes.extend_from_slice(b"XELAND02");
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&8192.0f32.to_le_bytes());
    bytes.extend_from_slice(&512.0f32.to_le_bytes());
    bytes.extend_from_slice(&(-40i32).to_le_bytes());
    bytes.extend_from_slice(&(12i32).to_le_bytes());
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&(-327680.0f32).to_le_bytes());
    bytes.extend_from_slice(&98304.0f32.to_le_bytes());
    bytes.extend_from_slice(&16384.0f32.to_le_bytes());
    bytes.extend_from_slice(&8192.0f32.to_le_bytes());
    bytes.extend_from_slice(&4096u32.to_le_bytes());
    bytes.extend_from_slice(&256u32.to_le_bytes());
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&288u32.to_le_bytes());
    bytes.extend_from_slice(&14u32.to_le_bytes());
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&32u32.to_le_bytes());
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&11u32.to_le_bytes());
    bytes.extend_from_slice(&32u32.to_le_bytes());
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&36u32.to_le_bytes());
    bytes.extend_from_slice(&4u32.to_le_bytes());
    bytes.extend_from_slice(&20u32.to_le_bytes()); // vertex_stride
    bytes.extend_from_slice(&2u32.to_le_bytes()); // file_index_format = AUTO
    bytes.extend_from_slice(&1u32.to_le_bytes()); // mesh_count

    bytes.extend_from_slice(&100.0f32.to_le_bytes());
    bytes.extend_from_slice(&1.0f32.to_le_bytes());
    bytes.extend_from_slice(&2.0f32.to_le_bytes());
    bytes.extend_from_slice(&3.0f32.to_le_bytes());
    bytes.extend_from_slice(&(-10.0f32).to_le_bytes());
    bytes.extend_from_slice(&(-20.0f32).to_le_bytes());
    bytes.extend_from_slice(&(-30.0f32).to_le_bytes());
    bytes.extend_from_slice(&10.0f32.to_le_bytes());
    bytes.extend_from_slice(&20.0f32.to_le_bytes());
    bytes.extend_from_slice(&30.0f32.to_le_bytes());
    bytes.extend_from_slice(&3u32.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());

    bytes.extend_from_slice(&0.0f32.to_le_bytes());
    bytes.extend_from_slice(&0.0f32.to_le_bytes());
    bytes.extend_from_slice(&0.0f32.to_le_bytes());
    bytes.extend_from_slice(&[128, 128, 255, 255]);
    bytes.extend_from_slice(&[0, 0, 255, 255]);

    bytes.extend_from_slice(&1.0f32.to_le_bytes());
    bytes.extend_from_slice(&0.0f32.to_le_bytes());
    bytes.extend_from_slice(&0.0f32.to_le_bytes());
    bytes.extend_from_slice(&[255, 128, 128, 255]);
    bytes.extend_from_slice(&[0, 255, 0, 255]);

    bytes.extend_from_slice(&0.0f32.to_le_bytes());
    bytes.extend_from_slice(&1.0f32.to_le_bytes());
    bytes.extend_from_slice(&0.0f32.to_le_bytes());
    bytes.extend_from_slice(&[128, 255, 128, 255]);
    bytes.extend_from_slice(&[255, 0, 0, 255]);

    // vertex_count (3) <= 0xFFFF, so AUTO stores u16 triangle indices.
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());

    bytes
}

#[test]
fn terrain_vertex_has_expected_size_and_layout() {
    assert_eq!(size_of::<TerrainVertex>(), TERRAIN_VERTEX_STRIDE as usize);
    assert_eq!(offset_of!(TerrainVertex, position), 0);
    assert_eq!(offset_of!(TerrainVertex, normal), 12);
    assert_eq!(offset_of!(TerrainVertex, color), 16);
}

#[test]
fn pack_ubyte4n_bias_normal_matches_expected_bias_encoding() {
    assert_eq!(pack_ubyte4n_bias_normal(Vec3::new(-1.0, 0.0, 1.0)), [0, 128, 255, 255]);
    assert_eq!(pack_ubyte4n_bias_normal(Vec3::new(2.0, -2.0, 0.5)), [255, 0, 191, 255]);
}

#[test]
fn pack_d3dcolor_vclr_matches_d3dcolor_little_endian_bytes() {
    let packed = pack_d3dcolor_vclr(0x11, 0x22, 0x33, 0x44);
    assert_eq!(packed, [0x33, 0x22, 0x11, 0x44]);
    assert_eq!(u32::from_le_bytes(packed), 0x4411_2233);
}

#[test]
fn terrain_file_matches_fixture_bytes_and_round_trips_exactly() {
    let file = minimal_terrain_file();
    let expected = minimal_terrain_file_bytes();

    let serialized = serialize_terrain_file(&file).unwrap();
    assert_eq!(serialized, expected);

    validate_terrain_header(&expected).unwrap();

    let parsed = deserialize_terrain_file(&expected).unwrap();
    assert_eq!(parsed, file);

    let round_tripped = serialize_terrain_file(&parsed).unwrap();
    assert_eq!(round_tripped, expected);
}

#[test]
fn serialized_file_loads_from_disk() {
    let path = tempfile::NamedTempFile::new().unwrap();
    let file = minimal_terrain_file();

    std::fs::write(path.path(), serialize_terrain_file(&file).unwrap()).unwrap();

    let bytes = std::fs::read(path.path()).unwrap();
    assert_eq!(bytes, minimal_terrain_file_bytes());

    let loaded = load_terrain_file(path.path()).unwrap();
    assert_eq!(loaded, file);
}

#[test]
fn loads_header_without_mesh_payload() {
    let path = tempfile::NamedTempFile::new().unwrap();
    let bytes = minimal_terrain_file_bytes();
    std::fs::write(path.path(), &bytes[..TERRAIN_FILE_HEADER_BYTES]).unwrap();

    let header = load_terrain_header(path.path()).unwrap();
    let file = minimal_terrain_file();
    assert_eq!(header.cell_size, file.cell_size);
    assert_eq!(header.patch_size, file.patch_size);
    assert_eq!(header.origin_cell, file.origin_cell);
    assert_eq!(header.cell_size_xy, file.cell_size_xy);
    assert_eq!(header.world_origin, file.world_origin);
    assert_eq!(header.world_size, file.world_size);
    assert_eq!(header.atlas_size, file.atlas_size);
    assert_eq!(header.logical_tile_size, file.logical_tile_size);
    assert_eq!(header.gutter_size, file.gutter_size);
    assert_eq!(header.physical_tile_size, file.physical_tile_size);
    assert_eq!(header.tiles_per_row, file.tiles_per_row);
    assert_eq!(header.atlas_max_lod, file.atlas_max_lod);
    assert_eq!(header.material_size_xy, file.material_size_xy);
    assert_eq!(header.pattern_count, file.pattern_count);
    assert_eq!(header.pattern_tile_size, file.pattern_tile_size);
    assert_eq!(header.pattern_gutter_size, file.pattern_gutter_size);
    assert_eq!(header.pattern_physical_size, file.pattern_physical_size);
    assert_eq!(header.patterns_per_row, file.patterns_per_row);
    assert_eq!(header.mesh_count, file.meshes.len() as u32);
}

#[test]
fn load_header_rejects_truncated_file() {
    let path = tempfile::NamedTempFile::new().unwrap();
    let bytes = minimal_terrain_file_bytes();
    std::fs::write(path.path(), &bytes[..TERRAIN_FILE_HEADER_BYTES - 1]).unwrap();

    let error = load_terrain_header(path.path()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn rejects_invalid_magic_version_stride_and_index_format() {
    let bytes = minimal_terrain_file_bytes();

    let mut bad_magic = bytes.clone();
    bad_magic[0..8].copy_from_slice(b"BADLAND?");
    let error = deserialize_terrain_file(&bad_magic).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("magic mismatch"));

    let mut bad_version = bytes.clone();
    bad_version[8..12].copy_from_slice(&3u32.to_le_bytes());
    let error = deserialize_terrain_file(&bad_version).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("version mismatch"));

    let mut bad_stride = bytes.clone();
    bad_stride[104..108].copy_from_slice(&16u32.to_le_bytes());
    let error = deserialize_terrain_file(&bad_stride).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("vertex stride mismatch"));

    // Format 1 (the legacy all-u32 layout) is no longer accepted.
    let mut bad_index_format = bytes;
    bad_index_format[108..112].copy_from_slice(&1u32.to_le_bytes());
    let error = deserialize_terrain_file(&bad_index_format).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("index format mismatch"));
}

#[test]
fn narrow_mesh_serializes_u16_indices() {
    // The minimal mesh has 3 vertices (<= 0xFFFF), so triangles are u16 triples.
    let file = minimal_terrain_file();
    let serialized = serialize_terrain_file(&file).unwrap();

    // file header + mesh header + 3 vertices + one 6-byte (u16) triangle.
    let expected = TERRAIN_FILE_HEADER_BYTES + TERRAIN_MESH_HEADER_BYTES + 3 * TERRAIN_VERTEX_STRIDE as usize + 6;
    assert_eq!(serialized.len(), expected);
}

#[test]
fn narrow_mesh_round_trips_across_triangle_conversion_chunks() {
    // Narrowing and widening u16 indices run in fixed-size passes, so a mesh has
    // to span several full passes plus a partial tail to exercise the boundary.
    let vertex_count = 1000usize;
    let triangle_count = 2 * TRIANGLE_CONVERSION_CHUNK + 88;
    let mut file = minimal_terrain_file();
    let template = file.meshes[0].vertices[0];
    file.meshes[0].vertices = vec![template; vertex_count];
    // Vary every index so a pass that dropped, duplicated, or misaligned a chunk
    // would change the decoded triangles rather than reproducing the input.
    file.meshes[0].triangles = (0..triangle_count)
        .map(|i| {
            let base = (i * 3) % vertex_count;
            [
                base as u32,
                ((base + 1) % vertex_count) as u32,
                ((base + 2) % vertex_count) as u32,
            ]
        })
        .collect();

    let serialized = serialize_terrain_file(&file).unwrap();

    let expected = TERRAIN_FILE_HEADER_BYTES
        + TERRAIN_MESH_HEADER_BYTES
        + vertex_count * TERRAIN_VERTEX_STRIDE as usize
        + triangle_count * 6;
    assert_eq!(serialized.len(), expected);

    let parsed = deserialize_terrain_file(&serialized).unwrap();
    assert_eq!(parsed, file);
}

#[test]
fn terrain_mesh_rejects_out_of_bounds_triangle_indices_on_save_and_load() {
    let mut file = minimal_terrain_file();
    file.meshes[0].triangles[0][2] = 3;
    let error = serialize_terrain_file(&file).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(error.to_string().contains("outside vertex count 3"));

    let mut bytes = minimal_terrain_file_bytes();
    let last_index_offset = bytes.len() - std::mem::size_of::<u16>();
    bytes[last_index_offset..].copy_from_slice(&3_u16.to_le_bytes());
    let error = deserialize_terrain_file(&bytes).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("outside vertex count 3"));
}

#[test]
fn wide_mesh_serializes_u32_indices_and_round_trips() {
    // A mesh whose vertex_count exceeds 0xFFFF must store u32 triangle indices.
    let vertex_count = 0x1_0000usize + 1; // 65537 > 0xFFFF
    let mut file = minimal_terrain_file();
    let template = file.meshes[0].vertices[0];
    file.meshes[0].vertices = vec![template; vertex_count];
    file.meshes[0].triangles = vec![[0, 1, 0x1_0000]]; // index 65536 only fits in u32

    let serialized = serialize_terrain_file(&file).unwrap();

    // file header + mesh header + vertices + one 12-byte (u32) triangle.
    let expected =
        TERRAIN_FILE_HEADER_BYTES + TERRAIN_MESH_HEADER_BYTES + vertex_count * TERRAIN_VERTEX_STRIDE as usize + 12;
    assert_eq!(serialized.len(), expected);

    let parsed = deserialize_terrain_file(&serialized).unwrap();
    assert_eq!(parsed, file);
}
