//! MGE-XE `terrain.bin` terrain structures and explicit byte-level serializers.
//!
//! # `terrain.bin` ABI note
//!
//! All scalar values are serialized in little-endian order. `terrain.bin` always
//! begins with this fixed-size 116-byte header:
//!
//! | Offset | Size | Field | Type | Validation |
//! | --- | ---: | --- | --- | --- |
//! | 0 | 8 | `magic` | `[u8; 8]` | must equal `XELAND02` |
//! | 8 | 4 | `version` | `u32` | must equal `2` |
//! | 12 | 4 | `cell_size` | `f32` | generator-controlled |
//! | 16 | 4 | `patch_size` | `f32` | generator-controlled |
//! | 20 | 8 | `origin_cell` | `[i32; 2]` | generator-controlled |
//! | 28 | 8 | `cell_size_xy` | `[u32; 2]` | generator-controlled |
//! | 36 | 8 | `world_origin` | `[f32; 2]` | generator-controlled |
//! | 44 | 8 | `world_size` | `[f32; 2]` | generator-controlled |
//! | 52 | 4 | `atlas_size` | `u32` | generator-controlled |
//! | 56 | 4 | `logical_tile_size` | `u32` | generator-controlled |
//! | 60 | 4 | `gutter_size` | `u32` | generator-controlled |
//! | 64 | 4 | `physical_tile_size` | `u32` | generator-controlled |
//! | 68 | 4 | `tiles_per_row` | `u32` | generator-controlled |
//! | 72 | 4 | `atlas_max_lod` | `u32` | generator-controlled |
//! | 76 | 8 | `material_size_xy` | `[u32; 2]` | generator-controlled |
//! | 84 | 4 | `pattern_count` | `u32` | generator-controlled |
//! | 88 | 4 | `pattern_tile_size` | `u32` | generator-controlled |
//! | 92 | 4 | `pattern_gutter_size` | `u32` | generator-controlled |
//! | 96 | 4 | `pattern_physical_size` | `u32` | generator-controlled |
//! | 100 | 4 | `patterns_per_row` | `u32` | generator-controlled |
//! | 104 | 4 | `vertex_stride` | `u32` | must equal `20` |
//! | 108 | 4 | `file_index_format` | `u32` | must equal `2` (per-mesh `u16`/`u32` inferred from `vertex_count`) |
//! | 112 | 4 | `mesh_count` | `u32` | number of repeated mesh payloads |
//!
//! The mesh payload then repeats `mesh_count` times with no padding:
//!
//! | Relative offset | Size | Field | Type | Validation |
//! | --- | ---: | --- | --- | --- |
//! | 0 | 16 | `bounding_sphere` | `BoundingSphere` | `radius: f32`, then `center: [f32; 3]` |
//! | 16 | 24 | `bounding_box` | `BoundingBox` | `min: [f32; 3]`, then `max: [f32; 3]` |
//! | 40 | 4 | `vertex_count` | `u32` | number of 20-byte vertices |
//! | 44 | 4 | `triangle_count` | `u32` | number of triangles (index width per `file_index_format`) |
//! | 48 | `vertex_count * 20` | `vertices` | `TerrainVertex[]` | tightly packed |
//! | 48 + vertices | `triangle_count * 6` or `* 12` | `triangles` | `[u16; 3][]` / `[u32; 3][]` | width per `file_index_format` |
//!
//! `TerrainVertex` is exactly 20 bytes:
//!
//! | Offset | Size | Field | Type | Encoding |
//! | --- | ---: | --- | --- | --- |
//! | 0 | 12 | `position` | `[f32; 3]` | world-space coordinates |
//! | 12 | 4 | `normal` | `[u8; 4]` | `UBYTE4N`, bytes from `round(clamp(n * 0.5 + 0.5, 0, 1) * 255)` |
//! | 16 | 4 | `color` | `[u8; 4]` | D3D9 `D3DCOLOR`, raw bytes `[B, G, R, A]` |
//!
//! Triangle indices are stored per mesh at the narrowest width that addresses the
//! mesh's vertices: little-endian `u16` triples when `vertex_count <= 0xFFFF`, else
//! `u32` triples. The on-disk width therefore matches the runtime GPU index-buffer
//! width by construction, so loaders copy indices without a narrowing pass.
//!
//! Fixed companion runtime filenames are part of the terrain contract even though
//! they are not serialized into `terrain.bin`:
//!
//! - `terrain.bin`
//! - `terrain_atlas.dds`
//! - `terrain_material.dds`
//! - `terrain_material_flags.dds`
//! - `terrain_patch_albedo.dds`
//! - `terrain_blend_patterns.dds`

