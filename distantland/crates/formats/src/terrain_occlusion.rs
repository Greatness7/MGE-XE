//! Generated `terrain_occlusion.bin` horizon-occluder asset.
//!
//! # `terrain_occlusion.bin` ABI note
//!
//! All scalar values are serialized in little-endian order. The file starts with
//! this fixed 56-byte header, followed by one tightly packed `f32` max-height grid:
//!
//! | Offset | Size | Field | Type | Validation |
//! | --- | ---: | --- | --- | --- |
//! | 0 | 8 | `magic` | `[u8; 8]` | must equal `XEOCCL02` |
//! | 8 | 4 | `version` | `u32` | must equal `2` |
//! | 12 | 8 | `origin_cell` | `[i32; 2]` | must match `terrain.bin` |
//! | 20 | 8 | `cell_size_xy` | `[u32; 2]` | must match `terrain.bin` |
//! | 28 | 8 | `world_origin` | `[f32; 2]` | must match `terrain.bin` |
//! | 36 | 8 | `world_size` | `[f32; 2]` | must match `terrain.bin` |
//! | 44 | 4 | `base_spacing` | `f32` | configured horizon grid spacing |
//! | 48 | 4 | `base_nx` | `u32` | `ceil(world_size.x / base_spacing) + 1` |
//! | 52 | 4 | `base_ny` | `u32` | `ceil(world_size.y / base_spacing) + 1` |
//! | 56 | ... | base payload | `f32[]` | row-major `y * nx + x` |

use std::io;

use bytes_io::Writer;

use crate::world::LAND_CELL_SIZE;

/// Fixed filename for the generated terrain occlusion asset.
pub const TERRAIN_OCCLUSION_FILE_NAME: &str = "terrain_occlusion.bin";
/// Eight-byte `terrain_occlusion.bin` magic.
pub const TERRAIN_OCCLUSION_MAGIC: [u8; 8] = *b"XEOCCL02";
/// `terrain_occlusion.bin` ABI version.
pub const TERRAIN_OCCLUSION_VERSION: u32 = 2;
/// Generator spacing matching the runtime default `Horizon Grid Spacing`.
pub const TERRAIN_OCCLUSION_GRID_SPACING: f32 = 512.0;
/// Empty max-height sentinel matching the host `EMPTY_HEIGHT` value.
pub const EMPTY_OCCLUSION_HEIGHT: f32 = f32::MIN;

const TERRAIN_OCCLUSION_HEADER_BYTES: usize = 56;
const LAND_VERTEX_SPACING: f32 = 128.0;
const LAND_GRID_SIZE: usize = 65;

/// Complete generated terrain occlusion asset.
#[derive(Clone, Debug, PartialEq)]
pub struct TerrainOcclusionFile {
    /// Covered region origin in exterior cell coordinates.
    pub origin_cell: [i32; 2],
    /// Covered region dimensions in exterior cells.
    pub cell_size_xy: [u32; 2],
    /// Covered region origin in world units.
    pub world_origin: [f32; 2],
    /// Covered region size in world units.
    pub world_size: [f32; 2],
    /// World-space spacing of the base grid.
    pub base_spacing: f32,
    /// Base-grid width in samples.
    pub base_nx: u32,
    /// Base-grid height in samples.
    pub base_ny: u32,
    /// Row-major base-grid max heights. `EMPTY_OCCLUSION_HEIGHT` marks uncovered samples.
    pub max_z: Vec<f32>,
}

/// Builds the base occlusion grid from decoded LAND heights of populated cells.
///
/// `cells` yields cell coordinates and decoded `[y][x]` height grids. The builder intentionally
/// mirrors `mgeHost64` horizon-field math: grid dimensions use `ceil(size / spacing) + 1`,
/// binning uses floor division clamped to the base grid.
///
/// LAND coordinates are exact for this parity contract: `8192.0` and `128.0` are powers of two,
/// so the generator's `cell * 8192 + grid * 128` expression produces the same `f32` coordinates
/// as the dense terrain mesh path at LAND sample points.
///
/// # Errors
///
/// Returns an error when `spacing` is not finite and positive, or when the implied grid is too
/// large to allocate on this platform.
pub fn build_terrain_occlusion<'a>(
    cells: impl IntoIterator<Item = ((i32, i32), &'a [[f32; LAND_GRID_SIZE]; LAND_GRID_SIZE])>,
    origin_cell: [i32; 2],
    cell_size_xy: [u32; 2],
    spacing: f32,
) -> io::Result<TerrainOcclusionFile> {
    validate_spacing(spacing)?;

    let world_origin = [origin_cell[0] as f32 * LAND_CELL_SIZE, origin_cell[1] as f32 * LAND_CELL_SIZE];
    let world_size = [
        cell_size_xy[0] as f32 * LAND_CELL_SIZE,
        cell_size_xy[1] as f32 * LAND_CELL_SIZE,
    ];
    let nx = grid_size_from_world_size(world_size[0], spacing)?;
    let ny = grid_size_from_world_size(world_size[1], spacing)?;
    let base_len = checked_grid_len(nx, ny, "terrain occlusion base grid")?;

    let mut max_z = vec![EMPTY_OCCLUSION_HEIGHT; base_len];

    let world_max = [world_origin[0] + world_size[0], world_origin[1] + world_size[1]];
    for ((cell_x, cell_y), heights) in cells {
        let cell_origin_x = cell_x as f32 * LAND_CELL_SIZE;
        let cell_origin_y = cell_y as f32 * LAND_CELL_SIZE;
        let x_bins: [Option<u32>; LAND_GRID_SIZE] = std::array::from_fn(|i| {
            height_sample_axis_index(
                nx,
                spacing,
                world_origin[0],
                world_max[0],
                cell_origin_x + i as f32 * LAND_VERTEX_SPACING,
            )
        });
        for (j, row) in heights.iter().enumerate() {
            let y = cell_origin_y + j as f32 * LAND_VERTEX_SPACING;
            let Some(iy) = height_sample_axis_index(ny, spacing, world_origin[1], world_max[1], y) else {
                continue;
            };
            for (i, &z) in row.iter().enumerate() {
                let Some(ix) = x_bins[i] else {
                    continue;
                };
                let index = (iy as usize) * (nx as usize) + (ix as usize);
                max_z[index] = max_z[index].max(z);
            }
        }
    }

    Ok(TerrainOcclusionFile {
        origin_cell,
        cell_size_xy,
        world_origin,
        world_size,
        base_spacing: spacing,
        base_nx: nx,
        base_ny: ny,
        max_z,
    })
}

