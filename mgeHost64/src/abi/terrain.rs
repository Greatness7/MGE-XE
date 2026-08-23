use super::{
    BoundingSphere, ByteReadError, ByteReader, D3dxVector3, SIZE_OF_TERRAIN_VERT, TERRAIN_FILE_INDEX_FORMAT_AUTO,
    TERRAIN_FILE_MAGIC, TERRAIN_FILE_VERSION,
};
use bytemuck::{Pod, Zeroable};
use thiserror::Error;

pub const TERRAIN_HEADER_SIZE: usize = 116;
pub const TERRAIN_MESH_HEADER_SIZE: usize = 48;

pub const TERRAIN_FILE_NAME: &str = "terrain.bin";

/// One vertex stored in `terrain.bin`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct TerrainVertex {
    /// World-space position.
    pub position: D3dxVector3,
    /// Bias-encoded normal bytes for `D3DDECLTYPE_UBYTE4N`.
    pub normal: [u8; 4],
    /// Little-endian D3DCOLOR value (`0xAARRGGBB` logically, `[B,G,R,A]` on disk).
    pub color: u32,
}

/// Fixed-size `terrain.bin` header read as a POD block in little-endian file order.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct TerrainFileHeader {
    pub magic: [u8; 8],
    pub version: u32,
    pub cell_size: f32,
    pub patch_size: f32,
    pub origin_cell: [i32; 2],
    pub cell_size_xy: [u32; 2],
    pub world_origin: [f32; 2],
    pub world_size: [f32; 2],
    pub atlas_size: u32,
    pub logical_tile_size: u32,
    pub gutter_size: u32,
    pub physical_tile_size: u32,
    pub tiles_per_row: u32,
    pub atlas_max_lod: u32,
    pub material_size_xy: [u32; 2],
    pub pattern_count: u32,
    pub pattern_tile_size: u32,
    pub pattern_gutter_size: u32,
    pub pattern_physical_size: u32,
    pub patterns_per_row: u32,
    pub vertex_stride: u32,
    pub file_index_format: u32,
    pub mesh_count: u32,
}

/// Per-mesh header stored immediately before one mesh payload.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct TerrainMeshHeader {
    pub bounding_sphere_radius: f32,
    pub bounding_sphere_center: D3dxVector3,
    pub bounding_box_min: D3dxVector3,
    pub bounding_box_max: D3dxVector3,
    pub vertex_count: u32,
    pub triangle_count: u32,
}

impl TerrainMeshHeader {
    /// Returns the runtime-style bounding sphere reconstructed from the serialized order.
    pub fn bounding_sphere(self) -> BoundingSphere {
        BoundingSphere {
            center: self.bounding_sphere_center,
            radius: self.bounding_sphere_radius,
        }
    }

    /// Returns `true` when this mesh's triangle indices are stored on disk (and
    /// uploaded) as 16-bit values, i.e. its vertices are addressable by a `u16`
    /// index buffer.
    ///
    /// This is the index-width predicate; it matches the generator's
    /// `mesh_uses_u16_indices` and the C++ client's `useIndex16` so the on-disk
    /// width and the uploaded GPU index-buffer width agree by construction.
    pub fn uses_u16_indices(self) -> bool {
        self.vertex_count <= 0xFFFF
    }

    /// Returns the serialized vertex byte count.
    pub fn vertex_data_bytes(self) -> Result<usize, TerrainFormatError> {
        checked_product(
            self.vertex_count as usize,
            SIZE_OF_TERRAIN_VERT as usize,
            "terrain vertex payload",
        )
    }

    /// Returns the serialized triangle-index byte count, using the per-mesh
    /// on-disk index width (6 bytes per triangle when [`Self::uses_u16_indices`],
    /// else 12).
    pub fn index_data_bytes(self) -> Result<usize, TerrainFormatError> {
        let bytes_per_triangle = if self.uses_u16_indices() { 6 } else { 12 };
        checked_product(self.triangle_count as usize, bytes_per_triangle, "terrain triangle payload")
    }
}

/// Byte layout of one mesh payload inside `terrain.bin`.
#[derive(Clone, Debug)]
pub struct TerrainMeshLayout {
    pub header: TerrainMeshHeader,
    #[cfg(test)]
    pub vertex_data_offset: usize,
    pub vertex_data_size: usize,
    #[cfg(test)]
    pub index_data_offset: usize,
    pub index_data_size: usize,
}