use std::fs::File;
use std::io::{self, Read};
use std::mem::{offset_of, size_of};
use std::path::Path;

use bytemuck::{Pod, Zeroable, cast_slice_mut};
use bytes_io::{Load, Reader, Save, Writer};
use glam::{Vec2, Vec3};

use crate::distant_statics::{BoundingBox, BoundingSphere};

/// Fixed filename for the terrain binary payload.
pub const TERRAIN_FILE_NAME: &str = "terrain.bin";
/// Fixed filename for the terrain source atlas.
pub const TERRAIN_ATLAS_FILE_NAME: &str = "terrain_atlas.dds";
/// Fixed filename for the terrain material-ID control map.
pub const TERRAIN_MATERIAL_FILE_NAME: &str = "terrain_material.dds";
/// Fixed filename for the terrain material-flags control map.
pub const TERRAIN_MATERIAL_FLAGS_FILE_NAME: &str = "terrain_material_flags.dds";
/// Fixed filename for the terrain far-field patch albedo atlas.
pub const TERRAIN_PATCH_ALBEDO_FILE_NAME: &str = "terrain_patch_albedo.dds";
/// Fixed filename for the terrain blend-pattern atlas.
pub const TERRAIN_BLEND_PATTERNS_FILE_NAME: &str = "terrain_blend_patterns.dds";
/// Eight-byte `terrain.bin` magic.
pub const TERRAIN_FILE_MAGIC: [u8; 8] = *b"XELAND02";
/// `terrain.bin` ABI version.
pub const TERRAIN_FILE_VERSION: u32 = 2;
/// Serialized terrain vertex stride in bytes.
pub const TERRAIN_VERTEX_STRIDE: u32 = 20;
/// Serialized triangle-index format code for per-mesh index width inferred from
/// `vertex_count`: `u16` triples when `vertex_count <= 0xFFFF`, else `u32` triples.
pub const TERRAIN_FILE_INDEX_FORMAT_AUTO: u32 = 2;

const TERRAIN_FILE_HEADER_BYTES: usize = 116;
const TERRAIN_MESH_HEADER_BYTES: usize = 48;

/// Returns `true` when a mesh of `vertex_count` vertices stores its triangle
/// indices as `u16` triples on disk (its vertices are addressable by a 16-bit
/// index buffer), and `false` when it stores `u32` triples. This is the single
/// on-disk index-width predicate; the runtime consumers mirror it so
/// the on-disk width matches the uploaded GPU index-buffer width by construction.
fn mesh_uses_u16_indices(vertex_count: u32) -> bool {
    vertex_count <= 0xFFFF
}

/// Serialized bytes per triangle (three indices) for a mesh of `vertex_count`
/// vertices, per [`mesh_uses_u16_indices`].
fn terrain_triangle_bytes(vertex_count: u32) -> usize {
    if mesh_uses_u16_indices(vertex_count) {
        3 * size_of::<u16>()
    } else {
        3 * size_of::<u32>()
    }
}

/// One terrain vertex stored in `terrain.bin`.
#[derive(Pod, Zeroable, Clone, Copy, Default, Debug, PartialEq)]
#[repr(C)]
pub struct TerrainVertex {
    /// World-space position.
    pub position: Vec3,
    /// Bias-encoded normal bytes for a D3D9 `UBYTE4N` input.
    pub normal: [u8; 4],
    /// D3D9 `D3DCOLOR` bytes in little-endian `[B, G, R, A]` order.
    pub color: [u8; 4],
}

