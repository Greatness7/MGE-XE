//! MGE-XE `static_meshes` v5 POD structures, serialization layout, and decoding.

use std::borrow::Cow;
use std::fs;
use std::io;
use std::ops::Range;
use std::path::Path;

use bytemuck::{NoUninit, Pod, Zeroable};
use glam::Vec3;
use half::f16;

/// MGE-XE static classification written into `static_meshes`.
#[repr(u8)]
#[derive(NoUninit, Clone, Copy, Default, Debug, PartialEq, Eq)]
pub enum StaticType {
    /// Let MGE-XE infer the class at runtime.
    #[default]
    StaticAuto = 0,
    /// Draw close to the player.
    StaticNear = 1,
    /// Draw at medium distance.
    StaticFar = 2,
    /// Draw at the farthest distance bucket.
    StaticVeryFar = 3,
    /// Treat the mesh as grass.
    StaticGrass = 4,
    /// Treat the mesh as a tree.
    StaticTree = 5,
    /// Treat the mesh as a building.
    StaticBuilding = 6,
}

/// Axis-aligned bounds stored in MGE-XE distant-land files.
#[derive(Pod, Zeroable, Clone, Copy, Default, Debug, PartialEq)]
#[repr(C)]
pub struct BoundingBox {
    /// Minimum corner in world space.
    pub min: Vec3,
    /// Maximum corner in world space.
    pub max: Vec3,
}

/// Bounding sphere stored in MGE-XE distant-land files.
#[derive(Pod, Zeroable, Clone, Copy, Default, Debug, PartialEq)]
#[repr(C)]
pub struct BoundingSphere {
    /// Sphere radius.
    pub radius: f32,
    /// Sphere center in world space.
    pub center: Vec3,
}

/// Maximum number of vertices in a generated terrain-horizon culling footprint.
pub const HORIZON_FOOTPRINT_MAX_VERTS: usize = 6;

/// Generated terrain-horizon culling footprint for one subset.
#[derive(Pod, Zeroable, Clone, Copy, Default, Debug, PartialEq)]
#[repr(C)]
pub struct HorizonFootprint {
    /// Subset-local maximum Z for the final subset geometry.
    pub max_z: f32,
    /// Number of valid vertices in `footprint_xy`; zero means no generated footprint.
    pub vertex_count: u8,
    /// Reserved padding; must be zero.
    pub padding: [u8; 3],
    /// Convex subset-local XY polygon. Unused slots are zero.
    pub footprint_xy: [[f32; 2]; HORIZON_FOOTPRINT_MAX_VERTS],
}

/// One source-component range in a v5 merged static subset.
#[derive(Pod, Zeroable, Clone, Copy, Default, Debug, PartialEq)]
#[repr(C)]
pub struct ComponentRecord {
    /// First triangle in the owning subset.
    pub first_triangle: u32,
    /// Number of triangles in this component.
    pub triangle_count: u32,
    /// Whole-source-model bounding-sphere radius times instance scale.
    pub radius: f32,
    /// Source [`StaticType`] encoded as `u8`.
    pub classification: u8,
    /// Reserved padding; must be zero.
    pub reserved: [u8; 3],
}

/// Packed distant-static vertex as written to `static_meshes`.
#[derive(Pod, Zeroable, Clone, Copy, Default, Debug, PartialEq)]
#[repr(C)]
pub struct PackedVertex {
    /// Homogeneous position with packed `f16` components.
    pub position: [f16; 4],
    /// Packed normalized normal vector.
    pub normal: [u8; 4],
    /// Packed vertex color.
    pub color: [u8; 4],
    /// Primary UV coordinate.
    pub uv: [f16; 2],
    /// Atlas UV bounds.
    pub uv_bound: [f16; 4],
}

impl PackedVertex {
    /// Projects this vertex onto the 20-byte grass layout, dropping `uv_bound`.
    pub fn to_grass(&self) -> PackedGrassVertex {
        PackedGrassVertex {
            position: self.position,
            normal: self.normal,
            color: self.color,
            uv: self.uv,
        }
    }

    /// Returns a reference to the first 20 bytes of this vertex, reinterpreted as a
    /// [`PackedGrassVertex`]. Zero-cost: no copy, no allocation.
    ///
    /// Both structs are `#[repr(C)]` with the same field prefix in the same order,
    /// so the first `size_of::<PackedGrassVertex>()` bytes are a valid grass vertex.
    pub fn as_grass(&self) -> &PackedGrassVertex {
        const {
            use std::mem::offset_of;
            assert!(
                size_of::<PackedGrassVertex>() <= size_of::<PackedVertex>()
                    && align_of::<PackedGrassVertex>() <= align_of::<PackedVertex>()
                    && offset_of!(PackedVertex, position) == offset_of!(PackedGrassVertex, position)
                    && offset_of!(PackedVertex, normal) == offset_of!(PackedGrassVertex, normal)
                    && offset_of!(PackedVertex, color) == offset_of!(PackedGrassVertex, color)
                    && offset_of!(PackedVertex, uv) == offset_of!(PackedGrassVertex, uv),
            );
        }
        const SIZE: usize = size_of::<PackedGrassVertex>();
        let bytes = bytemuck::bytes_of(self);
        bytemuck::from_bytes(&bytes[..SIZE])
    }
}

