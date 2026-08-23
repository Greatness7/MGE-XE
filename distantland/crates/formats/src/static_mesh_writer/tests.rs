use glam::Vec3;
use half::f16;

use super::*;
use crate::distant_statics::{
    BoundingBox, BoundingSphere, COMPONENT_RECORD_SIZE, HorizonFootprint, PackedDistantStatic, PackedGrassVertex,
    PackedSubset, PackedVertex, SUBSET_RECORD_SIZE, StaticType,
};

fn component(first_triangle: u32, triangle_count: u32) -> ComponentRecord {
    ComponentRecord {
        first_triangle,
        triangle_count,
        ..ComponentRecord::default()
    }
}

#[test]
fn component_records_empty_list_accepted() {
    // No provenance means the subset renders at all tiers, so the tiling rule does not apply
    // even though the subset itself has triangles.
    assert!(validate_component_records(&[], 12).is_ok());
}

#[test]
fn component_records_contiguous_cover_accepted() {
    let components = [component(0, 5), component(5, 7)];
    assert!(validate_component_records(&components, 12).is_ok());
}

#[test]
fn component_records_gap_rejected() {
    let components = [component(0, 5), component(6, 6)];
    assert!(validate_component_records(&components, 12).is_err());
}

#[test]
fn component_records_partial_cover_rejected() {
    let components = [component(0, 5)];
    assert!(validate_component_records(&components, 12).is_err());
}

#[test]
fn component_records_zero_triangle_component_rejected() {
    let components = [component(0, 0)];
    assert!(validate_component_records(&components, 0).is_err());
}

#[test]
fn component_records_grass_classification_rejected() {
    let mut components = [component(0, 12)];
    components[0].classification = StaticType::StaticGrass as u8;
    assert!(validate_component_records(&components, 12).is_err());
}

#[test]
fn component_records_nonzero_reserved_rejected() {
    let mut components = [component(0, 12)];
    components[0].reserved = [1, 0, 0];
    assert!(validate_component_records(&components, 12).is_err());
}

fn make_test_subset(texture: &str, vertex_count: usize, triangle_count: usize) -> PackedSubset {
    let vertices = vec![PackedVertex::default(); vertex_count];
    let triangles = vec![[0u16, 0, 0]; triangle_count];
    PackedSubset {
        bounding_sphere: BoundingSphere {
            radius: 1.0,
            center: Vec3::new(2.0, 3.0, 4.0),
        },
        bounding_box: BoundingBox {
            min: Vec3::new(-1.0, -2.0, -3.0),
            max: Vec3::new(1.0, 2.0, 3.0),
        },
        vertices,
        triangles,
        components: Vec::new(),
        has_alpha: 0,
        has_uv_controller: 1,
        horizon_footprint: HorizonFootprint::default(),
        texture: Box::<str>::from(texture),
    }
}

/// Builds a subset from explicit vertices so geometry-byte layout can be inspected.
fn subset_with(texture: &str, vertices: Vec<PackedVertex>, triangles: Vec<[u16; 3]>) -> PackedSubset {
    PackedSubset {
        bounding_sphere: BoundingSphere {
            radius: 1.0,
            center: Vec3::new(2.0, 3.0, 4.0),
        },
        bounding_box: BoundingBox {
            min: Vec3::new(-1.0, -2.0, -3.0),
            max: Vec3::new(1.0, 2.0, 3.0),
        },
        vertices,
        triangles,
        components: Vec::new(),
        has_alpha: 0,
        has_uv_controller: 0,
        horizon_footprint: HorizonFootprint::default(),
        texture: Box::<str>::from(texture),
    }
}

/// A vertex with distinctive, non-identity values in every field (including `uv_bound`).
fn distinctive_vertex(seed: u8) -> PackedVertex {
    PackedVertex {
        position: [f16::from_f32(seed as f32); 4],
        normal: [seed, seed.wrapping_add(1), seed.wrapping_add(2), seed.wrapping_add(3)],
        color: [seed.wrapping_add(10), seed.wrapping_add(20), seed.wrapping_add(30), 255],
        uv: [f16::from_f32(0.25), f16::from_f32(0.75)],
        // Non-identity so a regression that serialized it for grass would be caught.
        uv_bound: [f16::from_f32(0.5); 4],
    }
}

