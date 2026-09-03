use std::io::ErrorKind;

use glam::Vec3;
use half::f16;
use itertools::Itertools;

use super::*;
use crate::{PackedDistantStatics, serialize_static_meshes};

#[test]
fn pack_d3dcolor_vclr_matches_d3dcolor_little_endian_bytes() {
    let packed = pack_d3dcolor_vclr(0x11, 0x22, 0x33, 0x44);
    assert_eq!(packed, [0x33, 0x22, 0x11, 0x44]);
    assert_eq!(u32::from_le_bytes(packed), 0x4411_2233);
}

fn vertex(seed: u8) -> PackedVertex {
    vertex_with_ordinal(seed, 0)
}

fn vertex_with_ordinal(seed: u8, ordinal: u16) -> PackedVertex {
    PackedVertex {
        position: [
            f16::from_f32(seed as f32),
            f16::from_f32(seed as f32 + 1.0),
            f16::from_f32(seed as f32 + 2.0),
            f16::from_f32(f32::from(ordinal)),
        ],
        normal: [seed, seed + 1, seed + 2, 255],
        color: [seed + 3, seed + 4, seed + 5, 255],
        uv: [f16::from_f32(0.25), f16::from_f32(0.75)],
    }
}

fn palette_entry(seed: u8) -> UvBoundRecord {
    let base = f32::from(seed) / 256.0;
    UvBoundRecord {
        bound: [base, base + 0.5, base + 0.125, base + 0.75],
    }
}

fn subset(texture: &str, seed: u8, with_component: bool) -> PackedSubset {
    let components = if with_component {
        vec![ComponentRecord {
            first_triangle: 0,
            triangle_count: 1,
            radius: 12.5,
            classification: StaticType::StaticTree as u8,
            reserved: [0; 3],
        }]
    } else {
        Vec::new()
    };
    PackedSubset {
        bounding_sphere: BoundingSphere {
            radius: 4.0,
            center: Vec3::new(1.0, 2.0, 3.0),
        },
        bounding_box: BoundingBox {
            min: Vec3::new(-1.0, -2.0, -3.0),
            max: Vec3::new(4.0, 5.0, 6.0),
        },
        vertices: vec![vertex(seed), vertex(seed + 10), vertex(seed + 20)],
        triangles: vec![[0, 1, 2]],
        components,
        palette: vec![palette_entry(seed)],
        has_alpha: 1,
        has_uv_controller: 0,
        horizon_footprint: HorizonFootprint {
            max_z: 6.0,
            vertex_count: 3,
            padding: [0; 3],
            footprint_xy: [[-1.0, -2.0], [4.0, -2.0], [4.0, 5.0], [0.0; 2], [0.0; 2], [0.0; 2]],
        },
        texture: texture.into(),
    }
}

fn distant_static(static_type: StaticType, subset: PackedSubset) -> PackedDistantStatic {
    PackedDistantStatic {
        bounding_sphere: BoundingSphere {
            radius: 20.0,
            center: Vec3::new(7.0, 8.0, 9.0),
        },
        bounding_box: BoundingBox {
            min: Vec3::splat(-10.0),
            max: Vec3::splat(10.0),
        },
        static_type,
        subsets: vec![subset],
    }
}

fn fixture() -> (PackedDistantStatics, Vec<u8>) {
    // Grass carries no palette and keeps position.w at 1.0, matching what the packer emits.
    let mut grass = subset("grass.dds", 31, false);
    grass.palette.clear();
    for (index, vertex) in grass.vertices.iter_mut().enumerate() {
        *vertex = vertex_with_ordinal(31 + 10 * index as u8, 1);
    }

    let statics: PackedDistantStatics = [
        (
            "regular.nif".to_string(),
            distant_static(StaticType::StaticTree, subset("opaque.dds", 1, true)),
        ),
        ("grass.nif".to_string(), distant_static(StaticType::StaticGrass, grass)),
    ]
    .into_iter()
    .collect();
    let bytes = serialize_static_meshes(&statics).unwrap();
    (statics, bytes)
}