/// Packed grass vertex as written to `static_meshes` (20 bytes, omits `uv_bound`).
///
/// Grass meshes share `PackedVertex` in memory throughout the pipeline; only the final
/// geometry serialization projects each vertex into this tightly packed grass layout.
#[derive(Pod, Zeroable, Clone, Copy, Default, Debug, PartialEq)]
#[repr(C)]
pub struct PackedGrassVertex {
    /// Homogeneous position with packed `f16` components.
    pub position: [f16; 4],
    /// Packed normalized normal vector.
    pub normal: [u8; 4],
    /// Packed vertex color.
    pub color: [u8; 4],
    /// Primary UV coordinate.
    pub uv: [f16; 2],
}

/// One texture/material subset inside an MGE-XE distant static.
#[derive(Clone, Debug, PartialEq)]
pub struct PackedSubset {
    /// Bounding sphere for the subset geometry.
    pub bounding_sphere: BoundingSphere,
    /// Axis-aligned bounds for the subset geometry.
    pub bounding_box: BoundingBox,
    /// Packed vertex buffer.
    pub vertices: Vec<PackedVertex>,
    /// Triangle index buffer.
    pub triangles: Vec<[u16; 3]>,
    /// Component provenance for v5 merged synthetic statics.
    pub components: Vec<ComponentRecord>,
    /// Non-zero when this subset uses alpha blending.
    pub has_alpha: u8,
    /// Non-zero when this subset uses an animated UV controller.
    pub has_uv_controller: u8,
    /// Optional generated terrain-horizon culling footprint.
    pub horizon_footprint: HorizonFootprint,
    /// Texture path serialized as a NUL-terminated path in the static mesh file.
    pub texture: Box<str>,
}

impl Default for PackedSubset {
    fn default() -> Self {
        Self {
            bounding_sphere: BoundingSphere::default(),
            bounding_box: BoundingBox::default(),
            vertices: Vec::default(),
            triangles: Vec::default(),
            components: Vec::default(),
            has_alpha: 0,
            has_uv_controller: 0,
            horizon_footprint: HorizonFootprint::default(),
            texture: Box::<str>::from(""),
        }
    }
}

/// One top-level static entry in MGE-XE `static_meshes`.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct PackedDistantStatic {
    /// Bounding sphere for the whole static.
    pub bounding_sphere: BoundingSphere,
    /// Axis-aligned bounds for the whole static.
    pub bounding_box: BoundingBox,
    /// Static classification used by MGE-XE.
    pub static_type: StaticType,
    /// Geometry subsets that make up the static.
    pub subsets: Vec<PackedSubset>,
}

/// Magic bytes for the v5 static-meshes file.
pub const STATIC_MESHES_MAGIC: &[u8; 8] = b"XESTAT05";
/// V5 file-format version.
pub const STATIC_MESHES_VERSION: u32 = 5;
/// Byte size of the file header.
pub const HEADER_SIZE: usize = 136;
/// Byte size of one `StaticRecord`.
pub const STATIC_RECORD_SIZE: usize = 52;
/// Byte size of one `SubsetRecord`.
pub const SUBSET_RECORD_SIZE: usize = 144;
/// Byte size of one `ComponentRecord`.
pub const COMPONENT_RECORD_SIZE: usize = 16;
/// Byte stride of one regular (non-grass) `PackedVertex` in the geometry blob.
pub const STATIC_VERTEX_STRIDE: usize = std::mem::size_of::<PackedVertex>();
/// Byte stride of one grass `PackedGrassVertex` in the geometry blob.
pub const GRASS_VERTEX_STRIDE: usize = std::mem::size_of::<PackedGrassVertex>();
/// Byte size of one index element (`u16`).
pub const INDEX_ELEMENT_SIZE: usize = 2;

/// V5 static-meshes file header (136 bytes).
#[derive(Pod, Zeroable, Clone, Copy, Default, Debug, PartialEq)]
#[repr(C)]
pub struct StaticMeshesFileHeader {
    /// Magic `XESTAT05`.
    pub magic: [u8; 8],
    /// File-format version (5).
    pub version: u32,
    /// Header byte size (136).
    pub header_size: u32,
    /// `StaticRecord` byte size (52).
    pub static_record_size: u32,
    /// `SubsetRecord` byte size (144).
    pub subset_record_size: u32,
    /// Regular (non-grass) vertex byte stride (28).
    pub vertex_stride: u32,
    /// Index element byte size (2).
    pub index_element_size: u32,
    /// Number of top-level statics in the file.
    pub static_count: u32,
    /// Number of geometry subsets across all statics.
    pub subset_count: u32,
    /// File-absolute offset of the static table.
    pub static_table_offset: u64,
    /// Total bytes of the static table (`static_count * static_record_size`).
    pub static_table_size: u64,
    /// File-absolute offset of the subset table (8-byte aligned).
    pub subset_table_offset: u64,
    /// Total bytes of the subset table (`subset_count * subset_record_size`).
    pub subset_table_size: u64,
    /// File-absolute offset of the texture-path blob.
    pub texture_blob_offset: u64,
    /// Total bytes of the texture blob.
    pub texture_blob_size: u64,
    /// File-absolute offset of the geometry blob (8-byte aligned).
    pub geometry_blob_offset: u64,
    /// Total bytes of the geometry blob.
    pub geometry_blob_size: u64,
    /// Grass vertex byte stride (20).
    pub grass_vertex_stride: u32,
    /// Reserved; must be zero.
    pub reserved: u32,
    /// File-absolute offset of the component table (8-byte aligned).
    pub component_table_offset: u64,
    /// Total bytes of the component table (`component_count * component_record_size`).
    pub component_table_size: u64,
    /// `ComponentRecord` byte size (16).
    pub component_record_size: u32,
    /// Number of component records across all subsets.
    pub component_count: u32,
}