/// Serializes a terrain occlusion file into explicit `terrain_occlusion.bin` bytes.
///
/// # Errors
///
/// Returns an error if the base grid is internally inconsistent or the serialized size overflows
/// `usize`.
pub fn serialize_terrain_occlusion_file(file: &TerrainOcclusionFile) -> io::Result<Vec<u8>> {
    validate_file(file)?;
    let mut writer = Writer::new(Vec::with_capacity(serialized_file_size(file)?));
    writer.save_bytes(&TERRAIN_OCCLUSION_MAGIC)?;
    writer.save(&TERRAIN_OCCLUSION_VERSION)?;
    writer.save(&file.origin_cell)?;
    writer.save(&file.cell_size_xy)?;
    writer.save(&file.world_origin)?;
    writer.save(&file.world_size)?;
    writer.save(&file.base_spacing)?;
    writer.save(&file.base_nx)?;
    writer.save(&file.base_ny)?;
    writer.save_vec(&file.max_z)?;
    Ok(writer.cursor.into_inner())
}

fn height_sample_axis_index(nx: u32, spacing: f32, origin: f32, world_max: f32, coordinate: f32) -> Option<u32> {
    if !coordinate.is_finite() || coordinate < origin || coordinate > world_max {
        return None;
    }

    Some(((coordinate - origin) / spacing).floor().clamp(0.0, (nx - 1) as f32) as u32)
}

fn validate_file(file: &TerrainOcclusionFile) -> io::Result<()> {
    validate_spacing(file.base_spacing)?;
    let expected_nx = grid_size_from_world_size(file.world_size[0], file.base_spacing)?;
    let expected_ny = grid_size_from_world_size(file.world_size[1], file.base_spacing)?;
    if file.base_nx != expected_nx || file.base_ny != expected_ny {
        return Err(invalid_input(format!(
            "terrain_occlusion.bin base dimensions mismatch: expected {expected_nx}x{expected_ny}, found {}x{}",
            file.base_nx, file.base_ny
        )));
    }

    let expected_len = checked_grid_len(file.base_nx, file.base_ny, "terrain occlusion base grid")?;
    if file.max_z.len() != expected_len {
        return Err(invalid_input(format!(
            "terrain_occlusion.bin base payload length mismatch: expected {expected_len}, found {}",
            file.max_z.len()
        )));
    }

    Ok(())
}

fn serialized_file_size(file: &TerrainOcclusionFile) -> io::Result<usize> {
    TERRAIN_OCCLUSION_HEADER_BYTES
        .checked_add(
            file.max_z
                .len()
                .checked_mul(size_of_f32())
                .ok_or_else(payload_size_overflow)?,
        )
        .ok_or_else(payload_size_overflow)
}

fn grid_size_from_world_size(world_size: f32, spacing: f32) -> io::Result<u32> {
    if !world_size.is_finite() || world_size < 0.0 {
        return Err(invalid_data(
            "terrain_occlusion.bin world size must be finite and non-negative",
        ));
    }
    let samples = (world_size / spacing).ceil() + 1.0;
    if samples > u32::MAX as f32 {
        return Err(invalid_data("terrain_occlusion.bin grid dimension exceeds u32"));
    }
    Ok(samples as u32)
}

fn checked_grid_len(nx: u32, ny: u32, context: &str) -> io::Result<usize> {
    (nx as usize)
        .checked_mul(ny as usize)
        .ok_or_else(|| invalid_input(format!("{context} length overflowed usize")))
}

fn validate_spacing(spacing: f32) -> io::Result<()> {
    if !spacing.is_finite() || spacing <= 0.0 {
        return Err(invalid_data("terrain_occlusion.bin spacing must be finite and positive"));
    }
    Ok(())
}

fn size_of_f32() -> usize {
    std::mem::size_of::<f32>()
}

fn payload_size_overflow() -> io::Error {
    invalid_input("terrain_occlusion.bin serialized size overflowed usize")
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

#[cfg(test)]
mod tests;
