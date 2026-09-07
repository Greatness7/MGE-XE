//! Canonical identities and hashing vocabulary for incremental generation units.
//!
//! This module owns the representation and settings partitions shared by
//! projection, the state database, and production fingerprint callers.

/// Invalidation knob for every statics-domain fingerprint recipe.
///
/// Bump when a change to mesh, texture, merge, shard-assignment, record-key, or serializer
/// behavior must invalidate cached statics output. One knob rather than one per recipe: each
/// extra knob is one more a human must remember to bump, the exact failure the knobs exist
/// to prevent.
pub const STATICS_RECIPE_VERSION: u32 = 5;

/// Invalidation knob for every terrain-domain fingerprint recipe.
///
/// See [`STATICS_RECIPE_VERSION`] for why this is one knob and not one per recipe.
pub const TERRAIN_RECIPE_VERSION: u32 = 3;

/// Exterior-cell coordinate key for one merge unit.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct MergeUnitKey {
    pub x: i32,
    pub y: i32,
}

impl MergeUnitKey {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

impl std::fmt::Display for MergeUnitKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write_coordinate_display(f, self.x, self.y)
    }
}

/// Landscape-cell coordinate key for one terrain material unit.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct TerrainCellUnitKey {
    /// Landscape cell X coordinate.
    pub x: i32,
    /// Landscape cell Y coordinate.
    pub y: i32,
}

impl TerrainCellUnitKey {
    /// Builds a terrain-cell key from a landscape grid coordinate.
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

impl std::fmt::Display for TerrainCellUnitKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write_coordinate_display(f, self.x, self.y)
    }
}

/// Absolute 4-by-4-cell coordinate key for one terrain mesh chunk.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct TerrainChunkUnitKey {
    pub x: i32,
    pub y: i32,
}

impl TerrainChunkUnitKey {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// Maps a landscape cell to its absolute 4-by-4-cell chunk using Euclidean division.
    pub const fn from_cell(cell: TerrainCellUnitKey) -> Self {
        Self::new(cell.x.div_euclid(4), cell.y.div_euclid(4))
    }
}

impl std::fmt::Display for TerrainChunkUnitKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write_coordinate_display(f, self.x, self.y)
    }
}

/// Stable logical identity of one terrain mesh work item.
///
/// Names the nominal square of LAND cells the builder processes, in absolute cell coordinates. The
/// key deliberately encodes neither a region vector ordinal nor an atlas placement, so it stays
/// stable for any layout that decomposes the world into the same squares, and it also names edge
/// work whose owned-cell rectangle is clipped by its region.
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    serde::Deserialize,
    serde::Serialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub struct TerrainMeshWorkKey {
    /// Absolute X coordinate of the work item's first LAND cell.
    pub start_cell_x: i32,
    /// Absolute Y coordinate of the work item's first LAND cell.
    pub start_cell_y: i32,
    /// Nominal width and height of the work item, in LAND cells.
    pub cells_per_side: u32,
}

/// Renders a unit coordinate as legacy display text: `{x},{y}` with no whitespace.
///
/// Domain digests and human-facing dirty-key reports use this exact form.
pub fn write_coordinate_display(f: &mut std::fmt::Formatter<'_>, x: i32, y: i32) -> std::fmt::Result {
    write!(f, "{x},{y}")
}

/// Appends an optional 32-byte hash: a presence flag, followed by the bytes when present.
///
/// Projection and unit fingerprinting both embed optional hashes, and their streams must
/// agree byte-for-byte, so the encoding lives here rather than being spelled out per caller.
pub fn write_hash_option(writer: &mut CanonicalWriter, value: Option<[u8; 32]>) {
    writer.write_bool(value.is_some());
    if let Some(value) = value {
        writer.write_bytes(&value);
    }
}