/// One top-level static entry in the static table (52 bytes).
#[derive(Pod, Zeroable, Clone, Copy, Default, Debug, PartialEq)]
#[repr(C)]
pub struct StaticRecord {
    /// `StaticType` cast to `u32`.
    pub static_type: u32,
    /// Parent bounding sphere.
    pub sphere: BoundingSphere,
    /// Parent AABB as stored by the generator (source of truth for the loader).
    pub aabb: BoundingBox,
    /// Index of the first subset owned by this static.
    pub first_subset_index: u32,
    /// Number of subsets owned by this static.
    pub subset_count: u32,
}

/// One subset entry in the subset table (144 bytes).
#[derive(Pod, Zeroable, Clone, Copy, Default, Debug, PartialEq)]
#[repr(C)]
pub struct SubsetRecord {
    /// Subset bounding sphere.
    pub sphere: BoundingSphere,
    /// Subset AABB.
    pub aabb: BoundingBox,
    /// File-absolute offset of the NUL-terminated texture path in the texture blob.
    pub texture_path_offset: u64,
    /// File-absolute offset of the vertex payload in the geometry blob.
    pub vertex_offset: u64,
    /// File-absolute offset of the index payload in the geometry blob.
    pub index_offset: u64,
    /// Number of packed vertices.
    pub vertex_count: u32,
    /// Number of triangles.
    pub triangle_count: u32,
    /// Bitmask: bit 0 = has_alpha, bit 1 = has_uv_controller.
    pub flags: u32,
    /// Length of the texture path in bytes (excluding the trailing NUL).
    pub texture_path_length: u32,
    /// Optional generated terrain-horizon culling footprint.
    pub horizon_footprint: HorizonFootprint,
    /// Index of the first component owned by this subset.
    pub first_component_index: u32,
    /// Number of components owned by this subset.
    pub component_count: u32,
}