/// One terrain mesh payload inside `terrain.bin`.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct TerrainMesh {
    /// Bounding sphere for the mesh chunk.
    pub bounding_sphere: BoundingSphere,
    /// Axis-aligned bounds for the mesh chunk.
    pub bounding_box: BoundingBox,
    /// Terrain vertices.
    pub vertices: Vec<TerrainVertex>,
    /// Triangle index buffer stored as `u32` triples on disk.
    pub triangles: Vec<[u32; 3]>,
}

/// Top-level terrain container stored in `terrain.bin`.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct TerrainFile {
    /// Terrain cell size in world units.
    pub cell_size: f32,
    /// Terrain patch size in world units.
    pub patch_size: f32,
    /// Minimum covered exterior cell coordinate.
    pub origin_cell: [i32; 2],
    /// Covered region size in cells on X and Y.
    pub cell_size_xy: [u32; 2],
    /// World-space origin of the covered region.
    pub world_origin: Vec2,
    /// World-space size of the covered region.
    pub world_size: Vec2,
    /// Source-atlas width and height in pixels.
    pub atlas_size: u32,
    /// Logical source tile size in pixels.
    pub logical_tile_size: u32,
    /// Source-atlas gutter width in pixels.
    pub gutter_size: u32,
    /// Physical source-atlas tile size including gutters.
    pub physical_tile_size: u32,
    /// Number of atlas tiles per row.
    pub tiles_per_row: u32,
    /// Largest physically stored source-atlas mip index.
    pub atlas_max_lod: u32,
    /// Material control-map size in texels on X and Y.
    pub material_size_xy: [u32; 2],
    /// Number of blend-pattern tiles.
    pub pattern_count: u32,
    /// Blend-pattern tile size in pixels.
    pub pattern_tile_size: u32,
    /// Blend-pattern gutter width in pixels.
    pub pattern_gutter_size: u32,
    /// Blend-pattern physical tile size including gutters.
    pub pattern_physical_size: u32,
    /// Number of blend-pattern tiles per row.
    pub patterns_per_row: u32,
    /// Terrain meshes stored in the file.
    pub meshes: Vec<TerrainMesh>,
}

impl TerrainFile {
    /// Whether every package-global header fact matches, ignoring the mesh records themselves.
    ///
    /// Record reuse rebuilds the file from carried records plus freshly computed global facts, so
    /// this check confirms the committed file was produced for the same package before its records
    /// are adopted.
    pub fn header_matches(&self, other: &Self) -> bool {
        // Destructured so that adding a header field is a compile error here rather than a silently
        // unchecked reuse condition.
        let Self {
            cell_size,
            patch_size,
            origin_cell,
            cell_size_xy,
            world_origin,
            world_size,
            atlas_size,
            logical_tile_size,
            gutter_size,
            physical_tile_size,
            tiles_per_row,
            atlas_max_lod,
            material_size_xy,
            pattern_count,
            pattern_tile_size,
            pattern_gutter_size,
            pattern_physical_size,
            patterns_per_row,
            meshes: _,
        } = self;
        cell_size == &other.cell_size
            && patch_size == &other.patch_size
            && origin_cell == &other.origin_cell
            && cell_size_xy == &other.cell_size_xy
            && world_origin == &other.world_origin
            && world_size == &other.world_size
            && atlas_size == &other.atlas_size
            && logical_tile_size == &other.logical_tile_size
            && gutter_size == &other.gutter_size
            && physical_tile_size == &other.physical_tile_size
            && tiles_per_row == &other.tiles_per_row
            && atlas_max_lod == &other.atlas_max_lod
            && material_size_xy == &other.material_size_xy
            && pattern_count == &other.pattern_count
            && pattern_tile_size == &other.pattern_tile_size
            && pattern_gutter_size == &other.pattern_gutter_size
            && pattern_physical_size == &other.pattern_physical_size
            && patterns_per_row == &other.patterns_per_row
    }
}

