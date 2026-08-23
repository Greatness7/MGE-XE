use super::*;
use smallvec::SmallVec;

// Matches the current patch sample grid; larger grids spill to the heap without changing behavior.
const PRECOMPUTED_AXIS_INLINE_CAPACITY: usize = 32;

/// Returns the three (x, y) vertex coordinates of the triangle containing cell-local point (x, y).
/// Mirrors `get_triangle_at` in usage_info.rs.
pub fn terrain_triangle_at_cell_coord(x: f32, y: f32) -> [(usize, usize); 3] {
    let px = x.clamp(0.0, 63.999);
    let py = y.clamp(0.0, 63.999);

    let col = px.floor() as usize;
    let row = py.floor() as usize;

    let bw = (row * 65 + col) & 1;

    let u = px - col as f32;
    let v = py - row as f32;

    let lt = ((bw == 0) & (v > u) | (bw == 1) & (u + v < 1.0)) as usize;

    let (bl, br, tl, tr) = ((col, row), (col + 1, row), (col, row + 1), (col + 1, row + 1));

    match (bw, lt) {
        (0, 0) => [br, tr, bl],
        (0, 1) => [tr, tl, bl],
        (1, 0) => [tl, br, tr],
        (1, 1) => [bl, br, tl],
        _ => unreachable!(),
    }
}

/// Returns barycentric weights for `p` inside the triangle `(a, b, c)`.
pub fn barycentric_weights(p: Vec2, a: Vec2, b: Vec2, c: Vec2) -> Vec3 {
    let v0 = b - a;
    let v1 = c - a;
    let v2 = p - a;
    let d00 = v0.dot(v0);
    let d01 = v0.dot(v1);
    let d11 = v1.dot(v1);
    let d20 = v2.dot(v0);
    let d21 = v2.dot(v1);
    let denom = d00 * d11 - d01 * d01;
    if denom.abs() < f32::EPSILON {
        return Vec3::new(1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0);
    }
    let v = (d11 * d20 - d01 * d21) / denom;
    let w = (d00 * d21 - d01 * d20) / denom;
    Vec3::new(1.0 - v - w, v, w)
}

/// Selects the appropriate MIP LOD for a texture given the pixel step of the sample grid.
///
/// The LOD is derived from the ratio of grid step to texture-space cell extent (4 quad
/// units per texture tile), matching MGE-XE's landscape texture scale convention.
pub(crate) fn terrain_texture_lod_for_grid(texture: &TerrainTexture, grid: &SampleGrid) -> f32 {
    let base = texture.base();

    let dx = grid.x_step();
    let dy = grid.y_step();
    let du_dx = dx / 4.0;
    let dv_dy = dy / 4.0;

    let rho = (du_dx * base.width() as f32).max(dv_dy * base.height() as f32);
    rho.log2().max(0.0)
}

/// Precomputed pixel-sample positions for one cell dimension of the atlas or standalone tile.
///
/// Each entry in `x` / `y` is a cell-local float coordinate in [0, 63.999]; the
/// vectors drive bilinear sampling without recomputing the cell-to-atlas mapping
/// once per pixel.
pub(crate) struct SampleGrid {
    /// Cell-local sample X positions, one per output pixel column.
    pub(crate) x: Vec<f32>,
    /// Cell-local sample Y positions, one per output pixel row.
    pub(crate) y: Vec<f32>,
}

/// A pair of wrapping texture indices and interpolation fraction for one axis sample.
#[derive(Clone, Copy)]
pub(crate) struct RawSampleAxis {
    /// Lower texel index (wrapping).
    pub(crate) i0: usize,
    /// Upper texel index (wrapping, `i0 + 1 mod extent`).
    pub(crate) i1: usize,
    /// Linear interpolation factor in [0, 1).
    pub(crate) t: f32,
}