/// Deserializes a complete v5 `static_meshes` file into its stored static records.
///
/// Source mesh keys are not present in the file, so the returned vector follows the file's
/// static-table order. All file offsets, table sizes, record ranges, texture strings, component
/// ranges, vertex strides, and triangle indices are validated before a value is returned.
/// Grass vertices have no stored `uv_bound`; that field is reconstructed as zero.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidData`] when the bytes are truncated, use an unsupported
/// format, contain an invalid range or record, or have unexpected trailing data.
pub fn deserialize_static_meshes(bytes: &[u8]) -> io::Result<Vec<PackedDistantStatic>> {
    if bytes.len() < HEADER_SIZE {
        return Err(invalid_data(format!(
            "static_meshes is truncated: expected at least {HEADER_SIZE} header bytes, found {}",
            bytes.len()
        )));
    }

    let header = bytemuck::pod_read_unaligned::<StaticMeshesFileHeader>(&bytes[..HEADER_SIZE]);
    validate_header(&header)?;

    let static_table_size = checked_product(header.static_count, STATIC_RECORD_SIZE, "static_meshes static table size")?;
    let subset_table_size = checked_product(header.subset_count, SUBSET_RECORD_SIZE, "static_meshes subset table size")?;
    let component_table_size = checked_product(
        header.component_count,
        COMPONENT_RECORD_SIZE,
        "static_meshes component table size",
    )?;
    require_size(header.static_table_size, static_table_size, "static table")?;
    require_size(header.subset_table_size, subset_table_size, "subset table")?;
    require_size(header.component_table_size, component_table_size, "component table")?;

    let static_range = checked_file_range(
        header.static_table_offset,
        header.static_table_size,
        bytes.len(),
        "static table",
    )?;
    let subset_range = checked_file_range(
        header.subset_table_offset,
        header.subset_table_size,
        bytes.len(),
        "subset table",
    )?;
    let component_range = checked_file_range(
        header.component_table_offset,
        header.component_table_size,
        bytes.len(),
        "component table",
    )?;
    let texture_range = checked_file_range(
        header.texture_blob_offset,
        header.texture_blob_size,
        bytes.len(),
        "texture blob",
    )?;
    let geometry_range = checked_file_range(
        header.geometry_blob_offset,
        header.geometry_blob_size,
        bytes.len(),
        "geometry blob",
    )?;

    if header.static_table_offset != HEADER_SIZE as u64 {
        return Err(invalid_data(format!(
            "static_meshes static table must begin at byte {HEADER_SIZE}, found {}",
            header.static_table_offset
        )));
    }
    for (name, offset) in [
        ("static table", header.static_table_offset),
        ("subset table", header.subset_table_offset),
        ("component table", header.component_table_offset),
        ("geometry blob", header.geometry_blob_offset),
    ] {
        if offset % 8 != 0 {
            return Err(invalid_data(format!(
                "static_meshes {name} offset {offset} is not 8-byte aligned"
            )));
        }
    }
    require_ordered_ranges(
        [
            ("static table", &static_range),
            ("subset table", &subset_range),
            ("component table", &component_range),
            ("texture blob", &texture_range),
            ("geometry blob", &geometry_range),
        ],
        bytes.len(),
    )?;

    let static_records = read_pod_table::<StaticRecord>(bytes, static_range, header.static_count, "static table")?;
    let subset_records = read_pod_table::<SubsetRecord>(bytes, subset_range, header.subset_count, "subset table")?;
    let component_records =
        read_pod_table::<ComponentRecord>(bytes, component_range, header.component_count, "component table")?;

    let mut expected_subset = 0_u32;
    let mut expected_component = 0_u32;
    let mut geometry_cursor = geometry_range.start;
    let mut statics = Vec::with_capacity(static_records.len());

    for (static_index, record) in static_records.iter().copied().enumerate() {
        let static_type = static_type_from_u32(record.static_type).ok_or_else(|| {
            invalid_data(format!(
                "static_meshes static {static_index} has invalid static type {}",
                record.static_type
            ))
        })?;
        if record.first_subset_index != expected_subset {
            return Err(invalid_data(format!(
                "static_meshes static {static_index} subset range begins at {}, expected {expected_subset}",
                record.first_subset_index
            )));
        }
        let subset_end = record
            .first_subset_index
            .checked_add(record.subset_count)
            .ok_or_else(|| invalid_data(format!("static_meshes static {static_index} subset range overflows u32")))?;
        if subset_end > header.subset_count {
            return Err(invalid_data(format!(
                "static_meshes static {static_index} subset range ends at {subset_end}, beyond subset count {}",
                header.subset_count
            )));
        }

        let mut subsets = Vec::with_capacity(record.subset_count as usize);
        for subset_index in record.first_subset_index..subset_end {
            let subset_record = subset_records[subset_index as usize];
            let decoded = decode_subset(
                bytes,
                &header,
                &texture_range,
                &geometry_range,
                &component_records,
                subset_index,
                static_type,
                subset_record,
                &mut expected_component,
                &mut geometry_cursor,
            )?;
            subsets.push(decoded);
        }
        expected_subset = subset_end;
        statics.push(PackedDistantStatic {
            bounding_sphere: record.sphere,
            bounding_box: record.aabb,
            static_type,
            subsets,
        });
    }

    if expected_subset != header.subset_count {
        return Err(invalid_data(format!(
            "static_meshes static ranges cover {expected_subset} of {} subsets",
            header.subset_count
        )));
    }
    if expected_component != header.component_count {
        return Err(invalid_data(format!(
            "static_meshes subset ranges cover {expected_component} of {} components",
            header.component_count
        )));
    }
    if geometry_cursor != geometry_range.end {
        return Err(invalid_data(format!(
            "static_meshes geometry records end at byte {geometry_cursor}, expected {}",
            geometry_range.end
        )));
    }

    Ok(statics)
}

/// Loads and deserializes a v5 `static_meshes` file.
///
/// # Errors
///
/// Returns an I/O error if the file cannot be read, or [`io::ErrorKind::InvalidData`] if its
/// contents fail [`deserialize_static_meshes`] validation.
pub fn load_static_meshes(path: &Path) -> io::Result<Vec<PackedDistantStatic>> {
    let file = fs::File::open(path)?;
    // SAFETY: the mapping is read-only and remains borrowed only for this call. Generated
    // output validation requires callers not to replace or mutate the file concurrently.
    let bytes = unsafe { memmap2::MmapOptions::new().map(&file)? };
    deserialize_static_meshes(&bytes)
}