const _: () = {
    assert!(size_of::<Vec2>() == 8);
    assert!(size_of::<Vec3>() == 12);
    assert!(size_of::<BoundingSphere>() == 16);
    assert!(size_of::<BoundingBox>() == 24);
    assert!(size_of::<TerrainVertex>() == TERRAIN_VERTEX_STRIDE as usize);
    assert!(offset_of!(TerrainVertex, position) == 0);
    assert!(offset_of!(TerrainVertex, normal) == 12);
    assert!(offset_of!(TerrainVertex, color) == 16);
};

/// Packs a world-space normal into the D3D9 `UBYTE4N` byte representation used by
/// [`TerrainVertex::normal`].
///
/// The fourth component is currently written as `255` so the decoded `.w` lane is
/// `1.0` if runtime code ever reads it.
pub fn pack_ubyte4n_bias_normal(normal: Vec3) -> [u8; 4] {
    [
        encode_bias_normal_component(normal.x),
        encode_bias_normal_component(normal.y),
        encode_bias_normal_component(normal.z),
        u8::MAX,
    ]
}

/// Packs shader-space RGBA bytes into the little-endian raw byte order that D3D9
/// `D3DDECLTYPE_D3DCOLOR` expects in a vertex buffer.
pub const fn pack_d3dcolor_vclr(red: u8, green: u8, blue: u8, alpha: u8) -> [u8; 4] {
    [blue, green, red, alpha]
}

/// Validates the fixed `terrain.bin` header prefix in `bytes`.
///
/// This checks the minimum fixed header size plus the magic, version, vertex stride,
/// and file-index format constants. It does not parse mesh payloads.
///
/// # Errors
///
/// Returns an error if the bytes are too short or the fixed header constants do not
/// match the terrain ABI.
pub fn validate_terrain_header(bytes: &[u8]) -> io::Result<()> {
    let mut reader = Reader::new(bytes);
    TerrainFileHeader::load(&mut reader).map(|_| ())
}

/// Serializes a terrain file into explicit `terrain.bin` bytes.
///
/// # Errors
///
/// Returns an error if any vertex, triangle, or mesh count exceeds the `u32` on-disk
/// range or if the serialized size overflows `usize`.
pub fn serialize_terrain_file(file: &TerrainFile) -> io::Result<Vec<u8>> {
    let mut writer = Writer::new(Vec::with_capacity(serialized_terrain_file_size(file)?));
    writer.save(file)?;
    Ok(writer.cursor.into_inner())
}

/// Deserializes a terrain file from explicit `terrain.bin` bytes.
///
/// # Errors
///
/// Returns an error if the bytes do not match the terrain ABI, if the payload is
/// truncated, or if trailing bytes remain after the last mesh.
pub fn deserialize_terrain_file(bytes: &[u8]) -> io::Result<TerrainFile> {
    let mut reader = Reader::new(bytes);
    let file = reader.load()?;

    if remaining_len(&reader) != 0 {
        return Err(invalid_data(format!(
            "terrain.bin contains {} unexpected trailing bytes",
            remaining_len(&reader)
        )));
    }

    Ok(file)
}

/// Reads and validates a serialized terrain file from `path`.
///
/// # Errors
///
/// Returns an error if the file cannot be read or if its contents do not match the
/// terrain ABI.
pub fn load_terrain_file(path: &Path) -> io::Result<TerrainFile> {
    let file = File::open(path)?;
    // SAFETY: The mapping is read-only and remains alive for the complete decode.
    let bytes = unsafe { memmap2::MmapOptions::new().map(&file)? };
    deserialize_terrain_file(&bytes)
}

fn encode_bias_normal_component(value: f32) -> u8 {
    ((value * 0.5 + 0.5).clamp(0.0, 1.0) * 255.0).round() as u8
}

fn serialized_terrain_file_size(file: &TerrainFile) -> io::Result<usize> {
    let mut total = TERRAIN_FILE_HEADER_BYTES;

    for mesh in &file.meshes {
        total = total
            .checked_add(serialized_terrain_mesh_size(mesh)?)
            .ok_or_else(|| invalid_input("terrain.bin serialized size overflowed usize"))?;
    }

    Ok(total)
}