fn header(bytes: &[u8]) -> StaticMeshesFileHeader {
    bytemuck::pod_read_unaligned(&bytes[..HEADER_SIZE])
}

fn rewrite_pod<T: Pod + Copy>(bytes: &mut [u8], offset: usize, mutate: impl FnOnce(&mut T)) {
    let end = offset + std::mem::size_of::<T>();
    let mut value = bytemuck::pod_read_unaligned::<T>(&bytes[offset..end]);
    mutate(&mut value);
    bytes[offset..end].copy_from_slice(bytemuck::bytes_of(&value));
}

fn rewrite_header(bytes: &mut [u8], mutate: impl FnOnce(&mut StaticMeshesFileHeader)) {
    rewrite_pod(bytes, 0, mutate);
}

fn rewrite_static(bytes: &mut [u8], index: usize, mutate: impl FnOnce(&mut StaticRecord)) {
    let header = header(bytes);
    rewrite_pod(
        bytes,
        header.static_table_offset as usize + index * STATIC_RECORD_SIZE,
        mutate,
    );
}

fn rewrite_subset(bytes: &mut [u8], index: usize, mutate: impl FnOnce(&mut SubsetRecord)) {
    let header = header(bytes);
    rewrite_pod(
        bytes,
        header.subset_table_offset as usize + index * SUBSET_RECORD_SIZE,
        mutate,
    );
}

fn rewrite_component(bytes: &mut [u8], index: usize, mutate: impl FnOnce(&mut ComponentRecord)) {
    let header = header(bytes);
    rewrite_pod(
        bytes,
        header.component_table_offset as usize + index * COMPONENT_RECORD_SIZE,
        mutate,
    );
}

fn rewrite_palette(bytes: &mut [u8], index: usize, mutate: impl FnOnce(&mut UvBoundRecord)) {
    let header = header(bytes);
    rewrite_pod(
        bytes,
        header.palette_table_offset as usize + index * PALETTE_RECORD_SIZE,
        mutate,
    );
}

fn assert_invalid(bytes: &[u8]) {
    let error = deserialize_static_meshes(bytes).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidData, "{error}");
}

#[test]
fn serialize_deserialize_round_trip_preserves_stored_values() {
    let (statics, bytes) = fixture();
    let decoded = deserialize_static_meshes(&bytes).unwrap();
    let expected = statics.values().cloned().collect_vec();

    // Nothing is dropped by the grass projection now: both layouts are identical, so the
    // round trip is exact for grass and statics alike.
    assert_eq!(decoded, expected);
}

#[test]
fn deserialize_accepts_unaligned_input_slices() {
    let (_, bytes) = fixture();
    let expected = deserialize_static_meshes(&bytes).unwrap();
    let mut storage = vec![0_u8; bytes.len() + std::mem::align_of::<SubsetRecord>()];
    let start = (0..std::mem::align_of::<SubsetRecord>())
        .find(|&offset| {
            !(storage.as_ptr() as usize + offset + HEADER_SIZE).is_multiple_of(std::mem::align_of::<StaticRecord>())
        })
        .unwrap();
    storage[start..start + bytes.len()].copy_from_slice(&bytes);

    let decoded = deserialize_static_meshes(&storage[start..start + bytes.len()]).unwrap();
    assert_eq!(decoded, expected);
}

#[test]
fn load_static_meshes_reads_a_serialized_file() {
    let (_, bytes) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("static_meshes");
    std::fs::write(&path, bytes).unwrap();

    let decoded = load_static_meshes(&path).unwrap();

    assert_eq!(decoded.len(), 2);
}

#[test]
fn every_truncated_prefix_is_rejected() {
    let (_, bytes) = fixture();

    for end in 0..bytes.len() {
        assert_invalid(&bytes[..end]);
    }
}