#[allow(clippy::too_many_arguments)]
fn decode_subset(
    bytes: &[u8],
    header: &StaticMeshesFileHeader,
    texture_blob: &Range<usize>,
    geometry_blob: &Range<usize>,
    component_records: &[ComponentRecord],
    subset_index: u32,
    static_type: StaticType,
    record: SubsetRecord,
    expected_component: &mut u32,
    geometry_cursor: &mut usize,
) -> io::Result<PackedSubset> {
    if record.flags & !0b11 != 0 {
        return Err(invalid_data(format!(
            "static_meshes subset {subset_index} has unknown flags {:#x}",
            record.flags
        )));
    }
    validate_horizon_footprint(record.horizon_footprint, subset_index)?;
    let texture = decode_texture_path(bytes, texture_blob, subset_index, record)?;

    if record.first_component_index != *expected_component {
        return Err(invalid_data(format!(
            "static_meshes subset {subset_index} component range begins at {}, expected {}",
            record.first_component_index, *expected_component
        )));
    }
    let component_end = record
        .first_component_index
        .checked_add(record.component_count)
        .ok_or_else(|| invalid_data(format!("static_meshes subset {subset_index} component range overflows u32")))?;
    if component_end > header.component_count {
        return Err(invalid_data(format!(
            "static_meshes subset {subset_index} component range ends at {component_end}, beyond component count {}",
            header.component_count
        )));
    }
    let components = component_records[record.first_component_index as usize..component_end as usize].to_vec();
    validate_components(&components, record.triangle_count, subset_index)?;
    *expected_component = component_end;

    if record.vertex_count > u16::MAX as u32 {
        return Err(invalid_data(format!(
            "static_meshes subset {subset_index} vertex count {} exceeds the u16 index limit",
            record.vertex_count
        )));
    }
    let vertex_stride = if static_type == StaticType::StaticGrass {
        GRASS_VERTEX_STRIDE
    } else {
        STATIC_VERTEX_STRIDE
    };
    let vertex_bytes = checked_product(record.vertex_count, vertex_stride, "static_meshes vertex payload size")?;
    let index_count = record
        .triangle_count
        .checked_mul(3)
        .ok_or_else(|| invalid_data(format!("static_meshes subset {subset_index} index count overflows u32")))?;
    let index_bytes = checked_product(index_count, INDEX_ELEMENT_SIZE, "static_meshes index payload size")?;
    let vertex_range = checked_file_range(record.vertex_offset, vertex_bytes, bytes.len(), "subset vertex payload")?;
    let index_range = checked_file_range(record.index_offset, index_bytes, bytes.len(), "subset index payload")?;
    if vertex_range.start != *geometry_cursor {
        return Err(invalid_data(format!(
            "static_meshes subset {subset_index} vertex payload begins at byte {}, expected {}",
            vertex_range.start, *geometry_cursor
        )));
    }
    if index_range.start != vertex_range.end {
        return Err(invalid_data(format!(
            "static_meshes subset {subset_index} index payload begins at byte {}, expected {}",
            index_range.start, vertex_range.end
        )));
    }
    if vertex_range.start < geometry_blob.start || index_range.end > geometry_blob.end {
        return Err(invalid_data(format!(
            "static_meshes subset {subset_index} geometry payload lies outside the geometry blob"
        )));
    }

    let vertices = if static_type == StaticType::StaticGrass {
        read_pod_table::<PackedGrassVertex>(bytes, vertex_range, record.vertex_count, "grass vertex payload")?
            .iter()
            .map(|&vertex| PackedVertex {
                position: vertex.position,
                normal: vertex.normal,
                color: vertex.color,
                uv: vertex.uv,
                uv_bound: [f16::ZERO; 4],
            })
            .collect()
    } else {
        read_pod_table::<PackedVertex>(bytes, vertex_range, record.vertex_count, "static vertex payload")?.into_owned()
    };
    let triangles =
        read_pod_table::<[u16; 3]>(bytes, index_range.clone(), record.triangle_count, "triangle payload")?.into_owned();
    for (triangle_index, triangle) in triangles.iter().enumerate() {
        for index in triangle {
            if u32::from(*index) >= record.vertex_count {
                return Err(invalid_data(format!(
                    "static_meshes subset {subset_index} triangle {triangle_index} index {index} is outside vertex count {}",
                    record.vertex_count
                )));
            }
        }
    }
    *geometry_cursor = index_range.end;

    Ok(PackedSubset {
        bounding_sphere: record.sphere,
        bounding_box: record.aabb,
        vertices,
        triangles,
        components,
        has_alpha: (record.flags & 1) as u8,
        has_uv_controller: ((record.flags >> 1) & 1) as u8,
        horizon_footprint: record.horizon_footprint,
        texture,
    })
}

fn validate_header(header: &StaticMeshesFileHeader) -> io::Result<()> {
    if &header.magic != STATIC_MESHES_MAGIC {
        return Err(invalid_data(format!(
            "static_meshes magic mismatch: expected {:?}, found {:?}",
            String::from_utf8_lossy(STATIC_MESHES_MAGIC),
            String::from_utf8_lossy(&header.magic)
        )));
    }
    if header.version != STATIC_MESHES_VERSION {
        return Err(invalid_data(format!(
            "static_meshes version mismatch: expected {STATIC_MESHES_VERSION}, found {}",
            header.version
        )));
    }
    for (name, actual, expected) in [
        ("header size", header.header_size, HEADER_SIZE as u32),
        ("static record size", header.static_record_size, STATIC_RECORD_SIZE as u32),
        ("subset record size", header.subset_record_size, SUBSET_RECORD_SIZE as u32),
        (
            "component record size",
            header.component_record_size,
            COMPONENT_RECORD_SIZE as u32,
        ),
        ("regular vertex stride", header.vertex_stride, STATIC_VERTEX_STRIDE as u32),
        ("grass vertex stride", header.grass_vertex_stride, GRASS_VERTEX_STRIDE as u32),
        ("index element size", header.index_element_size, INDEX_ELEMENT_SIZE as u32),
    ] {
        if actual != expected {
            return Err(invalid_data(format!(
                "static_meshes {name} mismatch: expected {expected}, found {actual}"
            )));
        }
    }
    if header.reserved != 0 {
        return Err(invalid_data(format!(
            "static_meshes reserved header field must be zero, found {}",
            header.reserved
        )));
    }
    Ok(())
}