fn serialized_terrain_mesh_size(mesh: &TerrainMesh) -> io::Result<usize> {
    let vertex_count = u32_len(mesh.vertices.len(), "terrain vertices")?;
    u32_len(mesh.triangles.len(), "terrain triangles")?;

    let vertex_bytes = mesh
        .vertices
        .len()
        .checked_mul(size_of::<TerrainVertex>())
        .ok_or_else(|| invalid_input("terrain vertex payload size overflowed usize"))?;
    let triangle_bytes = mesh
        .triangles
        .len()
        .checked_mul(terrain_triangle_bytes(vertex_count))
        .ok_or_else(|| invalid_input("terrain triangle payload size overflowed usize"))?;

    TERRAIN_MESH_HEADER_BYTES
        .checked_add(vertex_bytes)
        .and_then(|size| size.checked_add(triangle_bytes))
        .ok_or_else(|| invalid_input("terrain mesh serialized size overflowed usize"))
}

impl Load for TerrainFile {
    fn load(stream: &mut Reader<'_>) -> io::Result<Self> {
        let header = TerrainFileHeader::load(stream)?;
        ensure_sequence_minimum(stream, header.mesh_count, TERRAIN_MESH_HEADER_BYTES, "mesh headers")?;
        let meshes = stream.load_seq(header.mesh_count)?;

        Ok(Self {
            cell_size: header.cell_size,
            patch_size: header.patch_size,
            origin_cell: header.origin_cell,
            cell_size_xy: header.cell_size_xy,
            world_origin: header.world_origin,
            world_size: header.world_size,
            atlas_size: header.atlas_size,
            logical_tile_size: header.logical_tile_size,
            gutter_size: header.gutter_size,
            physical_tile_size: header.physical_tile_size,
            tiles_per_row: header.tiles_per_row,
            atlas_max_lod: header.atlas_max_lod,
            material_size_xy: header.material_size_xy,
            pattern_count: header.pattern_count,
            pattern_tile_size: header.pattern_tile_size,
            pattern_gutter_size: header.pattern_gutter_size,
            pattern_physical_size: header.pattern_physical_size,
            patterns_per_row: header.patterns_per_row,
            meshes,
        })
    }
}

impl Save for TerrainFile {
    fn save(&self, stream: &mut Writer) -> io::Result<()> {
        stream.save_bytes(&TERRAIN_FILE_MAGIC)?;
        stream.save(&TERRAIN_FILE_VERSION)?;
        stream.save(&self.cell_size)?;
        stream.save(&self.patch_size)?;
        stream.save(&self.origin_cell)?;
        stream.save(&self.cell_size_xy)?;
        stream.save(&self.world_origin)?;
        stream.save(&self.world_size)?;
        stream.save(&self.atlas_size)?;
        stream.save(&self.logical_tile_size)?;
        stream.save(&self.gutter_size)?;
        stream.save(&self.physical_tile_size)?;
        stream.save(&self.tiles_per_row)?;
        stream.save(&self.atlas_max_lod)?;
        stream.save(&self.material_size_xy)?;
        stream.save(&self.pattern_count)?;
        stream.save(&self.pattern_tile_size)?;
        stream.save(&self.pattern_gutter_size)?;
        stream.save(&self.pattern_physical_size)?;
        stream.save(&self.patterns_per_row)?;
        stream.save(&TERRAIN_VERTEX_STRIDE)?;
        stream.save(&TERRAIN_FILE_INDEX_FORMAT_AUTO)?;
        stream.save(&u32_len(self.meshes.len(), "terrain meshes")?)?;
        stream.save_seq(&self.meshes)
    }
}