#[test]
fn header_identity_and_fixed_layout_fields_are_validated() {
    let (_, original) = fixture();
    let mutations: &[fn(&mut StaticMeshesFileHeader)] = &[
        |header| header.magic = *b"BADMAGIC",
        |header| header.version += 1,
        |header| header.header_size += 1,
        |header| header.static_record_size += 1,
        |header| header.subset_record_size += 1,
        |header| header.component_record_size += 1,
        |header| header.palette_record_size += 1,
        |header| header.vertex_stride += 1,
        |header| header.grass_vertex_stride += 1,
        |header| header.index_element_size += 1,
        |header| header.reserved = 1,
    ];

    for mutation in mutations {
        let mut bytes = original.clone();
        rewrite_header(&mut bytes, mutation);
        assert_invalid(&bytes);
    }
}

#[test]
fn header_table_counts_sizes_offsets_and_alignment_are_validated() {
    let (_, original) = fixture();
    let mutations: &[fn(&mut StaticMeshesFileHeader)] = &[
        |header| header.static_count += 1,
        |header| header.subset_count += 1,
        |header| header.component_count += 1,
        |header| header.palette_count += 1,
        |header| header.static_table_size += 1,
        |header| header.subset_table_size += 1,
        |header| header.component_table_size += 1,
        |header| header.palette_table_size += 1,
        |header| header.static_table_offset += 8,
        |header| header.subset_table_offset += 1,
        |header| header.component_table_offset += 1,
        |header| header.palette_table_offset += 1,
        |header| header.texture_blob_offset = header.palette_table_offset,
        |header| header.geometry_blob_offset += 1,
        |header| header.geometry_blob_size += 1,
    ];

    for mutation in mutations {
        let mut bytes = original.clone();
        rewrite_header(&mut bytes, mutation);
        assert_invalid(&bytes);
    }
}

#[test]
fn static_and_subset_ranges_are_validated() {
    let (_, original) = fixture();

    let mut bytes = original.clone();
    rewrite_static(&mut bytes, 0, |record| record.static_type = 99);
    assert_invalid(&bytes);

    let mut bytes = original.clone();
    rewrite_static(&mut bytes, 0, |record| record.first_subset_index = 1);
    assert_invalid(&bytes);

    let mut bytes = original.clone();
    rewrite_static(&mut bytes, 0, |record| record.subset_count = u32::MAX);
    assert_invalid(&bytes);

    let mut bytes = original.clone();
    rewrite_subset(&mut bytes, 0, |record| record.first_component_index = 1);
    assert_invalid(&bytes);

    let mut bytes = original.clone();
    rewrite_subset(&mut bytes, 0, |record| record.component_count = u32::MAX);
    assert_invalid(&bytes);

    let mut bytes = original.clone();
    rewrite_subset(&mut bytes, 0, |record| record.first_palette_index = 1);
    assert_invalid(&bytes);

    let mut bytes = original.clone();
    rewrite_subset(&mut bytes, 0, |record| record.palette_count = u32::MAX);
    assert_invalid(&bytes);
}

#[test]
fn texture_path_offset_length_termination_and_encoding_are_validated() {
    let (_, original) = fixture();
    let original_header = header(&original);

    let mut bytes = original.clone();
    rewrite_subset(&mut bytes, 0, |record| record.texture_path_offset = 0);
    assert_invalid(&bytes);

    let mut bytes = original.clone();
    rewrite_subset(&mut bytes, 0, |record| record.texture_path_length = 0);
    assert_invalid(&bytes);

    let record_offset = original_header.subset_table_offset as usize;
    let record = bytemuck::pod_read_unaligned::<SubsetRecord>(&original[record_offset..record_offset + SUBSET_RECORD_SIZE]);
    let path_start = record.texture_path_offset as usize;
    let terminator = path_start + record.texture_path_length as usize;

    let mut bytes = original.clone();
    bytes[terminator] = b'x';
    assert_invalid(&bytes);

    let mut bytes = original.clone();
    bytes[path_start + 1] = 0;
    assert_invalid(&bytes);

    let mut bytes = original.clone();
    bytes[path_start] = 0xff;
    assert_invalid(&bytes);
}