fn decode_texture_path(
    bytes: &[u8],
    texture_blob: &Range<usize>,
    subset_index: u32,
    record: SubsetRecord,
) -> io::Result<Box<str>> {
    if record.texture_path_length == 0 {
        return Err(invalid_data(format!(
            "static_meshes subset {subset_index} has an empty texture path"
        )));
    }
    let total_size = u64::from(record.texture_path_length)
        .checked_add(1)
        .ok_or_else(|| invalid_data(format!("static_meshes subset {subset_index} texture range overflows u64")))?;
    let range = checked_file_range(record.texture_path_offset, total_size, bytes.len(), "subset texture path")?;
    if range.start < texture_blob.start || range.end > texture_blob.end {
        return Err(invalid_data(format!(
            "static_meshes subset {subset_index} texture path lies outside the texture blob"
        )));
    }
    let terminator = range.end - 1;
    if bytes[terminator] != 0 {
        return Err(invalid_data(format!(
            "static_meshes subset {subset_index} texture path is not NUL terminated"
        )));
    }
    let path_bytes = &bytes[range.start..terminator];
    if path_bytes.contains(&0) {
        return Err(invalid_data(format!(
            "static_meshes subset {subset_index} texture path contains an interior NUL"
        )));
    }
    let path = std::str::from_utf8(path_bytes).map_err(|error| {
        invalid_data(format!(
            "static_meshes subset {subset_index} texture path is not UTF-8: {error}"
        ))
    })?;
    Ok(Box::<str>::from(path))
}

fn validate_components(components: &[ComponentRecord], triangle_count: u32, subset_index: u32) -> io::Result<()> {
    if components.is_empty() {
        return Ok(());
    }
    let mut expected_triangle = 0_u32;
    for (component_index, component) in components.iter().enumerate() {
        if component.first_triangle != expected_triangle {
            return Err(invalid_data(format!(
                "static_meshes subset {subset_index} component {component_index} begins at triangle {}, expected {expected_triangle}",
                component.first_triangle
            )));
        }
        if component.triangle_count == 0 {
            return Err(invalid_data(format!(
                "static_meshes subset {subset_index} component {component_index} has zero triangles"
            )));
        }
        expected_triangle = component
            .first_triangle
            .checked_add(component.triangle_count)
            .ok_or_else(|| {
                invalid_data(format!(
                    "static_meshes subset {subset_index} component {component_index} triangle range overflows u32"
                ))
            })?;
        if expected_triangle > triangle_count {
            return Err(invalid_data(format!(
                "static_meshes subset {subset_index} component {component_index} exceeds triangle count {triangle_count}"
            )));
        }
        if !component.radius.is_finite() || component.radius < 0.0 {
            return Err(invalid_data(format!(
                "static_meshes subset {subset_index} component {component_index} radius must be finite and non-negative"
            )));
        }
        if static_type_from_u8(component.classification).is_none() {
            return Err(invalid_data(format!(
                "static_meshes subset {subset_index} component {component_index} has invalid classification {}",
                component.classification
            )));
        }
        if component.reserved != [0; 3] {
            return Err(invalid_data(format!(
                "static_meshes subset {subset_index} component {component_index} reserved bytes must be zero"
            )));
        }
    }
    if expected_triangle != triangle_count {
        return Err(invalid_data(format!(
            "static_meshes subset {subset_index} components cover {expected_triangle} of {triangle_count} triangles"
        )));
    }
    Ok(())
}

fn validate_horizon_footprint(footprint: HorizonFootprint, subset_index: u32) -> io::Result<()> {
    if usize::from(footprint.vertex_count) > HORIZON_FOOTPRINT_MAX_VERTS {
        return Err(invalid_data(format!(
            "static_meshes subset {subset_index} horizon footprint has {} vertices, maximum is {HORIZON_FOOTPRINT_MAX_VERTS}",
            footprint.vertex_count
        )));
    }
    if footprint.padding != [0; 3] {
        return Err(invalid_data(format!(
            "static_meshes subset {subset_index} horizon footprint padding must be zero"
        )));
    }
    if footprint.footprint_xy[usize::from(footprint.vertex_count)..]
        .iter()
        .any(|point| *point != [0.0; 2])
    {
        return Err(invalid_data(format!(
            "static_meshes subset {subset_index} horizon footprint has nonzero unused vertices"
        )));
    }
    Ok(())
}

fn static_type_from_u32(value: u32) -> Option<StaticType> {
    u8::try_from(value).ok().and_then(static_type_from_u8)
}

fn static_type_from_u8(value: u8) -> Option<StaticType> {
    match value {
        0 => Some(StaticType::StaticAuto),
        1 => Some(StaticType::StaticNear),
        2 => Some(StaticType::StaticFar),
        3 => Some(StaticType::StaticVeryFar),
        4 => Some(StaticType::StaticGrass),
        5 => Some(StaticType::StaticTree),
        6 => Some(StaticType::StaticBuilding),
        _ => None,
    }
}

fn checked_product(count: u32, stride: usize, label: &str) -> io::Result<u64> {
    u64::from(count)
        .checked_mul(stride as u64)
        .ok_or_else(|| invalid_data(format!("{label} overflows u64")))
}