impl Load for TerrainMesh {
    fn load(stream: &mut Reader<'_>) -> io::Result<Self> {
        ensure_remaining(stream, TERRAIN_MESH_HEADER_BYTES, "terrain mesh header")?;

        let bounding_sphere = stream.load_pod()?;
        let bounding_box = stream.load_pod()?;
        let vertex_count = stream.load()?;
        let triangle_count = stream.load()?;
        let required_payload = required_mesh_payload_bytes(vertex_count, triangle_count)?;
        ensure_remaining(stream, required_payload, "terrain mesh payload")?;

        let vertices = stream.load_vec(vertex_count)?;
        let triangles = if mesh_uses_u16_indices(vertex_count) {
            load_narrow_triangles(stream, usize_from_u32(triangle_count, "terrain triangle_count")?)?
        } else {
            stream.load_vec(triangle_count)?
        };
        validate_triangle_indices(&triangles, vertex_count, io::ErrorKind::InvalidData)?;

        Ok(Self {
            bounding_sphere,
            bounding_box,
            vertices,
            triangles,
        })
    }
}

impl Save for TerrainMesh {
    fn save(&self, stream: &mut Writer) -> io::Result<()> {
        let vertex_count = u32_len(self.vertices.len(), "terrain vertices")?;
        validate_triangle_indices(&self.triangles, vertex_count, io::ErrorKind::InvalidInput)?;

        stream.save_pod(&self.bounding_sphere)?;
        stream.save_pod(&self.bounding_box)?;
        stream.save(&vertex_count)?;
        stream.save(&u32_len(self.triangles.len(), "terrain triangles")?)?;
        stream.save_vec(&self.vertices)?;
        if mesh_uses_u16_indices(vertex_count) {
            save_narrow_triangles(stream, &self.triangles)
        } else {
            stream.save_vec(&self.triangles)
        }
    }
}

/// Triangles converted per pass when a mesh stores `u16` indices on disk.
///
/// Narrowing and widening run through a fixed stack buffer of this many
/// triangles instead of a heap vector sized to the whole mesh, so neither
/// direction allocates a second index buffer. 256 triples is 1.5 KiB of `u16`
/// staging -- small enough to stay resident, large enough that the per-pass
/// read/write overhead is negligible against the copy itself.
const TRIANGLE_CONVERSION_CHUNK: usize = 256;

/// Reads `triangle_count` `u16` index triples and widens them into the in-memory
/// `[u32; 3]` representation.
///
/// The caller must have already checked that the payload bytes are present, so a
/// short read here is a truncation the header validation failed to catch.
fn load_narrow_triangles(stream: &mut Reader<'_>, triangle_count: usize) -> io::Result<Vec<[u32; 3]>> {
    let mut triangles = Vec::with_capacity(triangle_count);
    let mut staging = [[0u16; 3]; TRIANGLE_CONVERSION_CHUNK];

    let mut remaining = triangle_count;
    while remaining != 0 {
        let len = remaining.min(TRIANGLE_CONVERSION_CHUNK);
        let staging = &mut staging[..len];
        stream.read_exact(cast_slice_mut(staging))?;
        triangles.extend(staging.iter().map(|&[a, b, c]| [u32::from(a), u32::from(b), u32::from(c)]));
        remaining -= len;
    }

    Ok(triangles)
}

/// Narrows `triangles` to `u16` triples and writes them.
///
/// Only called once [`validate_triangle_indices`] has confirmed every index is
/// below a `vertex_count` that [`mesh_uses_u16_indices`] accepted, so the
/// conversion cannot actually overflow; it stays checked because a silent
/// truncation here would write a corrupt index buffer.
fn save_narrow_triangles(stream: &mut Writer, triangles: &[[u32; 3]]) -> io::Result<()> {
    let mut staging = [[0u16; 3]; TRIANGLE_CONVERSION_CHUNK];

    for chunk in triangles.chunks(TRIANGLE_CONVERSION_CHUNK) {
        for (narrowed, &[a, b, c]) in staging.iter_mut().zip(chunk) {
            *narrowed = [
                narrow_triangle_index(a)?,
                narrow_triangle_index(b)?,
                narrow_triangle_index(c)?,
            ];
        }
        stream.save_vec(&staging[..chunk.len()])?;
    }

    Ok(())
}