fn make_test_static(name: &str, subsets: Vec<PackedSubset>) -> (String, PackedDistantStatic) {
    make_typed_static(name, StaticType::StaticTree, subsets)
}

fn make_typed_static(name: &str, static_type: StaticType, subsets: Vec<PackedSubset>) -> (String, PackedDistantStatic) {
    let bs = BoundingSphere {
        radius: 10.0,
        center: Vec3::new(0.0, 0.0, 0.0),
    };
    let bb = BoundingBox {
        min: Vec3::new(-10.0, -10.0, -10.0),
        max: Vec3::new(10.0, 10.0, 10.0),
    };
    (
        name.to_string(),
        PackedDistantStatic {
            bounding_sphere: bs,
            bounding_box: bb,
            static_type,
            subsets,
        },
    )
}

fn static_record<'a>(bytes: &'a [u8], header: &StaticMeshesFileHeader, index: usize) -> &'a StaticRecord {
    let start = header.static_table_offset as usize + index * STATIC_RECORD_SIZE;
    bytemuck::from_bytes::<StaticRecord>(&bytes[start..start + STATIC_RECORD_SIZE])
}

fn subset_record<'a>(bytes: &'a [u8], header: &StaticMeshesFileHeader, index: usize) -> &'a SubsetRecord {
    let start = header.subset_table_offset as usize + index * SUBSET_RECORD_SIZE;
    bytemuck::from_bytes::<SubsetRecord>(&bytes[start..start + SUBSET_RECORD_SIZE])
}

#[test]
fn v4_empty_file() {
    let statics: PackedDistantStatics = PackedDistantStatics::default();
    let bytes = serialize_static_meshes(&statics).unwrap();

    assert_eq!(&bytes[..8], b"XESTAT05");
    let header = bytemuck::from_bytes::<StaticMeshesFileHeader>(&bytes[..HEADER_SIZE]);
    assert_eq!(header.version, 5);
    assert_eq!(header.header_size, HEADER_SIZE as u32);
    assert_eq!(HEADER_SIZE, 136);
    assert_eq!(SUBSET_RECORD_SIZE, 144);
    assert_eq!(COMPONENT_RECORD_SIZE, 16);
    assert_eq!(header.vertex_stride, STATIC_VERTEX_STRIDE as u32);
    assert_eq!(header.vertex_stride, 28);
    assert_eq!(header.grass_vertex_stride, GRASS_VERTEX_STRIDE as u32);
    assert_eq!(header.grass_vertex_stride, 20);
    assert_eq!(header.reserved, 0);
    assert_eq!(header.static_count, 0);
    assert_eq!(header.subset_count, 0);
    assert_eq!(header.static_table_offset, HEADER_SIZE as u64);
    assert_eq!(header.static_table_size, 0);
    assert_eq!(header.subset_table_offset, HEADER_SIZE as u64);
    assert_eq!(header.component_table_offset, HEADER_SIZE as u64);
    assert_eq!(header.component_table_size, 0);
    assert_eq!(header.component_record_size, COMPONENT_RECORD_SIZE as u32);
    assert_eq!(header.component_count, 0);
    assert_eq!(header.texture_blob_offset, HEADER_SIZE as u64);
    assert_eq!(header.texture_blob_size, 0);
}

#[test]
fn v4_header_fields() {
    let distant_statics: PackedDistantStatics = [
        make_test_static("a.nif", vec![make_test_subset("tex.dds", 4, 2)]),
        make_test_static("b.nif", vec![make_test_subset("tx2.dds", 8, 4)]),
    ]
    .into_iter()
    .collect();

    let bytes = serialize_static_meshes(&distant_statics).unwrap();
    let header = bytemuck::from_bytes::<StaticMeshesFileHeader>(&bytes[..HEADER_SIZE]);

    assert_eq!(header.static_count, 2);
    assert_eq!(header.subset_count, 2);
    assert_eq!(header.static_record_size, STATIC_RECORD_SIZE as u32);
    assert_eq!(header.subset_record_size, SUBSET_RECORD_SIZE as u32);
    assert_eq!(header.vertex_stride, STATIC_VERTEX_STRIDE as u32);
    assert_eq!(header.grass_vertex_stride, GRASS_VERTEX_STRIDE as u32);
    assert_eq!(header.reserved, 0);
    assert_eq!(header.index_element_size, INDEX_ELEMENT_SIZE as u32);
}