fn require_size(actual: u64, expected: u64, label: &str) -> io::Result<()> {
    if actual != expected {
        return Err(invalid_data(format!(
            "static_meshes {label} size mismatch: expected {expected}, found {actual}"
        )));
    }
    Ok(())
}

fn checked_file_range(offset: u64, size: u64, file_len: usize, label: &str) -> io::Result<Range<usize>> {
    let end = offset
        .checked_add(size)
        .ok_or_else(|| invalid_data(format!("static_meshes {label} range overflows u64")))?;
    let start = usize::try_from(offset).map_err(|_| invalid_data(format!("static_meshes {label} offset exceeds usize")))?;
    let end = usize::try_from(end).map_err(|_| invalid_data(format!("static_meshes {label} end exceeds usize")))?;
    if end > file_len {
        return Err(invalid_data(format!(
            "static_meshes {label} range {start}..{end} exceeds file length {file_len}"
        )));
    }
    Ok(start..end)
}

fn require_ordered_ranges<const N: usize>(ranges: [(&str, &Range<usize>); N], file_len: usize) -> io::Result<()> {
    for pair in ranges.windows(2) {
        let (left_name, left) = pair[0];
        let (right_name, right) = pair[1];
        if left.end > right.start {
            return Err(invalid_data(format!("static_meshes {left_name} overlaps {right_name}")));
        }
    }
    let (last_name, last) = ranges[N - 1];
    if last.end != file_len {
        return Err(invalid_data(format!(
            "static_meshes has {} trailing bytes after {last_name}",
            file_len - last.end
        )));
    }
    Ok(())
}