/// One MIP level of a prepared texture, with sample axes pre-mapped to the output grid.
pub(crate) struct PreparedTextureLevel<'a> {
    /// Pixel data as `[u8; 4]` RGBA quads.
    pub(crate) pixels: &'a [[u8; 4]],
    /// Width of this MIP level in texels.
    pub(crate) width: usize,
    /// Pre-mapped sample axes for each output column.
    pub(crate) x: SmallVec<[RawSampleAxis; PRECOMPUTED_AXIS_INLINE_CAPACITY]>,
    /// Pre-mapped sample axes for each output row.
    pub(crate) y: SmallVec<[RawSampleAxis; PRECOMPUTED_AXIS_INLINE_CAPACITY]>,
}

/// A terrain texture slot prepared for LOD-blended bilinear sampling over the output grid.
pub(crate) struct PreparedTextureSlot<'a> {
    /// Fractional blend weight between `level0` and `level1`; 0.0 means use `level0` only.
    pub(crate) frac: f32,
    /// The lower prepared MIP level.
    pub(crate) level0: PreparedTextureLevel<'a>,
    /// The upper prepared MIP level; present only when `frac > 0.0`.
    pub(crate) level1: Option<PreparedTextureLevel<'a>>,
}

impl SampleGrid {
    pub(crate) fn width(&self) -> u32 {
        self.x.len() as u32
    }

    pub(crate) fn height(&self) -> u32 {
        self.y.len() as u32
    }

    /// Returns the cell-coordinate step between adjacent output pixels on the X axis.
    pub(crate) fn x_step(&self) -> f32 {
        sample_coord_step(&self.x)
    }

    /// Returns the cell-coordinate step between adjacent output pixels on the Y axis.
    pub(crate) fn y_step(&self) -> f32 {
        sample_coord_step(&self.y)
    }
}

/// Returns the absolute step between consecutive sample coordinates, or 64.0 for a
/// single-pixel grid.
fn sample_coord_step(coords: &[f32]) -> f32 {
    if coords.len() >= 2 {
        (coords[1] - coords[0]).abs()
    } else {
        64.0
    }
}

/// Prepares one texture slot for LOD-blended bilinear sampling over `grid`.
///
/// Only the one or two mip levels selected by the LOD are pre-mapped to the grid's
/// sample axes so that rendering can walk pre-resolved `(i0, i1, t)` triplets
/// instead of recomputing them per pixel.
pub(crate) fn prepare_texture_slot<'a>(texture: &'a TerrainTexture, grid: &SampleGrid) -> PreparedTextureSlot<'a> {
    let max_level = texture.levels.len().saturating_sub(1);
    let lod = terrain_texture_lod_for_grid(texture, grid).clamp(0.0, max_level as f32);
    let level0 = lod.floor() as usize;
    let level1 = (level0 + 1).min(max_level);
    let raw_frac = lod - level0 as f32;
    let frac = if level1 == level0 || raw_frac <= 0.0 { 0.0 } else { raw_frac };

    let prepare_level = |index: usize| {
        let image = &texture.levels[index];
        let (pixels, _) = image.as_raw().as_chunks::<4>();
        PreparedTextureLevel {
            pixels,
            width: image.width() as usize,
            x: build_raw_sample_axes(&grid.x, image.width()),
            y: build_raw_sample_axes(&grid.y, image.height()),
        }
    };

    PreparedTextureSlot {
        frac,
        level0: prepare_level(level0),
        level1: (frac > 0.0).then(|| prepare_level(level1)),
    }
}

/// Pre-maps a set of cell-local coordinates to repeating-texture sample axes.
///
/// The input `coords` are in [0, 63.999]; dividing by 4.0 converts to texture-space
/// so that each terrain "quad" (4 cell units) maps to one full texture tile.
pub(crate) fn build_raw_sample_axes(
    coords: &[f32],
    extent: u32,
) -> SmallVec<[RawSampleAxis; PRECOMPUTED_AXIS_INLINE_CAPACITY]> {
    coords.iter().map(|coord| raw_sample_axis(*coord / 4.0, extent)).collect()
}

/// Computes a repeating-texture [`RawSampleAxis`] for a single coordinate.
fn raw_sample_axis(coord: f32, extent: u32) -> RawSampleAxis {
    let extent_i32 = extent as i32;
    let f = coord.rem_euclid(1.0) * extent as f32 - 0.5;
    let i0 = f.floor() as i32;
    RawSampleAxis {
        i0: i0.rem_euclid(extent_i32) as usize,
        i1: (i0 + 1).rem_euclid(extent_i32) as usize,
        t: f - i0 as f32,
    }
}