fn narrow_triangle_index(index: u32) -> io::Result<u16> {
    u16::try_from(index).map_err(|_| invalid_input("terrain triangle index exceeds u16"))
}

fn validate_triangle_indices(triangles: &[[u32; 3]], vertex_count: u32, error_kind: io::ErrorKind) -> io::Result<()> {
    for (triangle_index, triangle) in triangles.iter().enumerate() {
        for &index in triangle {
            if index >= vertex_count {
                return Err(io::Error::new(
                    error_kind,
                    format!("terrain mesh triangle {triangle_index} index {index} is outside vertex count {vertex_count}"),
                ));
            }
        }
    }
    Ok(())
}

/// Fixed-layout metadata read from the 116-byte `terrain.bin` header.
#[derive(Clone, Debug, PartialEq)]
pub struct TerrainFileHeader {
    /// World-space width and height of one terrain cell.
    pub(crate) cell_size: f32,
    /// World-space width and height of one terrain patch.
    pub(crate) patch_size: f32,
    /// Minimum landscape cell coordinate covered by the terrain package.
    pub origin_cell: [i32; 2],
    /// Width and height of the terrain package in landscape cells.
    pub cell_size_xy: [u32; 2],
    /// World-space origin of the terrain package.
    pub(crate) world_origin: Vec2,
    /// World-space width and height of the terrain package.
    pub(crate) world_size: Vec2,
    /// Width and height of the square source-texture atlas.
    pub atlas_size: u32,
    /// Width and height of one logical source-atlas tile.
    pub logical_tile_size: u32,
    /// Gutter width around each logical source-atlas tile.
    pub gutter_size: u32,
    /// Width and height of one physical source-atlas tile, including gutters.
    pub physical_tile_size: u32,
    /// Number of source-atlas tiles in each row.
    pub tiles_per_row: u32,
    /// Maximum source-atlas mip level sampled by the terrain shader.
    pub atlas_max_lod: u32,
    /// Width and height of the terrain material control maps.
    pub material_size_xy: [u32; 2],
    /// Number of blend patterns stored in the pattern atlas.
    pub pattern_count: u32,
    /// Width and height of one logical blend-pattern tile.
    pub pattern_tile_size: u32,
    /// Gutter width around each logical blend-pattern tile.
    pub pattern_gutter_size: u32,
    /// Width and height of one physical blend-pattern tile, including gutters.
    pub pattern_physical_size: u32,
    /// Number of blend-pattern tiles in each row.
    pub patterns_per_row: u32,
    /// Number of terrain mesh payloads following the header.
    pub mesh_count: u32,
}

impl Load for TerrainFileHeader {
    fn load(stream: &mut Reader<'_>) -> io::Result<Self> {
        ensure_remaining(stream, TERRAIN_FILE_HEADER_BYTES, "terrain file header")?;

        let magic: [u8; 8] = stream.load()?;
        if magic != TERRAIN_FILE_MAGIC {
            return Err(invalid_data(format!(
                "terrain.bin magic mismatch: expected {:?}, found {:?}",
                String::from_utf8_lossy(&TERRAIN_FILE_MAGIC),
                String::from_utf8_lossy(&magic)
            )));
        }

        let version: u32 = stream.load()?;
        if version != TERRAIN_FILE_VERSION {
            return Err(invalid_data(format!(
                "terrain.bin version mismatch: expected {TERRAIN_FILE_VERSION}, found {version}"
            )));
        }

        let mut header = Self {
            cell_size: stream.load()?,
            patch_size: stream.load()?,
            origin_cell: stream.load()?,
            cell_size_xy: stream.load()?,
            world_origin: stream.load()?,
            world_size: stream.load()?,
            atlas_size: stream.load()?,
            logical_tile_size: stream.load()?,
            gutter_size: stream.load()?,
            physical_tile_size: stream.load()?,
            tiles_per_row: stream.load()?,
            atlas_max_lod: stream.load()?,
            material_size_xy: stream.load()?,
            pattern_count: stream.load()?,
            pattern_tile_size: stream.load()?,
            pattern_gutter_size: stream.load()?,
            pattern_physical_size: stream.load()?,
            patterns_per_row: stream.load()?,
            mesh_count: 0,
        };

        let vertex_stride: u32 = stream.load()?;
        if vertex_stride != TERRAIN_VERTEX_STRIDE {
            return Err(invalid_data(format!(
                "terrain.bin vertex stride mismatch: expected {TERRAIN_VERTEX_STRIDE}, found {vertex_stride}"
            )));
        }

        let file_index_format: u32 = stream.load()?;
        if file_index_format != TERRAIN_FILE_INDEX_FORMAT_AUTO {
            return Err(invalid_data(format!(
                "terrain.bin index format mismatch: expected {} (per-mesh u16/u32 inferred from vertex_count), \
                 found {file_index_format}; regenerate distant land",
                TERRAIN_FILE_INDEX_FORMAT_AUTO
            )));
        }

        header.mesh_count = stream.load()?;
        Ok(header)
    }
}