#[test]
fn geometry_offsets_counts_and_indices_are_validated() {
    let (_, original) = fixture();

    let mut bytes = original.clone();
    rewrite_subset(&mut bytes, 0, |record| record.vertex_offset += 1);
    assert_invalid(&bytes);

    let mut bytes = original.clone();
    rewrite_subset(&mut bytes, 0, |record| record.index_offset += 1);
    assert_invalid(&bytes);

    let mut bytes = original.clone();
    rewrite_subset(&mut bytes, 0, |record| record.vertex_count = u32::from(u16::MAX) + 1);
    assert_invalid(&bytes);

    let mut bytes = original.clone();
    rewrite_subset(&mut bytes, 0, |record| record.triangle_count = u32::MAX);
    assert_invalid(&bytes);

    let record_offset = header(&original).subset_table_offset as usize;
    let record = bytemuck::pod_read_unaligned::<SubsetRecord>(&original[record_offset..record_offset + SUBSET_RECORD_SIZE]);
    let mut bytes = original.clone();
    bytes[record.index_offset as usize..record.index_offset as usize + 2].copy_from_slice(&u16::MAX.to_ne_bytes());
    assert_invalid(&bytes);
}

#[test]
fn flags_horizon_and_component_semantics_are_validated() {
    let (_, original) = fixture();

    let mut bytes = original.clone();
    rewrite_subset(&mut bytes, 0, |record| record.flags |= 4);
    assert_invalid(&bytes);

    let mut bytes = original.clone();
    rewrite_subset(&mut bytes, 0, |record| record.horizon_footprint.vertex_count = 7);
    assert_invalid(&bytes);

    let mut bytes = original.clone();
    rewrite_subset(&mut bytes, 0, |record| record.horizon_footprint.padding[0] = 1);
    assert_invalid(&bytes);

    let mut bytes = original.clone();
    rewrite_subset(&mut bytes, 0, |record| record.horizon_footprint.footprint_xy[5] = [1.0, 2.0]);
    assert_invalid(&bytes);

    let mutations: &[fn(&mut ComponentRecord)] = &[
        |component| component.first_triangle = 1,
        |component| component.triangle_count = 0,
        |component| component.triangle_count = 2,
        |component| component.radius = f32::NAN,
        |component| component.radius = -1.0,
        |component| component.classification = 99,
        |component| component.reserved[0] = 1,
    ];
    for mutation in mutations {
        let mut bytes = original.clone();
        rewrite_component(&mut bytes, 0, mutation);
        assert_invalid(&bytes);
    }
}

#[test]
fn palette_ranges_are_validated_and_entries_round_trip() {
    let (statics, original) = fixture();

    // The palette decodes back to exactly what was written, bit for bit.
    let decoded = deserialize_static_meshes(&original).unwrap();
    let expected = statics.values().next().unwrap().subsets[0].palette.clone();
    assert_eq!(decoded[0].subsets[0].palette, expected);
    assert!(!expected.is_empty());
    // Grass carries no palette entries.
    assert!(decoded[1].subsets[0].palette.is_empty());

    // The palette table is real bytes: perturbing an entry changes the decode.
    let mut bytes = original.clone();
    rewrite_palette(&mut bytes, 0, |entry| entry.bound[0] = 0.375);
    let perturbed = deserialize_static_meshes(&bytes).unwrap();
    assert_ne!(perturbed[0].subsets[0].palette, expected);

    // `first_palette_index` past the table would wrap the unsigned subtraction that the
    // remaining-entries check performs, so it is validated on its own.
    let mut bytes = original.clone();
    rewrite_subset(&mut bytes, 0, |record| {
        record.first_palette_index = u32::MAX;
        record.palette_count = 0;
    });
    assert_invalid(&bytes);

    // Over-cap palettes are rejected even when the range is inside the table.
    let mut bytes = original.clone();
    rewrite_header(&mut bytes, |header| {
        header.palette_count = UV_BOUND_PALETTE_CAP + 1;
        header.palette_table_size = u64::from(UV_BOUND_PALETTE_CAP + 1) * PALETTE_RECORD_SIZE as u64;
    });
    rewrite_subset(&mut bytes, 0, |record| {
        record.palette_count = UV_BOUND_PALETTE_CAP + 1;
    });
    assert_invalid(&bytes);
}