#[inline(always)]
pub(crate) fn lerp4(a: Vec4, b: Vec4, t: f32) -> Vec4 {
    a * (1.0 - t) + b * t
}

/// Converts an `[u8; 4]` RGBA pixel to a `Vec4` of `f32` values.
///
/// # Safety
///
/// On `x86_64` with the `sse4.1` target feature, this function uses
/// `_mm_cvtepu8_epi32` + `_mm_cvtepi32_ps` intrinsics.  The cfg gate ensures these
/// intrinsics are only emitted when the CPU supports SSE4.1, so the `unsafe` block
/// is sound for any binary compiled with `-C target-cpu=native` or equivalent.
#[cfg(all(target_arch = "x86_64", target_feature = "sse4.1"))]
#[inline(always)]
fn pixel_to_f32(p: [u8; 4]) -> Vec4 {
    use core::arch::x86_64::*;
    // SAFETY: `sse4.1` target feature is verified at compile time by the cfg guard.
    unsafe {
        let bytes = _mm_cvtsi32_si128(i32::from_ne_bytes(p));
        let ints = _mm_cvtepu8_epi32(bytes);
        Vec4::from(_mm_cvtepi32_ps(ints))
    }
}

#[cfg(not(all(target_arch = "x86_64", target_feature = "sse4.1")))]
#[inline(always)]
fn pixel_to_f32(p: [u8; 4]) -> Vec4 {
    Vec4::new(p[0] as f32, p[1] as f32, p[2] as f32, p[3] as f32)
}

/// Fills `out` with bilinearly-sampled `Vec4` pixels for row `py` of a single mip level.
/// Equivalent to calling `sample_precomputed_level` for each `px`, but walks the source
/// row pair once for cache locality. `out.len()` must equal `level.x.len()`.
pub(crate) fn sample_precomputed_level_row(level: &PreparedTextureLevel<'_>, py: usize, out: &mut [Vec4]) {
    debug_assert_eq!(out.len(), level.x.len());

    let y = level.y[py];
    let pixels = level.pixels;
    let w = level.width;
    let row0_start = y.i0 * w;
    let row1_start = y.i1 * w;
    let yt = y.t;

    debug_assert!(row0_start + w <= pixels.len());
    debug_assert!(row1_start + w <= pixels.len());

    let row0 = unsafe { pixels.get_unchecked(row0_start..row0_start + w) };
    let row1 = unsafe { pixels.get_unchecked(row1_start..row1_start + w) };

    for (dst, x) in out.iter_mut().zip(level.x.iter().copied()) {
        debug_assert!(x.i0 < w);
        debug_assert!(x.i1 < w);

        let c00 = pixel_to_f32(unsafe { *row0.get_unchecked(x.i0) });
        let c10 = pixel_to_f32(unsafe { *row0.get_unchecked(x.i1) });
        let c01 = pixel_to_f32(unsafe { *row1.get_unchecked(x.i0) });
        let c11 = pixel_to_f32(unsafe { *row1.get_unchecked(x.i1) });

        let top = lerp4(c00, c10, x.t);
        let bot = lerp4(c01, c11, x.t);
        *dst = lerp4(top, bot, yt);
    }
}

/// Row-form of [`sample_precomputed_texture_lod`]. Fills `out` with LOD-blended samples
/// for row `py`. `scratch` is used only when an LOD blend is required, and must be the
/// same length as `out`; callers should reuse it across rows.
pub(crate) fn sample_precomputed_texture_lod_row(
    slot: &PreparedTextureSlot<'_>,
    py: usize,
    out: &mut [Vec4],
    scratch: &mut [Vec4],
) {
    sample_precomputed_level_row(&slot.level0, py, out);
    let Some(level1) = &slot.level1 else {
        return;
    };
    debug_assert_eq!(scratch.len(), out.len());
    sample_precomputed_level_row(level1, py, scratch);
    let frac = slot.frac;
    for (dst, src) in out.iter_mut().zip(scratch.iter().copied()) {
        *dst = lerp4(*dst, src, frac);
    }
}