#[test]
fn v4_file_size_is_exact() {
    let distant_statics: PackedDistantStatics = [
        make_test_static("a.nif", vec![make_test_subset("tex.dds", 4, 2)]),
        make_test_static(
            "b.nif",
            vec![make_test_subset("tex_longer.dds", 8, 4), make_test_subset("xx.dds", 3, 1)],
        ),
    ]
    .into_iter()
    .collect();

    let bytes = serialize_static_meshes(&distant_statics).unwrap();
    let header = bytemuck::from_bytes::<StaticMeshesFileHeader>(&bytes[..HEADER_SIZE]);
    let expected_size = header.geometry_blob_offset + header.geometry_blob_size;
    assert_eq!(bytes.len() as u64, expected_size);
}

#[test]
fn v4_static_table_order() {
    let distant_statics: PackedDistantStatics = [
        make_test_static("a.nif", vec![make_test_subset("a.dds", 1, 1)]),
        make_test_static("b.nif", vec![make_test_subset("b.dds", 1, 1)]),
    ]
    .into_iter()
    .collect();

    let bytes = serialize_static_meshes(&distant_statics).unwrap();
    let header = bytemuck::from_bytes::<StaticMeshesFileHeader>(&bytes[..HEADER_SIZE]);

    let r0 = static_record(&bytes, header, 0);
    let r1 = static_record(&bytes, header, 1);

    assert_eq!(r0.static_type, StaticType::StaticTree as u32);
    assert_eq!(r0.subset_count, 1);
    assert_eq!(r0.first_subset_index, 0);

    assert_eq!(r1.subset_count, 1);
    assert_eq!(r1.first_subset_index, 1);
}

#[test]
fn v4_subset_contiguous_ranges() {
    let distant_statics: PackedDistantStatics = [
        make_test_static(
            "a.nif",
            vec![make_test_subset("a1.dds", 1, 1), make_test_subset("a2.dds", 1, 1)],
        ),
        make_test_static(
            "b.nif",
            vec![
                make_test_subset("b1.dds", 1, 1),
                make_test_subset("b2.dds", 1, 1),
                make_test_subset("b3.dds", 1, 1),
            ],
        ),
    ]
    .into_iter()
    .collect();

    let bytes = serialize_static_meshes(&distant_statics).unwrap();
    let header = bytemuck::from_bytes::<StaticMeshesFileHeader>(&bytes[..HEADER_SIZE]);
    // Static 0: subsets [0, 1]
    let r0 = static_record(&bytes, header, 0);
    assert_eq!(r0.first_subset_index, 0);
    assert_eq!(r0.subset_count, 2);

    // Static 1: subsets [2, 4]
    let r1 = static_record(&bytes, header, 1);
    assert_eq!(r1.first_subset_index, 2);
    assert_eq!(r1.subset_count, 3);
}

#[test]
fn v4_subset_table_aligned() {
    let distant_statics: PackedDistantStatics = [make_test_static("a.nif", vec![make_test_subset("a.dds", 1, 1)])]
        .into_iter()
        .collect();

    let bytes = serialize_static_meshes(&distant_statics).unwrap();
    let header = bytemuck::from_bytes::<StaticMeshesFileHeader>(&bytes[..HEADER_SIZE]);

    assert_eq!(header.subset_table_offset % 8, 0);
    assert_eq!(header.geometry_blob_offset % 8, 0);
}

#[test]
fn v4_texture_paths_nul_terminated() {
    let distant_statics: PackedDistantStatics = [make_test_static("a.nif", vec![make_test_subset("rock.dds", 1, 1)])]
        .into_iter()
        .collect();

    let bytes = serialize_static_meshes(&distant_statics).unwrap();
    let header = bytemuck::from_bytes::<StaticMeshesFileHeader>(&bytes[..HEADER_SIZE]);

    let sub = subset_record(&bytes, header, 0);

    assert_eq!(sub.texture_path_length, 8); // "rock.dds" = 8 bytes
    assert!(
        sub.texture_path_offset + u64::from(sub.texture_path_length) < header.texture_blob_offset + header.texture_blob_size
    );

    let tex_start = sub.texture_path_offset as usize;
    assert_eq!(&bytes[tex_start..tex_start + 8], b"rock.dds");
    assert_eq!(bytes[tex_start + 8], 0); // NUL
}