/// Maps one mesh work item's nominal cell square to every absolute chunk unit it overlaps.
///
/// The square is deliberately *not* clipped to the owning atlas region. A work item's read set is
/// the nominal square plus a one-cell fringe: `default_chunk_uniform_height` scans one cell past the
/// square in `+x`/`+y` regardless of the region, and the dense grid's far edge vertices sample the
/// cell beyond the square. Because a chunk's fingerprint already covers its own 4-by-4 area plus a
/// one-cell halo, every cell within distance one of a nominal cell is covered by that cell's chunk,
/// which makes chunks-overlapping-the-nominal-square a sufficient dependency declaration. Clipping
/// to the region would not be: a narrow region can leave fringe reads several cells outside every
/// declared chunk's halo.
pub fn overlapping_absolute_chunks(start_cell_x: i32, start_cell_y: i32, cells_per_side: usize) -> Vec<TerrainChunkUnitKey> {
    let span = i32::try_from(cells_per_side).expect("mesh chunk cells per side must fit in i32");
    // `from_cell` is `div_euclid`, which is monotonic, so the covered chunks are exactly the
    // rectangle spanned by the square's first and last cell. Enumerating x-major yields the
    // sorted, deduplicated order because `Ord` compares `x` first.
    let (first, last) = (
        TerrainChunkUnitKey::from_cell(TerrainCellUnitKey::new(start_cell_x, start_cell_y)),
        TerrainChunkUnitKey::from_cell(TerrainCellUnitKey::new(start_cell_x + span - 1, start_cell_y + span - 1)),
    );
    let mut chunks = Vec::new();
    for x in first.x..=last.x {
        for y in first.y..=last.y {
            chunks.push(TerrainChunkUnitKey::new(x, y));
        }
    }
    chunks
}

/// A value with a stable, platform-independent canonical byte representation.
pub trait CanonicalWrite {
    /// Appends this value to `writer` without architecture-dependent layout or byte order.
    fn write_canonical(&self, writer: &mut CanonicalWriter);
}

/// Builds canonical fingerprint bytes and hashes the finished representation with BLAKE3.
///
/// Integer and float primitives are always written little-endian. Variable-width values
/// use `u64` length prefixes, including each member of a canonically sorted set, so adjacent
/// fields cannot collide through different splits.
#[derive(Debug, Default)]
pub struct CanonicalWriter {
    bytes: Vec<u8>,
}

impl CanonicalWriter {
    /// Creates an empty canonical writer.
    pub const fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    /// Writes one unsigned byte.
    pub fn write_u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    /// Writes a little-endian `u16`.
    pub fn write_u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    /// Writes a boolean as exactly `0` or `1`.
    pub fn write_bool(&mut self, value: bool) {
        self.write_u8(u8::from(value));
    }

    /// Writes a little-endian `u32`.
    pub fn write_u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    /// Writes a little-endian `u64`.
    pub fn write_u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    /// Writes a little-endian `i32`.
    pub fn write_i32(&mut self, value: i32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    /// Writes the raw IEEE-754 bits of an `f32` in little-endian order.
    pub fn write_f32(&mut self, value: f32) {
        self.write_u32(value.to_bits());
    }

    /// Writes a length-prefixed byte string.
    pub fn write_bytes(&mut self, value: &[u8]) {
        self.write_u64(value.len() as u64);
        self.bytes.extend_from_slice(value);
    }

    /// Writes a length-prefixed UTF-8 string.
    pub fn write_str(&mut self, value: &str) {
        self.write_bytes(value.as_bytes());
    }

    /// Returns the canonical bytes accumulated so far.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Returns the BLAKE3 digest of all canonical bytes written so far.
    pub fn finish(self) -> [u8; 32] {
        *blake3::hash(&self.bytes).as_bytes()
    }

    /// Hashes the current bytes with BLAKE3, then clears the buffer while retaining capacity.
    ///
    /// The digest is identical to [`Self::finish`] on the same byte sequence. Intended for
    /// sequential unit loops that reuse one writer across many payloads.
    pub fn finish_and_clear(&mut self) -> [u8; 32] {
        let digest = *blake3::hash(&self.bytes).as_bytes();
        self.bytes.clear();
        digest
    }
}

impl CanonicalWrite for MergeUnitKey {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.write_i32(self.x);
        writer.write_i32(self.y);
    }
}

impl CanonicalWrite for TerrainCellUnitKey {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.write_i32(self.x);
        writer.write_i32(self.y);
    }
}

impl CanonicalWrite for TerrainChunkUnitKey {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.write_i32(self.x);
        writer.write_i32(self.y);
    }
}

#[cfg(test)]
mod tests;
