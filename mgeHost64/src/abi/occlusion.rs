use super::{ByteReadError, ByteReader};
use bytemuck::{Pod, Zeroable};
use thiserror::Error;

/// Fixed filename for the generated terrain occlusion asset.
pub const TERRAIN_OCCLUSION_FILE_NAME: &str = "terrain_occlusion.bin";
/// Magic for `terrain_occlusion.bin`.
pub const TERRAIN_OCCLUSION_FILE_MAGIC: [u8; 8] = *b"XEOCCL02";
/// Supported `terrain_occlusion.bin` version.
pub const TERRAIN_OCCLUSION_FILE_VERSION: u32 = 2;
/// Fixed byte size of the `terrain_occlusion.bin` header.
pub const TERRAIN_OCCLUSION_HEADER_SIZE: usize = 56;

/// Fixed-size `terrain_occlusion.bin` header read as a POD block in little-endian file order.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Zeroable, Pod)]
pub struct TerrainOcclusionFileHeader {
    /// Format magic, `XEOCCL02`.
    pub magic: [u8; 8],
    /// Format version, currently `2`.
    pub version: u32,
    /// Covered region origin in exterior cell coordinates.
    pub origin_cell: [i32; 2],
    /// Covered region dimensions in exterior cells.
    pub cell_size_xy: [u32; 2],
    /// Covered region origin in world units.
    pub world_origin: [f32; 2],
    /// Covered region size in world units.
    pub world_size: [f32; 2],
    /// Level-0 horizon grid spacing.
    pub base_spacing: f32,
    /// Level-0 grid width in samples.
    pub base_nx: u32,
    /// Level-0 grid height in samples.
    pub base_ny: u32,
}

/// Parsed terrain occlusion asset.
#[derive(Clone, Debug, PartialEq)]
pub struct TerrainOcclusionData {
    /// Parsed file header.
    pub header: TerrainOcclusionFileHeader,
    /// Row-major base-grid max heights.
    pub max_z: Vec<f32>,
}