#[test]
fn v4_repeated_texture_paths_share_blob_offset() {
    let distant_statics: PackedDistantStatics = [make_test_static(
        "a.nif",
        vec![make_test_subset("shared.dds", 1, 1), make_test_subset("shared.dds", 1, 1)],
    )]
    .into_iter()
    .collect();

    let bytes = serialize_static_meshes(&distant_statics).unwrap();
    let header = bytemuck::from_bytes::<StaticMeshesFileHeader>(&bytes[..HEADER_SIZE]);
    let first = subset_record(&bytes, header, 0);
    let second = subset_record(&bytes, header, 1);

    assert_eq!(header.texture_blob_size, "shared.dds".len() as u64 + 1);
    assert_eq!(first.texture_path_offset, second.texture_path_offset);
    assert_eq!(first.texture_path_length, "shared.dds".len() as u32);
    assert_eq!(second.texture_path_length, "shared.dds".len() as u32);

    let tex_start = first.texture_path_offset as usize;
    assert_eq!(&bytes[tex_start..tex_start + "shared.dds".len()], b"shared.dds");
    assert_eq!(bytes[tex_start + "shared.dds".len()], 0);
}

#[test]
fn v4_distinct_texture_paths_use_distinct_blob_offsets() {
    let distant_statics: PackedDistantStatics = [make_test_static(
        "a.nif",
        vec![make_test_subset("first.dds", 1, 1), make_test_subset("second.dds", 1, 1)],
    )]
    .into_iter()
    .collect();

    let bytes = serialize_static_meshes(&distant_statics).unwrap();
    let header = bytemuck::from_bytes::<StaticMeshesFileHeader>(&bytes[..HEADER_SIZE]);
    let first = subset_record(&bytes, header, 0);
    let second = subset_record(&bytes, header, 1);

    assert_eq!(
        header.texture_blob_size,
        ("first.dds".len() + 1 + "second.dds".len() + 1) as u64
    );
    assert_ne!(first.texture_path_offset, second.texture_path_offset);
    assert_eq!(first.texture_path_offset, header.texture_blob_offset);
    assert_eq!(
        second.texture_path_offset,
        header.texture_blob_offset + "first.dds".len() as u64 + 1
    );
}

#[test]
fn v4_subset_flags() {
    let mut subset = make_test_subset("f.dds", 1, 1);
    subset.has_alpha = 1;
    subset.has_uv_controller = 1;

    let distant_statics: PackedDistantStatics = [make_test_static("a.nif", vec![subset])].into_iter().collect();

    let bytes = serialize_static_meshes(&distant_statics).unwrap();
    let header = bytemuck::from_bytes::<StaticMeshesFileHeader>(&bytes[..HEADER_SIZE]);

    let sub = subset_record(&bytes, header, 0);

    assert_eq!(sub.flags, 3); // bit 0 (alpha) + bit 1 (uv_controller)
    assert_eq!(sub.horizon_footprint.vertex_count, 0);
}

#[test]
fn v4_subset_record_includes_horizon_footprint_at_offset_80() {
    let mut subset = make_test_subset("f.dds", 3, 1);
    subset.horizon_footprint = HorizonFootprint {
        max_z: 9.0,
        vertex_count: 3,
        footprint_xy: [[1.0, 2.0], [3.0, 4.0], [5.0, 6.0], [0.0; 2], [0.0; 2], [0.0; 2]],
        ..HorizonFootprint::default()
    };

    let distant_statics: PackedDistantStatics = [make_test_static("a.nif", vec![subset])].into_iter().collect();

    let bytes = serialize_static_meshes(&distant_statics).unwrap();
    let header = bytemuck::from_bytes::<StaticMeshesFileHeader>(&bytes[..HEADER_SIZE]);
    let sub = subset_record(&bytes, header, 0);
    let footprint_offset = header.subset_table_offset as usize + 80;
    let footprint = bytemuck::from_bytes::<HorizonFootprint>(&bytes[footprint_offset..footprint_offset + 56]);

    assert_eq!(std::mem::offset_of!(SubsetRecord, horizon_footprint), 80);
    assert_eq!(sub.horizon_footprint, *footprint);
    assert_eq!(footprint.max_z, 9.0);
    assert_eq!(footprint.vertex_count, 3);
    assert_eq!(footprint.footprint_xy[1], [3.0, 4.0]);
}