impl TerrainMeshLayout {
    /// Lazily decodes vertices from this mesh payload.
    ///
    /// Test-only oracle for the generated occlusion asset; production never scans terrain vertices.
    #[cfg(test)]
    pub fn iter_vertices<'a>(
        &self,
        bytes: &'a [u8],
    ) -> Result<impl Iterator<Item = TerrainVertex> + 'a, TerrainFormatError> {
        let stride = SIZE_OF_TERRAIN_VERT as usize;
        let size = self.header.vertex_data_bytes()?;
        let slice =
            bytes
                .get(self.vertex_data_offset..self.vertex_data_offset + size)
                .ok_or(TerrainFormatError::UnexpectedEof {
                    offset: self.vertex_data_offset,
                    needed: size,
                    remaining: bytes.len().saturating_sub(self.vertex_data_offset),
                })?;
        Ok(slice.chunks_exact(stride).map(bytemuck::pod_read_unaligned))
    }
}

/// Parsed `terrain.bin` header plus per-mesh payload offsets.
#[derive(Clone, Debug)]
pub struct TerrainFileLayout {
    pub header: TerrainFileHeader,
    pub meshes: Vec<TerrainMeshLayout>,
}

#[derive(Debug, Error, PartialEq)]
pub enum TerrainFormatError {
    #[error("unexpected EOF at byte {offset}: needed {needed} more bytes but only {remaining} remain")]
    UnexpectedEof { offset: usize, needed: usize, remaining: usize },
    #[error("terrain.bin magic must be XELAND02, got {0:?}")]
    InvalidMagic([u8; 8]),
    #[error("terrain.bin version must be 2, got {0}")]
    UnsupportedVersion(u32),
    #[error("terrain.bin vertex_stride must be 20, got {0}")]
    UnsupportedVertexStride(u32),
    #[error("terrain.bin file_index_format must be 2, got {0}")]
    UnsupportedIndexFormat(u32),
    #[error("terrain.bin region layout is invalid: {0}")]
    InvalidRegion(&'static str),
    #[error("terrain.bin source-atlas layout is invalid: {0}")]
    InvalidAtlas(&'static str),
    #[error("terrain.bin material map layout is invalid: {0}")]
    InvalidMaterialLayout(&'static str),
    #[error("terrain.bin blend-pattern layout is invalid: {0}")]
    InvalidPatternLayout(&'static str),
    #[error("terrain.bin payload size overflow while computing {0}")]
    IntegerOverflow(&'static str),
    #[error("terrain.bin has {total} bytes but the mesh payloads consume only {consumed}")]
    TrailingBytes { consumed: usize, total: usize },
}

impl From<ByteReadError> for TerrainFormatError {
    fn from(error: ByteReadError) -> Self {
        Self::UnexpectedEof {
            offset: error.offset,
            needed: error.needed,
            remaining: error.remaining,
        }
    }
}

fn checked_product(lhs: usize, rhs: usize, label: &'static str) -> Result<usize, TerrainFormatError> {
    lhs.checked_mul(rhs).ok_or(TerrainFormatError::IntegerOverflow(label))
}

fn expected_atlas_max_lod(logical_tile_size: u32) -> Option<u32> {
    match logical_tile_size {
        64 => Some(0),
        128 => Some(1),
        256 => Some(2),
        512 => Some(3),
        _ => None,
    }
}

fn validate_header(header: &TerrainFileHeader) -> Result<(), TerrainFormatError> {
    if header.magic != TERRAIN_FILE_MAGIC {
        return Err(TerrainFormatError::InvalidMagic(header.magic));
    }
    if header.version != TERRAIN_FILE_VERSION {
        return Err(TerrainFormatError::UnsupportedVersion(header.version));
    }
    if header.vertex_stride != SIZE_OF_TERRAIN_VERT {
        return Err(TerrainFormatError::UnsupportedVertexStride(header.vertex_stride));
    }
    if header.file_index_format != TERRAIN_FILE_INDEX_FORMAT_AUTO {
        return Err(TerrainFormatError::UnsupportedIndexFormat(header.file_index_format));
    }
    if !(header.cell_size.is_finite() && header.cell_size > 0.0) {
        return Err(TerrainFormatError::InvalidRegion("cell_size must be finite and positive"));
    }
    if !(header.patch_size.is_finite() && header.patch_size > 0.0) {
        return Err(TerrainFormatError::InvalidRegion("patch_size must be finite and positive"));
    }
    if header.cell_size_xy[0] == 0 || header.cell_size_xy[1] == 0 {
        return Err(TerrainFormatError::InvalidRegion("cell_size_xy must be non-zero"));
    }

    let patches_per_cell = header.cell_size / header.patch_size;
    let rounded_patches = patches_per_cell.round();
    if !rounded_patches.is_finite() || rounded_patches <= 0.0 || (patches_per_cell - rounded_patches).abs() > 0.001 {
        return Err(TerrainFormatError::InvalidRegion(
            "cell_size must be an integer multiple of patch_size",
        ));
    }
    let patches_per_cell_u32 = rounded_patches as u32;

    let expected_world_origin = [
        header.origin_cell[0] as f32 * header.cell_size,
        header.origin_cell[1] as f32 * header.cell_size,
    ];
    if (header.world_origin[0] - expected_world_origin[0]).abs() > 0.25
        || (header.world_origin[1] - expected_world_origin[1]).abs() > 0.25
    {
        return Err(TerrainFormatError::InvalidRegion(
            "world_origin must equal origin_cell * cell_size",
        ));
    }

    let expected_world_size = [
        header.cell_size_xy[0] as f32 * header.cell_size,
        header.cell_size_xy[1] as f32 * header.cell_size,
    ];
    if (header.world_size[0] - expected_world_size[0]).abs() > 0.25
        || (header.world_size[1] - expected_world_size[1]).abs() > 0.25
    {
        return Err(TerrainFormatError::InvalidRegion(
            "world_size must equal cell_size_xy * cell_size",
        ));
    }

    let expected_material_size = [
        header.cell_size_xy[0]
            .checked_mul(patches_per_cell_u32)
            .ok_or(TerrainFormatError::IntegerOverflow("material_size_xy.x"))?,
        header.cell_size_xy[1]
            .checked_mul(patches_per_cell_u32)
            .ok_or(TerrainFormatError::IntegerOverflow("material_size_xy.y"))?,
    ];
    if header.material_size_xy[0] == 0
        || header.material_size_xy[1] == 0
        || header.material_size_xy != expected_material_size
    {
        return Err(TerrainFormatError::InvalidMaterialLayout(
            "material_size_xy must match the patch grid",
        ));
    }

    if header.atlas_size == 0
        || header.logical_tile_size == 0
        || header.tiles_per_row == 0
        || header.physical_tile_size != header.logical_tile_size + 2 * header.gutter_size
        || header
            .tiles_per_row
            .checked_mul(header.physical_tile_size)
            .ok_or(TerrainFormatError::IntegerOverflow("atlas row width"))?
            > header.atlas_size
    {
        return Err(TerrainFormatError::InvalidAtlas(
            "atlas_size, tile sizes, or tiles_per_row are inconsistent",
        ));
    }
    let expected_lod = expected_atlas_max_lod(header.logical_tile_size).ok_or(TerrainFormatError::InvalidAtlas(
        "logical_tile_size must be one of 64, 128, 256, or 512",
    ))?;
    if header.atlas_max_lod != expected_lod {
        return Err(TerrainFormatError::InvalidAtlas(
            "atlas_max_lod does not match logical_tile_size",
        ));
    }

    if header.pattern_count == 0
        || header.pattern_count > 256
        || header.pattern_tile_size == 0
        || header.patterns_per_row == 0
        || header.pattern_physical_size != header.pattern_tile_size + 2 * header.pattern_gutter_size
    {
        return Err(TerrainFormatError::InvalidPatternLayout(
            "pattern_count or pattern tile sizes are inconsistent",
        ));
    }

    Ok(())
}

fn read_header(reader: &mut ByteReader<'_>) -> Result<TerrainFileHeader, TerrainFormatError> {
    Ok(bytemuck::pod_read_unaligned(reader.read_exact_bytes(TERRAIN_HEADER_SIZE)?))
}

fn read_mesh_header(reader: &mut ByteReader<'_>) -> Result<TerrainMeshHeader, TerrainFormatError> {
    Ok(bytemuck::pod_read_unaligned(
        reader.read_exact_bytes(TERRAIN_MESH_HEADER_SIZE)?,
    ))
}

/// Parses `terrain.bin` and records the exact byte offsets of each mesh payload.
pub fn parse_terrain_file_layout(bytes: &[u8]) -> Result<TerrainFileLayout, TerrainFormatError> {
    let mut reader = ByteReader::new(bytes);
    let header = read_header(&mut reader)?;
    validate_header(&header)?;

    let mut meshes = Vec::with_capacity(header.mesh_count as usize);
    for _ in 0..header.mesh_count {
        let mesh_header = read_mesh_header(&mut reader)?;
        let vertex_data_offset = reader.offset();
        #[cfg(not(test))]
        let _ = vertex_data_offset;
        let vertex_data_size = mesh_header.vertex_data_bytes()?;
        reader.skip(vertex_data_size)?;
        let index_data_offset = reader.offset();
        #[cfg(not(test))]
        let _ = index_data_offset;
        let index_data_size = mesh_header.index_data_bytes()?;
        reader.skip(index_data_size)?;
        meshes.push(TerrainMeshLayout {
            header: mesh_header,
            #[cfg(test)]
            vertex_data_offset,
            vertex_data_size,
            #[cfg(test)]
            index_data_offset,
            index_data_size,
        });
    }

    if reader.offset() != bytes.len() {
        return Err(TerrainFormatError::TrailingBytes {
            consumed: reader.offset(),
            total: bytes.len(),
        });
    }

    Ok(TerrainFileLayout { header, meshes })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_u32(bytes: &mut Vec<u8>, value: u32) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn push_i32(bytes: &mut Vec<u8>, value: i32) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn push_f32(bytes: &mut Vec<u8>, value: f32) {
        push_u32(bytes, value.to_bits());
    }

    fn push_vec3(bytes: &mut Vec<u8>, value: [f32; 3]) {
        push_f32(bytes, value[0]);
        push_f32(bytes, value[1]);
        push_f32(bytes, value[2]);
    }

    fn requires_32_bit_index_buffer(header: TerrainMeshHeader) -> bool {
        !header.uses_u16_indices()
    }

    fn read_all_triangles(mesh: &TerrainMeshLayout, bytes: &[u8]) -> Result<Vec<[u32; 3]>, TerrainFormatError> {
        let stride = if mesh.header.uses_u16_indices() { 6 } else { 12 };
        let size = mesh.header.index_data_bytes()?;
        let slice =
            bytes
                .get(mesh.index_data_offset..mesh.index_data_offset + size)
                .ok_or(TerrainFormatError::UnexpectedEof {
                    offset: mesh.index_data_offset,
                    needed: size,
                    remaining: bytes.len().saturating_sub(mesh.index_data_offset),
                })?;
        let result = if mesh.header.uses_u16_indices() {
            slice
                .chunks_exact(stride)
                .map(|chunk| {
                    let triangle: [u16; 3] = bytemuck::pod_read_unaligned(chunk);
                    [u32::from(triangle[0]), u32::from(triangle[1]), u32::from(triangle[2])]
                })
                .collect()
        } else {
            slice
                .chunks_exact(stride)
                .map(bytemuck::pod_read_unaligned::<[u32; 3]>)
                .collect()
        };
        Ok(result)
    }

    fn build_minimal_fixture() -> Vec<u8> {
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&TERRAIN_FILE_MAGIC);
        push_u32(&mut bytes, TERRAIN_FILE_VERSION);
        push_f32(&mut bytes, 8192.0);
        push_f32(&mut bytes, 512.0);
        push_i32(&mut bytes, -40);
        push_i32(&mut bytes, -32);
        push_u32(&mut bytes, 1);
        push_u32(&mut bytes, 1);
        push_f32(&mut bytes, -327_680.0);
        push_f32(&mut bytes, -262_144.0);
        push_f32(&mut bytes, 8192.0);
        push_f32(&mut bytes, 8192.0);
        push_u32(&mut bytes, 1024);
        push_u32(&mut bytes, 64);
        push_u32(&mut bytes, 4);
        push_u32(&mut bytes, 72);
        push_u32(&mut bytes, 8);
        push_u32(&mut bytes, 0);
        push_u32(&mut bytes, 16);
        push_u32(&mut bytes, 16);
        push_u32(&mut bytes, 11);
        push_u32(&mut bytes, 32);
        push_u32(&mut bytes, 2);
        push_u32(&mut bytes, 36);
        push_u32(&mut bytes, 4);
        push_u32(&mut bytes, SIZE_OF_TERRAIN_VERT);
        push_u32(&mut bytes, TERRAIN_FILE_INDEX_FORMAT_AUTO);
        push_u32(&mut bytes, 1);

        push_f32(&mut bytes, 4.0);
        push_vec3(&mut bytes, [1.0, 2.0, 3.0]);
        push_vec3(&mut bytes, [0.0, 0.0, 0.0]);
        push_vec3(&mut bytes, [2.0, 2.0, 4.0]);
        push_u32(&mut bytes, 3);
        push_u32(&mut bytes, 1);

        push_vec3(&mut bytes, [0.0, 0.0, 0.0]);
        bytes.extend_from_slice(&[128, 128, 255, 0]);
        push_u32(&mut bytes, 0xFF33_66CC);

        push_vec3(&mut bytes, [2.0, 0.0, 0.0]);
        bytes.extend_from_slice(&[255, 128, 128, 0]);
        push_u32(&mut bytes, 0xFFCC_8844);

        push_vec3(&mut bytes, [0.0, 2.0, 4.0]);
        bytes.extend_from_slice(&[128, 255, 128, 0]);
        push_u32(&mut bytes, 0xFF11_2233);

        // vertex_count (3) <= 0xFFFF, so AUTO stores u16 triangle indices.
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());

        bytes
    }

    #[test]
    fn terrain_header_and_mesh_struct_sizes_match_contract() {
        assert_eq!(std::mem::size_of::<TerrainVertex>(), SIZE_OF_TERRAIN_VERT as usize);
        assert_eq!(std::mem::size_of::<TerrainFileHeader>(), TERRAIN_HEADER_SIZE);
        assert_eq!(std::mem::size_of::<TerrainMeshHeader>(), TERRAIN_MESH_HEADER_SIZE);
    }

    #[test]
    fn minimal_fixture_layout_and_payload_parse_exactly() {
        let bytes = build_minimal_fixture();
        assert_eq!(
            bytes.len(),
            TERRAIN_HEADER_SIZE + TERRAIN_MESH_HEADER_SIZE + 3 * SIZE_OF_TERRAIN_VERT as usize + 6
        );

        let layout = parse_terrain_file_layout(&bytes).expect("fixture must parse");
        assert_eq!(layout.header.magic, TERRAIN_FILE_MAGIC);
        assert_eq!(layout.header.version, TERRAIN_FILE_VERSION);
        assert_eq!(layout.header.vertex_stride, SIZE_OF_TERRAIN_VERT);
        assert_eq!(layout.header.file_index_format, TERRAIN_FILE_INDEX_FORMAT_AUTO);
        assert_eq!(layout.header.material_size_xy, [16, 16]);
        assert_eq!(layout.meshes.len(), 1);

        let mesh = &layout.meshes[0];
        assert_eq!(mesh.vertex_data_offset, TERRAIN_HEADER_SIZE + TERRAIN_MESH_HEADER_SIZE);
        assert_eq!(mesh.vertex_data_size, 60);
        assert_eq!(mesh.index_data_offset, TERRAIN_HEADER_SIZE + TERRAIN_MESH_HEADER_SIZE + 60);
        assert_eq!(mesh.index_data_size, 6);
        assert!(!requires_32_bit_index_buffer(mesh.header));
        let sphere = mesh.header.bounding_sphere();
        assert_eq!(sphere.center.x, 1.0);
        assert_eq!(sphere.center.y, 2.0);
        assert_eq!(sphere.center.z, 3.0);
        assert_eq!(sphere.radius, 4.0);

        let vertices = mesh.iter_vertices(&bytes).expect("vertices decode").collect::<Vec<_>>();
        assert_eq!(vertices.len(), 3);
        assert_eq!(vertices[0].position.x, 0.0);
        assert_eq!(vertices[0].position.y, 0.0);
        assert_eq!(vertices[0].position.z, 0.0);
        assert_eq!(vertices[0].normal, [128, 128, 255, 0]);
        assert_eq!(vertices[0].color, 0xFF33_66CC);
        assert_eq!(vertices[1].position.x, 2.0);
        assert_eq!(vertices[1].position.y, 0.0);
        assert_eq!(vertices[1].position.z, 0.0);
        assert_eq!(vertices[1].normal, [255, 128, 128, 0]);
        assert_eq!(vertices[1].color, 0xFFCC_8844);
        assert_eq!(vertices[2].position.x, 0.0);
        assert_eq!(vertices[2].position.y, 2.0);
        assert_eq!(vertices[2].position.z, 4.0);
        assert_eq!(vertices[2].normal, [128, 255, 128, 0]);
        assert_eq!(vertices[2].color, 0xFF11_2233);
        assert_eq!(vertices[0].color.to_le_bytes(), [0xCC, 0x66, 0x33, 0xFF]);

        let triangles = read_all_triangles(mesh, &bytes).expect("triangles decode");
        assert_eq!(triangles, vec![[0, 1, 2]]);
    }

    #[test]
    fn minimal_fixture_bytes_match_expected_contract_offsets() {
        let bytes = build_minimal_fixture();
        assert_eq!(&bytes[0..8], b"XELAND02");
        assert_eq!(&bytes[104..108], &SIZE_OF_TERRAIN_VERT.to_le_bytes());
        assert_eq!(&bytes[108..112], &TERRAIN_FILE_INDEX_FORMAT_AUTO.to_le_bytes());
        assert_eq!(&bytes[112..116], &1u32.to_le_bytes());
        assert_eq!(
            &bytes[TERRAIN_HEADER_SIZE..TERRAIN_HEADER_SIZE + 16],
            &[0, 0, 128, 64, 0, 0, 128, 63, 0, 0, 0, 64, 0, 0, 64, 64]
        );
        assert_eq!(
            &bytes[TERRAIN_HEADER_SIZE + TERRAIN_MESH_HEADER_SIZE..TERRAIN_HEADER_SIZE + TERRAIN_MESH_HEADER_SIZE + 20],
            &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 128, 128, 255, 0, 0xCC, 0x66, 0x33, 0xFF]
        );
        assert_eq!(&bytes[bytes.len() - 6..], &[0, 0, 1, 0, 2, 0]);
    }

    #[test]
    fn parser_rejects_invalid_vertex_stride() {
        let mut bytes = build_minimal_fixture();
        bytes[104..108].copy_from_slice(&16u32.to_le_bytes());

        let error = parse_terrain_file_layout(&bytes).expect_err("vertex stride must be validated");
        assert_eq!(error, TerrainFormatError::UnsupportedVertexStride(16));
    }

    #[test]
    fn mesh_payload_sizes_follow_stride_and_per_mesh_index_width() {
        // Narrow mesh: vertex_count <= 0xFFFF stores u16 triangle indices (6 bytes/tri).
        let narrow = TerrainMeshHeader {
            vertex_count: 4,
            triangle_count: 2,
            ..TerrainMeshHeader::default()
        };
        assert_eq!(narrow.vertex_data_bytes().expect("vertex payload size"), 80);
        assert_eq!(narrow.index_data_bytes().expect("index payload size"), 12);

        // Wide mesh: vertex_count > 0xFFFF stores u32 triangle indices (12 bytes/tri).
        let wide = TerrainMeshHeader {
            vertex_count: 0x1_0000,
            triangle_count: 2,
            ..TerrainMeshHeader::default()
        };
        assert_eq!(wide.index_data_bytes().expect("index payload size"), 24);
    }

    #[test]
    fn index_width_follows_vertex_count_only() {
        // vertex_count <= 0xFFFF -> u16 indices, no 32-bit index buffer required.
        assert!(!requires_32_bit_index_buffer(TerrainMeshHeader {
            vertex_count: 0xFFFF,
            triangle_count: 1,
            ..TerrainMeshHeader::default()
        }));
        // vertex_count > 0xFFFF -> u32 indices.
        assert!(requires_32_bit_index_buffer(TerrainMeshHeader {
            vertex_count: 0x1_0000,
            triangle_count: 1,
            ..TerrainMeshHeader::default()
        }));
        // triangle_count does NOT affect index width: a u16 index buffer addresses
        // 65536 vertices regardless of how many triangles reference them.
        assert!(!requires_32_bit_index_buffer(TerrainMeshHeader {
            vertex_count: 1,
            triangle_count: 0x1_0000,
            ..TerrainMeshHeader::default()
        }));
    }
}