/// Structural errors in `terrain_occlusion.bin`.
#[derive(Debug, Error, PartialEq)]
pub enum OcclusionFormatError {
    /// The file ended before a fixed field or payload could be read.
    #[error("unexpected EOF at byte {offset}: needed {needed} more bytes but only {remaining} remain")]
    UnexpectedEof {
        /// Byte offset where the read started.
        offset: usize,
        /// Requested byte count.
        needed: usize,
        /// Remaining byte count.
        remaining: usize,
    },
    /// The magic value was not `XEOCCL02`.
    #[error("terrain_occlusion.bin magic must be XEOCCL02, got {0:?}")]
    InvalidMagic([u8; 8]),
    /// The version is not supported by this host.
    #[error("terrain_occlusion.bin version must be 2, got {0}")]
    UnsupportedVersion(u32),
    /// The serialized region header is invalid.
    #[error("terrain_occlusion.bin region layout is invalid: {0}")]
    InvalidRegion(&'static str),
    /// The serialized grid header is invalid.
    #[error("terrain_occlusion.bin grid layout is invalid: {0}")]
    InvalidGrid(&'static str),
    /// Payload size overflowed while validating the file.
    #[error("terrain_occlusion.bin payload size overflow while computing {0}")]
    IntegerOverflow(&'static str),
    /// Extra bytes remained after the base-grid payload.
    #[error("terrain_occlusion.bin has {total} bytes but the base payload consumes only {consumed}")]
    TrailingBytes {
        /// Bytes consumed by the header and levels.
        consumed: usize,
        /// Total input byte count.
        total: usize,
    },
    /// Header fields do not match the paired `terrain.bin` header.
    #[error("terrain_occlusion.bin does not match terrain.bin: {0}")]
    TerrainMismatch(&'static str),
    /// A serialized height was NaN or infinite.
    #[error("terrain_occlusion.bin non-finite base height at index {index}")]
    NonFiniteHeight {
        /// Row-major index inside the base grid.
        index: usize,
    },
}

impl From<ByteReadError> for OcclusionFormatError {
    fn from(error: ByteReadError) -> Self {
        Self::UnexpectedEof {
            offset: error.offset,
            needed: error.needed,
            remaining: error.remaining,
        }
    }
}

/// Parses `terrain_occlusion.bin` and copies its base grid into an owned `f32` vector.
pub fn parse_terrain_occlusion(bytes: &[u8]) -> Result<TerrainOcclusionData, OcclusionFormatError> {
    let mut reader = ByteReader::new(bytes);
    let header = read_header(&mut reader)?;
    validate_header(&header)?;

    let len = checked_product_usize(header.base_nx as usize, header.base_ny as usize, "base sample count")?;
    let byte_count = checked_product_usize(len, size_of_f32(), "base byte count")?;
    let payload = reader.read_exact_bytes(byte_count)?;
    let mut max_z = Vec::with_capacity(len);
    for chunk in payload.chunks_exact(size_of_f32()) {
        max_z.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }

    if reader.offset() != bytes.len() {
        return Err(OcclusionFormatError::TrailingBytes {
            consumed: reader.offset(),
            total: bytes.len(),
        });
    }

    Ok(TerrainOcclusionData { header, max_z })
}

fn read_header(reader: &mut ByteReader<'_>) -> Result<TerrainOcclusionFileHeader, OcclusionFormatError> {
    Ok(bytemuck::pod_read_unaligned(
        reader.read_exact_bytes(TERRAIN_OCCLUSION_HEADER_SIZE)?,
    ))
}

fn validate_header(header: &TerrainOcclusionFileHeader) -> Result<(), OcclusionFormatError> {
    if header.magic != TERRAIN_OCCLUSION_FILE_MAGIC {
        return Err(OcclusionFormatError::InvalidMagic(header.magic));
    }
    if header.version != TERRAIN_OCCLUSION_FILE_VERSION {
        return Err(OcclusionFormatError::UnsupportedVersion(header.version));
    }
    if !(header.base_spacing.is_finite() && header.base_spacing > 0.0) {
        return Err(OcclusionFormatError::InvalidGrid("base_spacing must be finite and positive"));
    }
    if !header.world_origin[0].is_finite() || !header.world_origin[1].is_finite() {
        return Err(OcclusionFormatError::InvalidRegion("world_origin must be finite"));
    }
    if !(header.world_size[0].is_finite()
        && header.world_size[1].is_finite()
        && header.world_size[0] > 0.0
        && header.world_size[1] > 0.0)
    {
        return Err(OcclusionFormatError::InvalidRegion("world_size must be finite and positive"));
    }

    let expected_nx = grid_size_from_world_size(header.world_size[0], header.base_spacing)?;
    let expected_ny = grid_size_from_world_size(header.world_size[1], header.base_spacing)?;
    if header.base_nx != expected_nx || header.base_ny != expected_ny {
        return Err(OcclusionFormatError::InvalidGrid(
            "base dimensions must match world_size and base_spacing",
        ));
    }

    Ok(())
}

fn grid_size_from_world_size(world_size: f32, spacing: f32) -> Result<u32, OcclusionFormatError> {
    let samples = (world_size / spacing).ceil() + 1.0;
    if !samples.is_finite() || samples < 1.0 || samples > u32::MAX as f32 {
        return Err(OcclusionFormatError::InvalidGrid("base dimensions must fit in u32"));
    }
    Ok(samples as u32)
}

fn checked_product_usize(left: usize, right: usize, context: &'static str) -> Result<usize, OcclusionFormatError> {
    left.checked_mul(right).ok_or(OcclusionFormatError::IntegerOverflow(context))
}

fn size_of_f32() -> usize {
    std::mem::size_of::<f32>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    fn push_u32(bytes: &mut Vec<u8>, value: u32) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn push_i32(bytes: &mut Vec<u8>, value: i32) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn push_f32(bytes: &mut Vec<u8>, value: f32) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn build_minimal_fixture() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"XEOCCL02");
        push_u32(&mut bytes, 2);
        push_i32(&mut bytes, 1);
        push_i32(&mut bytes, -2);
        push_u32(&mut bytes, 1);
        push_u32(&mut bytes, 1);
        push_f32(&mut bytes, 8192.0);
        push_f32(&mut bytes, -16384.0);
        push_f32(&mut bytes, 8192.0);
        push_f32(&mut bytes, 8192.0);
        push_f32(&mut bytes, 8192.0);
        push_u32(&mut bytes, 2);
        push_u32(&mut bytes, 2);
        push_f32(&mut bytes, 1.0);
        push_f32(&mut bytes, f32::MIN);
        push_f32(&mut bytes, 3.0);
        push_f32(&mut bytes, 4.0);
        bytes
    }

    #[test]
    fn header_size_and_offsets_match_contract() {
        assert_eq!(size_of::<TerrainOcclusionFileHeader>(), TERRAIN_OCCLUSION_HEADER_SIZE);
        assert_eq!(offset_of!(TerrainOcclusionFileHeader, magic), 0);
        assert_eq!(offset_of!(TerrainOcclusionFileHeader, version), 8);
        assert_eq!(offset_of!(TerrainOcclusionFileHeader, origin_cell), 12);
        assert_eq!(offset_of!(TerrainOcclusionFileHeader, cell_size_xy), 20);
        assert_eq!(offset_of!(TerrainOcclusionFileHeader, world_origin), 28);
        assert_eq!(offset_of!(TerrainOcclusionFileHeader, world_size), 36);
        assert_eq!(offset_of!(TerrainOcclusionFileHeader, base_spacing), 44);
        assert_eq!(offset_of!(TerrainOcclusionFileHeader, base_nx), 48);
        assert_eq!(offset_of!(TerrainOcclusionFileHeader, base_ny), 52);
    }

    #[test]
    fn minimal_fixture_parses_exactly() {
        let bytes = build_minimal_fixture();
        let parsed = parse_terrain_occlusion(&bytes).expect("fixture must parse");

        assert_eq!(parsed.header.magic, TERRAIN_OCCLUSION_FILE_MAGIC);
        assert_eq!(parsed.header.version, TERRAIN_OCCLUSION_FILE_VERSION);
        assert_eq!(parsed.header.origin_cell, [1, -2]);
        assert_eq!(parsed.header.cell_size_xy, [1, 1]);
        assert_eq!(parsed.header.world_origin, [8192.0, -16384.0]);
        assert_eq!(parsed.header.world_size, [8192.0, 8192.0]);
        assert_eq!(parsed.header.base_spacing, 8192.0);
        assert_eq!(parsed.header.base_nx, 2);
        assert_eq!(parsed.header.base_ny, 2);
        assert_eq!(parsed.max_z, vec![1.0, f32::MIN, 3.0, 4.0]);
    }

    #[test]
    fn rejects_bad_magic_version_dimensions_truncation_and_trailing_bytes() {
        let bytes = build_minimal_fixture();

        let mut bad_magic = bytes.clone();
        bad_magic[0..8].copy_from_slice(b"BAD0CCL?");
        assert_eq!(
            parse_terrain_occlusion(&bad_magic).expect_err("bad magic"),
            OcclusionFormatError::InvalidMagic(*b"BAD0CCL?")
        );

        let mut bad_version = bytes.clone();
        bad_version[8..12].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(
            parse_terrain_occlusion(&bad_version).expect_err("bad version"),
            OcclusionFormatError::UnsupportedVersion(1)
        );

        let mut bad_nx = bytes.clone();
        bad_nx[48..52].copy_from_slice(&3u32.to_le_bytes());
        assert_eq!(
            parse_terrain_occlusion(&bad_nx).expect_err("bad base dimensions"),
            OcclusionFormatError::InvalidGrid("base dimensions must match world_size and base_spacing")
        );

        let mut truncated = bytes.clone();
        truncated.pop();
        assert!(matches!(
            parse_terrain_occlusion(&truncated).expect_err("truncated"),
            OcclusionFormatError::UnexpectedEof { .. }
        ));

        let mut trailing = bytes;
        trailing.push(0);
        assert_eq!(
            parse_terrain_occlusion(&trailing).expect_err("trailing bytes"),
            OcclusionFormatError::TrailingBytes {
                consumed: TERRAIN_OCCLUSION_HEADER_SIZE + 4 * size_of_f32(),
                total: TERRAIN_OCCLUSION_HEADER_SIZE + 4 * size_of_f32() + 1,
            }
        );
    }

    #[test]
    fn rejects_non_finite_spacing_and_size() {
        let bytes = build_minimal_fixture();

        let mut bad_spacing = bytes.clone();
        bad_spacing[44..48].copy_from_slice(&f32::NAN.to_le_bytes());
        assert_eq!(
            parse_terrain_occlusion(&bad_spacing).expect_err("bad spacing"),
            OcclusionFormatError::InvalidGrid("base_spacing must be finite and positive")
        );

        let mut bad_size = bytes;
        bad_size[36..40].copy_from_slice(&f32::INFINITY.to_le_bytes());
        assert_eq!(
            parse_terrain_occlusion(&bad_size).expect_err("bad size"),
            OcclusionFormatError::InvalidRegion("world_size must be finite and positive")
        );
    }
}