fn read_pod_table<'a, T: Pod + Copy>(
    bytes: &'a [u8],
    range: Range<usize>,
    count: u32,
    label: &str,
) -> io::Result<Cow<'a, [T]>> {
    let expected = checked_product(count, std::mem::size_of::<T>(), label)?;
    require_size(range.len() as u64, expected, label)?;
    let table_bytes = &bytes[range];
    Ok(match bytemuck::try_cast_slice(table_bytes) {
        Ok(table) => Cow::Borrowed(table),
        Err(_) => Cow::Owned(
            table_bytes
                .chunks_exact(std::mem::size_of::<T>())
                .map(bytemuck::pod_read_unaligned::<T>)
                .collect(),
        ),
    })
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// Compile-time layout assertions ensuring that the Rust structures match the
/// byte layout, alignment, sizes, and field offsets of the C++ MGE-XE
/// on-disk binary format exactly.
const _: () = {
    assert!(std::mem::size_of::<Vec3>() == 12);
    assert!(std::mem::align_of::<Vec3>() == 4);

    assert!(std::mem::size_of::<BoundingSphere>() == 16);
    assert!(std::mem::align_of::<BoundingSphere>() == 4);
    assert!(std::mem::offset_of!(BoundingSphere, radius) == 0);
    assert!(std::mem::offset_of!(BoundingSphere, center) == 4);

    assert!(std::mem::size_of::<BoundingBox>() == 24);
    assert!(std::mem::align_of::<BoundingBox>() == 4);
    assert!(std::mem::offset_of!(BoundingBox, min) == 0);
    assert!(std::mem::offset_of!(BoundingBox, max) == 12);

    assert!(std::mem::size_of::<HorizonFootprint>() == 56);
    assert!(std::mem::align_of::<HorizonFootprint>() == 4);
    assert!(std::mem::offset_of!(HorizonFootprint, max_z) == 0);
    assert!(std::mem::offset_of!(HorizonFootprint, vertex_count) == 4);
    assert!(std::mem::offset_of!(HorizonFootprint, padding) == 5);
    assert!(std::mem::offset_of!(HorizonFootprint, footprint_xy) == 8);

    assert!(std::mem::size_of::<ComponentRecord>() == COMPONENT_RECORD_SIZE);
    assert!(std::mem::align_of::<ComponentRecord>() == 4);
    assert!(std::mem::offset_of!(ComponentRecord, first_triangle) == 0);
    assert!(std::mem::offset_of!(ComponentRecord, triangle_count) == 4);
    assert!(std::mem::offset_of!(ComponentRecord, radius) == 8);
    assert!(std::mem::offset_of!(ComponentRecord, classification) == 12);
    assert!(std::mem::offset_of!(ComponentRecord, reserved) == 13);

    assert!(STATIC_VERTEX_STRIDE == 28);
    assert!(std::mem::size_of::<PackedVertex>() == STATIC_VERTEX_STRIDE);
    assert!(std::mem::align_of::<PackedVertex>() == 2);
    assert!(std::mem::offset_of!(PackedVertex, position) == 0);
    assert!(std::mem::offset_of!(PackedVertex, normal) == 8);
    assert!(std::mem::offset_of!(PackedVertex, color) == 12);
    assert!(std::mem::offset_of!(PackedVertex, uv) == 16);
    assert!(std::mem::offset_of!(PackedVertex, uv_bound) == 20);

    assert!(GRASS_VERTEX_STRIDE == 20);
    assert!(std::mem::size_of::<PackedGrassVertex>() == 20);
    assert!(std::mem::align_of::<PackedGrassVertex>() == 2);
    assert!(std::mem::offset_of!(PackedGrassVertex, position) == 0);
    assert!(std::mem::offset_of!(PackedGrassVertex, normal) == 8);
    assert!(std::mem::offset_of!(PackedGrassVertex, color) == 12);
    assert!(std::mem::offset_of!(PackedGrassVertex, uv) == 16);
    assert!(std::mem::offset_of!(PackedVertex, position) == std::mem::offset_of!(PackedGrassVertex, position));
    assert!(std::mem::offset_of!(PackedVertex, normal) == std::mem::offset_of!(PackedGrassVertex, normal));
    assert!(std::mem::offset_of!(PackedVertex, color) == std::mem::offset_of!(PackedGrassVertex, color));
    assert!(std::mem::offset_of!(PackedVertex, uv) == std::mem::offset_of!(PackedGrassVertex, uv));

    assert!(std::mem::size_of::<StaticMeshesFileHeader>() == HEADER_SIZE);
    assert!(std::mem::align_of::<StaticMeshesFileHeader>() == 8);
    assert!(std::mem::offset_of!(StaticMeshesFileHeader, magic) == 0);
    assert!(std::mem::offset_of!(StaticMeshesFileHeader, version) == 8);
    assert!(std::mem::offset_of!(StaticMeshesFileHeader, header_size) == 12);
    assert!(std::mem::offset_of!(StaticMeshesFileHeader, static_record_size) == 16);
    assert!(std::mem::offset_of!(StaticMeshesFileHeader, subset_record_size) == 20);
    assert!(std::mem::offset_of!(StaticMeshesFileHeader, vertex_stride) == 24);
    assert!(std::mem::offset_of!(StaticMeshesFileHeader, index_element_size) == 28);
    assert!(std::mem::offset_of!(StaticMeshesFileHeader, static_count) == 32);
    assert!(std::mem::offset_of!(StaticMeshesFileHeader, subset_count) == 36);
    assert!(std::mem::offset_of!(StaticMeshesFileHeader, static_table_offset) == 40);
    assert!(std::mem::offset_of!(StaticMeshesFileHeader, static_table_size) == 48);
    assert!(std::mem::offset_of!(StaticMeshesFileHeader, subset_table_offset) == 56);
    assert!(std::mem::offset_of!(StaticMeshesFileHeader, subset_table_size) == 64);
    assert!(std::mem::offset_of!(StaticMeshesFileHeader, texture_blob_offset) == 72);
    assert!(std::mem::offset_of!(StaticMeshesFileHeader, texture_blob_size) == 80);
    assert!(std::mem::offset_of!(StaticMeshesFileHeader, geometry_blob_offset) == 88);
    assert!(std::mem::offset_of!(StaticMeshesFileHeader, geometry_blob_size) == 96);
    assert!(std::mem::offset_of!(StaticMeshesFileHeader, grass_vertex_stride) == 104);
    assert!(std::mem::offset_of!(StaticMeshesFileHeader, reserved) == 108);
    assert!(std::mem::offset_of!(StaticMeshesFileHeader, component_table_offset) == 112);
    assert!(std::mem::offset_of!(StaticMeshesFileHeader, component_table_size) == 120);
    assert!(std::mem::offset_of!(StaticMeshesFileHeader, component_record_size) == 128);
    assert!(std::mem::offset_of!(StaticMeshesFileHeader, component_count) == 132);

    assert!(std::mem::size_of::<StaticRecord>() == STATIC_RECORD_SIZE);
    assert!(std::mem::align_of::<StaticRecord>() == 4);
    assert!(std::mem::offset_of!(StaticRecord, static_type) == 0);
    assert!(std::mem::offset_of!(StaticRecord, sphere) == 4);
    assert!(std::mem::offset_of!(StaticRecord, aabb) == 20);
    assert!(std::mem::offset_of!(StaticRecord, first_subset_index) == 44);
    assert!(std::mem::offset_of!(StaticRecord, subset_count) == 48);

    assert!(std::mem::size_of::<SubsetRecord>() == SUBSET_RECORD_SIZE);
    assert!(std::mem::align_of::<SubsetRecord>() == 8);
    assert!(std::mem::offset_of!(SubsetRecord, sphere) == 0);
    assert!(std::mem::offset_of!(SubsetRecord, aabb) == 16);
    assert!(std::mem::offset_of!(SubsetRecord, texture_path_offset) == 40);
    assert!(std::mem::offset_of!(SubsetRecord, vertex_offset) == 48);
    assert!(std::mem::offset_of!(SubsetRecord, index_offset) == 56);
    assert!(std::mem::offset_of!(SubsetRecord, vertex_count) == 64);
    assert!(std::mem::offset_of!(SubsetRecord, triangle_count) == 68);
    assert!(std::mem::offset_of!(SubsetRecord, flags) == 72);
    assert!(std::mem::offset_of!(SubsetRecord, texture_path_length) == 76);
    assert!(std::mem::offset_of!(SubsetRecord, horizon_footprint) == 80);
    assert!(std::mem::offset_of!(SubsetRecord, first_component_index) == 136);
    assert!(std::mem::offset_of!(SubsetRecord, component_count) == 140);
};

#[cfg(test)]
mod tests;