#[test]
fn v4_geometry_offsets_sequential() {
    let distant_statics: PackedDistantStatics = [make_test_static(
        "a.nif",
        vec![make_test_subset("a1.dds", 3, 2), make_test_subset("a2.dds", 5, 1)],
    )]
    .into_iter()
    .collect();

    let bytes = serialize_static_meshes(&distant_statics).unwrap();
    let header = bytemuck::from_bytes::<StaticMeshesFileHeader>(&bytes[..HEADER_SIZE]);

    let sub0 = subset_record(&bytes, header, 0);
    let sub1 = subset_record(&bytes, header, 1);

    // sub0: 3 vertices * 28 = 84, 2 triangles * 6 = 12 → total 96 per subset
    let sub0_vertex_bytes = u64::from(sub0.vertex_count) * STATIC_VERTEX_STRIDE as u64;
    let sub0_index_bytes = u64::from(sub0.triangle_count) * 3 * INDEX_ELEMENT_SIZE as u64;

    assert_eq!(sub0.vertex_offset, header.geometry_blob_offset);
    assert_eq!(sub0.index_offset, header.geometry_blob_offset + sub0_vertex_bytes);
    assert_eq!(
        sub1.vertex_offset,
        header.geometry_blob_offset + sub0_vertex_bytes + sub0_index_bytes
    );
}

#[test]
fn v4_empty_texture_path_rejected() {
    let mut subset = make_test_subset("ok.dds", 1, 1);
    subset.texture = Box::<str>::from("");
    let distant_statics: PackedDistantStatics = [make_test_static("a.nif", vec![subset])].into_iter().collect();

    let result = serialize_static_meshes(&distant_statics);
    assert!(result.is_err());
}

#[test]
fn v4_parent_aabb_stored() {
    let bb = BoundingBox {
        min: Vec3::new(-5.0, -6.0, -7.0),
        max: Vec3::new(5.0, 6.0, 7.0),
    };
    let mut ds = PackedDistantStatic::default();
    ds.bounding_box = bb;
    ds.subsets.push(make_test_subset("tex.dds", 1, 1));

    let distant_statics: PackedDistantStatics = [("a.nif".to_string(), ds)].into_iter().collect();
    let bytes = serialize_static_meshes(&distant_statics).unwrap();
    let r = static_record(
        &bytes,
        bytemuck::from_bytes::<StaticMeshesFileHeader>(&bytes[..HEADER_SIZE]),
        0,
    );

    assert_eq!(r.aabb.min.x, -5.0);
    assert_eq!(r.aabb.min.y, -6.0);
    assert_eq!(r.aabb.min.z, -7.0);
    assert_eq!(r.aabb.max.x, 5.0);
    assert_eq!(r.aabb.max.y, 6.0);
    assert_eq!(r.aabb.max.z, 7.0);
}

#[test]
fn v4_packed_grass_vertex_layout_and_color() {
    // The grass projection retains color and is exactly 20 bytes wide.
    assert_eq!(std::mem::size_of::<PackedGrassVertex>(), 20);
    let v = distinctive_vertex(7);
    let g = v.to_grass();
    assert_eq!(g.position, v.position);
    assert_eq!(g.normal, v.normal);
    assert_eq!(g.color, v.color);
    assert_eq!(g.uv, v.uv);
}

#[test]
fn v4_regular_subset_writes_28_byte_vertices() {
    let distant_statics: PackedDistantStatics = [make_typed_static(
        "r.nif",
        StaticType::StaticTree,
        vec![make_test_subset("t.dds", 4, 2)],
    )]
    .into_iter()
    .collect();
    let bytes = serialize_static_meshes(&distant_statics).unwrap();
    let header = bytemuck::from_bytes::<StaticMeshesFileHeader>(&bytes[..HEADER_SIZE]);
    let sub = subset_record(&bytes, header, 0);

    assert_eq!(STATIC_VERTEX_STRIDE, 28);
    assert_eq!(sub.index_offset - sub.vertex_offset, 4 * STATIC_VERTEX_STRIDE as u64);
}