/// Reads and validates only the fixed `terrain.bin` header from `path`.
///
/// The read is bounded to `TERRAIN_FILE_HEADER_BYTES`, so mesh payloads are not
/// read or materialized.
///
/// # Errors
///
/// Returns an error if the file cannot be read, is shorter than the fixed header,
/// or contains header constants that do not match the terrain ABI.
pub fn load_terrain_header(path: &Path) -> io::Result<TerrainFileHeader> {
    let mut bytes = Vec::with_capacity(TERRAIN_FILE_HEADER_BYTES);
    File::open(path)?
        .take(TERRAIN_FILE_HEADER_BYTES as u64)
        .read_to_end(&mut bytes)?;

    let mut reader = Reader::new(&bytes);
    TerrainFileHeader::load(&mut reader)
}

fn required_mesh_payload_bytes(vertex_count: u32, triangle_count: u32) -> io::Result<usize> {
    let index_bytes_per_triangle = terrain_triangle_bytes(vertex_count);
    let vertex_count = usize_from_u32(vertex_count, "terrain vertex_count")?;
    let triangle_count = usize_from_u32(triangle_count, "terrain triangle_count")?;

    let vertex_bytes = vertex_count
        .checked_mul(size_of::<TerrainVertex>())
        .ok_or_else(|| invalid_data("terrain vertex payload size overflowed usize"))?;
    let triangle_bytes = triangle_count
        .checked_mul(index_bytes_per_triangle)
        .ok_or_else(|| invalid_data("terrain triangle payload size overflowed usize"))?;

    vertex_bytes
        .checked_add(triangle_bytes)
        .ok_or_else(|| invalid_data("terrain mesh payload size overflowed usize"))
}

fn ensure_sequence_minimum(stream: &Reader<'_>, count: u32, minimum_item_bytes: usize, context: &str) -> io::Result<()> {
    let count = usize_from_u32(count, context)?;
    let required = count
        .checked_mul(minimum_item_bytes)
        .ok_or_else(|| invalid_data(format!("{context} size overflowed usize")))?;
    ensure_remaining(stream, required, context)
}

fn remaining_len(stream: &Reader<'_>) -> usize {
    let position = usize::try_from(stream.cursor.position()).unwrap_or(usize::MAX);
    stream.cursor.get_ref().len().saturating_sub(position)
}

fn ensure_remaining(stream: &Reader<'_>, needed: usize, context: &str) -> io::Result<()> {
    let remaining = remaining_len(stream);
    if remaining < needed {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("terrain.bin truncated while reading {context}: need {needed} bytes, have {remaining}"),
        ));
    }

    Ok(())
}

fn u32_len(len: usize, context: &str) -> io::Result<u32> {
    u32::try_from(len).map_err(|_| invalid_input(format!("{context} exceeds u32 on-disk range")))
}

fn usize_from_u32(value: u32, context: &str) -> io::Result<usize> {
    usize::try_from(value).map_err(|_| invalid_data(format!("{context} does not fit in usize")))
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

#[cfg(test)]
mod tests;