#[test]
fn v4_grass_subset_writes_20_byte_vertices() {
    let distant_statics: PackedDistantStatics = [make_typed_static(
        "g.nif",
        StaticType::StaticGrass,
        vec![make_test_subset("g.dds", 7, 3)],
    )]
    .into_iter()
    .collect();
    let bytes = serialize_static_meshes(&distant_statics).unwrap();
    let header = bytemuck::from_bytes::<StaticMeshesFileHeader>(&bytes[..HEADER_SIZE]);
    let sub = subset_record(&bytes, header, 0);

    assert_eq!(GRASS_VERTEX_STRIDE, 20);
    assert_eq!(sub.index_offset - sub.vertex_offset, 7 * GRASS_VERTEX_STRIDE as u64);
}

#[test]
fn v4_grass_serialized_bytes_drop_uv_bound() {
    let v0 = distinctive_vertex(1);
    let v1 = distinctive_vertex(2);
    let subset = subset_with("grass.dds", vec![v0, v1], vec![[0, 1, 2]]);
    let distant_statics: PackedDistantStatics = [make_typed_static("g.nif", StaticType::StaticGrass, vec![subset])]
        .into_iter()
        .collect();

    let bytes = serialize_static_meshes(&distant_statics).unwrap();
    let header = bytemuck::from_bytes::<StaticMeshesFileHeader>(&bytes[..HEADER_SIZE]);
    let sub = subset_record(&bytes, header, 0);

    // Two grass vertices occupy exactly 2 * 20 bytes (no 8-byte uv_bound per vertex).
    assert_eq!(sub.index_offset - sub.vertex_offset, 2 * GRASS_VERTEX_STRIDE as u64);

    let vo = sub.vertex_offset as usize;
    let g0 = bytemuck::from_bytes::<PackedGrassVertex>(&bytes[vo..vo + 20]);
    let g1 = bytemuck::from_bytes::<PackedGrassVertex>(&bytes[vo + 20..vo + 40]);

    // Bytes are exactly the projection: position, normal, color, uv present; uv_bound gone.
    assert_eq!(*g0, v0.to_grass());
    assert_eq!(*g1, v1.to_grass());
    assert_eq!(g0.color, v0.color);
    assert_eq!(g1.color, v1.color);
}

#[test]
fn v4_mixed_layout_sequential_offsets_and_exact_size() {
    let distant_statics: PackedDistantStatics = [
        make_typed_static(
            "reg_a.nif",
            StaticType::StaticTree,
            vec![make_test_subset("a1.dds", 3, 2), make_test_subset("a2.dds", 4, 1)],
        ),
        make_typed_static("grass.nif", StaticType::StaticGrass, vec![make_test_subset("g.dds", 5, 2)]),
        make_typed_static("reg_b.nif", StaticType::StaticNear, vec![make_test_subset("b.dds", 6, 3)]),
    ]
    .into_iter()
    .collect();

    let bytes = serialize_static_meshes(&distant_statics).unwrap();
    let header = bytemuck::from_bytes::<StaticMeshesFileHeader>(&bytes[..HEADER_SIZE]);

    // Walk the static table in stored order; geometry is laid out in that same order. Each
    // subset's stride is the parent static's stride, and offsets must be globally contiguous.
    let mut cursor = header.geometry_blob_offset;
    for s in 0..header.static_count as usize {
        let sr = static_record(&bytes, header, s);
        let stride = if sr.static_type == StaticType::StaticGrass as u32 {
            GRASS_VERTEX_STRIDE as u64
        } else {
            STATIC_VERTEX_STRIDE as u64
        };
        for offset in 0..sr.subset_count as usize {
            let sub = subset_record(&bytes, header, sr.first_subset_index as usize + offset);
            let vertex_bytes = u64::from(sub.vertex_count) * stride;
            let index_bytes = u64::from(sub.triangle_count) * 3 * INDEX_ELEMENT_SIZE as u64;
            assert_eq!(sub.vertex_offset, cursor);
            assert_eq!(sub.index_offset, cursor + vertex_bytes);
            cursor += vertex_bytes + index_bytes;
        }
    }

    assert_eq!(cursor, header.geometry_blob_offset + header.geometry_blob_size);
    assert_eq!(bytes.len() as u64, cursor);
}
