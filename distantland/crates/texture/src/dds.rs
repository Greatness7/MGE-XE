//! Shared DDS encoding primitives used by terrain DDS outputs and the static
//! texture atlas.
//!
//! D3DX9 / MGE-XE require legacy (non-DX10) DDS headers with a full mip chain.
//! `image_dds` produces correct output but is entirely single-threaded; these
//! helpers parallelise the 2×2 box mip generation and the `intel-tex-2` BCn
//! block compression that dominate encode time.

use std::io::Write;

use image::RgbaImage;
use image_dds::ddsfile::{D3DFormat, Header};
use intel_tex_2::RgbaSurface;
use rayon::prelude::*;

/// Returns the dimensions of the mip level below `(width, height)`.
pub fn next_mip_dimensions(width: u32, height: u32) -> (u32, u32) {
    ((width / 2).max(1), (height / 2).max(1))
}

/// Returns the number of mip levels from `(width, height)` down to 1×1.
pub fn mipmap_count(width: u32, height: u32) -> u32 {
    width.max(height).max(1).ilog2() + 1
}

/// Sums per-level byte sizes (from the 128-byte header down to 1×1) using `level_bytes`.
pub fn mip_capacity_hint(width: u32, height: u32, level_bytes: impl Fn(u32, u32) -> usize) -> usize {
    let mut capacity = 128;
    let mut width = width.max(1);
    let mut height = height.max(1);

    loop {
        capacity += level_bytes(width, height);
        if width == 1 && height == 1 {
            return capacity;
        }
        (width, height) = next_mip_dimensions(width, height);
    }
}

/// Returns the compressed byte size of one BCn mip level.
///
/// `block_bytes` is the compressed size of one 4x4 block: 8 for BC1 or 16 for BC3.
pub fn bcn_level_bytes(width: u32, height: u32, block_bytes: usize) -> usize {
    width.div_ceil(4) as usize * height.div_ceil(4) as usize * block_bytes
}

/// Builds a legacy D3D9 DDS header using the conventional `(width, height)` argument order.
///
/// # Errors
///
/// Returns the underlying `ddsfile` error if the header cannot be constructed.
pub fn legacy_d3d_header(
    width: u32,
    height: u32,
    d3d_format: D3DFormat,
    mip_count: u32,
) -> Result<Header, image_dds::ddsfile::Error> {
    Header::new_d3d(height, width, None, d3d_format, (mip_count > 1).then_some(mip_count), None)
}

/// Writes the 128-byte legacy DDS header (4-byte magic + 124-byte `DDS_HEADER`) to `writer`.
pub fn write_legacy_dds_header(writer: &mut impl Write, header: &Header) -> std::io::Result<u64> {
    writer.write_all(b"DDS ")?;
    header.write(writer).map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(128)
}

/// Downsamples `src` by 2× using a parallel per-channel 2×2 box filter in byte space.
///
/// This is the fast DDS mip-path filter used for generated atlas and terrain
/// DDS outputs. Source terrain textures that need color-correct sampling use
/// the separate sRGB-correct filter in `terrain::texture::texture_cache`.
pub fn downsample_2x2_rgba8_parallel(src: &RgbaImage) -> RgbaImage {
    let width = src.width();
    let height = src.height();
    let (next_width, next_height) = next_mip_dimensions(width, height);

    let mut out = RgbaImage::new(next_width, next_height);

    let src_bytes = src.as_raw();
    let src_stride = width as usize * 4;

    out.par_chunks_mut(next_width as usize * 4)
        .enumerate()
        .for_each(|(y, out_row)| {
            let sy = y * 2;
            let src_row0 = &src_bytes[sy * src_stride..(sy + 1) * src_stride];
            let src_row1 = if sy + 1 < height as usize {
                &src_bytes[(sy + 1) * src_stride..(sy + 2) * src_stride]
            } else {
                src_row0
            };

            for x in 0..next_width as usize {
                let sx = x * 2;
                let sx1 = if sx + 1 < width as usize { sx + 1 } else { sx };

                let p00_offset = sx * 4;
                let p01_offset = sx1 * 4;

                for c in 0..4 {
                    let v00 = src_row0[p00_offset + c] as u32;
                    let v01 = src_row0[p01_offset + c] as u32;
                    let v10 = src_row1[p00_offset + c] as u32;
                    let v11 = src_row1[p01_offset + c] as u32;
                    out_row[x * 4 + c] = ((v00 + v01 + v10 + v11 + 2) / 4) as u8;
                }
            }
        });

    out
}

/// The block-compressed formats these DDS helpers can produce.
///
/// `D3DFormat` covers dozens of legacy surface formats; only these two are BCn
/// formats this module knows how to encode. Resolving the caller's `D3DFormat`
/// into this enum once, at the public boundary, keeps every downstream step from
/// having to re-decide. This makes an unsupported format unrepresentable rather
/// than silently falling through to BC3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BcnCodec {
    /// BC1 / DXT1: 8 bytes per 4×4 block, 1-bit alpha.
    Bc1,
    /// BC3 / DXT5: 16 bytes per 4×4 block, interpolated alpha.
    Bc3,
}

impl BcnCodec {
    /// Resolves the legacy D3D surface format the caller asked for into a codec.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for any format this module cannot encode.
    fn from_d3d_format(d3d_format: D3DFormat) -> std::io::Result<Self> {
        match d3d_format {
            D3DFormat::DXT1 => Ok(Self::Bc1),
            D3DFormat::DXT5 => Ok(Self::Bc3),
            other => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("unsupported D3DFormat for BCn encoding: {other:?}"),
            )),
        }
    }

    /// Returns the compressed size of one 4×4 block in this codec.
    fn block_bytes(self) -> usize {
        match self {
            Self::Bc1 => 8,
            Self::Bc3 => 16,
        }
    }

    /// Encodes `img` into `out`, which must be exactly one mip level's worth of blocks.
    fn encode_into(self, img: &RgbaImage, pad: &mut Vec<u8>, out: &mut [u8]) {
        match self {
            Self::Bc1 => encode_bc1_into(img, pad, out),
            Self::Bc3 => encode_bc3_into(img, pad, out),
        }
    }
}

/// Encodes a complete BC1 (DXT1) or BC3 (DXT5) legacy-DDS mip chain into bytes.
///
/// # Errors
///
/// Returns an error if `d3d_format` is not a supported BCn format, or if the
/// legacy DDS header cannot be constructed or written.
pub fn encode_d3d_bcn_dds_with_mips(rgba: &RgbaImage, d3d_format: D3DFormat) -> std::io::Result<Vec<u8>> {
    // Reject unsupported formats before building a header we could not fill.
    let codec = BcnCodec::from_d3d_format(d3d_format)?;
    let width = rgba.width();
    let height = rgba.height();
    let mip_count = mipmap_count(width, height);
    let header =
        legacy_d3d_header(width, height, d3d_format, mip_count).map_err(|e| std::io::Error::other(e.to_string()))?;

    let block_bytes = codec.block_bytes();
    let mut bytes = Vec::with_capacity(mip_capacity_hint(width, height, |width, height| {
        bcn_level_bytes(width, height, block_bytes)
    }));
    write_legacy_dds_header(&mut bytes, &header)?;
    write_bcn_mip_chain(&mut bytes, rgba, codec);
    Ok(bytes)
}

/// Appends a full BCn mip chain, from base level down to 1×1, to `out`, with
/// parallel 2×2 byte-space mip generation and parallel block compression.
fn write_bcn_mip_chain(out: &mut Vec<u8>, base: &RgbaImage, codec: BcnCodec) {
    let chain_span = tracing::info_span!(
        "dds.write_bcn_mip_chain",
        report = true,
        mip_count = mipmap_count(base.width(), base.height()) as u64,
        bytes = tracing::field::Empty
    );
    let _chain_guard = chain_span.enter();

    let start_len = out.len();
    let mut owned: Option<RgbaImage> = None;
    let mut mip_level = 0_u32;
    let block_bytes = codec.block_bytes();
    let mut pad = Vec::new();
    loop {
        let img = owned.as_ref().unwrap_or(base);
        let width = img.width();
        let height = img.height();
        {
            let compress_span = tracing::info_span!(
                "dds.compress_bcn_mip",
                report = true,
                mip_level = mip_level as u64,
                width = width as u64,
                height = height as u64,
                bytes = tracing::field::Empty
            );
            let _guard = compress_span.enter();
            let size = bcn_level_bytes(width, height, block_bytes);
            let start = out.len();
            out.resize(start + size, 0);
            codec.encode_into(img, &mut pad, &mut out[start..start + size]);
            compress_span.record("bytes", size as u64);
        }

        if width == 1 && height == 1 {
            break;
        }

        let (next_width, next_height) = next_mip_dimensions(width, height);
        let next = {
            let _guard = tracing::info_span!(
                "dds.downsample_byte_space_mip",
                report = true,
                mip_level = mip_level as u64,
                width = width as u64,
                height = height as u64,
                next_width = next_width as u64,
                next_height = next_height as u64,
                filter = "box_2x2_byte_space"
            )
            .entered();
            downsample_2x2_rgba8_parallel(img)
        };
        owned = Some(next);
        mip_level += 1;
    }
    chain_span.record("bytes", (out.len() - start_len) as u64);
}

/// Encodes `img` into an existing BC1 (DXT1) output buffer.
///
/// `pad` is reusable scratch storage for non-block-aligned images. `out` must be
/// exactly `bcn_level_bytes(img.width(), img.height(), 8)` bytes long.
///
/// # Panics
///
/// Panics if `out` is not exactly the required BC1 byte size.
pub fn encode_bc1_into(img: &RgbaImage, pad: &mut Vec<u8>, out: &mut [u8]) {
    encode_bcn_into(img, 8, intel_tex_2::bc1::compress_blocks_into, pad, out);
}

/// Encodes `img` into an existing BC3 (DXT5) output buffer.
///
/// `pad` is reusable scratch storage for non-block-aligned images. `out` must be
/// exactly `bcn_level_bytes(img.width(), img.height(), 16)` bytes long.
///
/// # Panics
///
/// Panics if `out` is not exactly the required BC3 byte size.
pub fn encode_bc3_into(img: &RgbaImage, pad: &mut Vec<u8>, out: &mut [u8]) {
    encode_bcn_into(img, 16, intel_tex_2::bc3::compress_blocks_into, pad, out);
}

/// Encodes an aligned `img` directly into a BC1 region inside a strided block grid.
///
/// `dst` must be the exact row span for the destination region, not the whole
/// atlas buffer. Its length must equal `(img.height() / 4) * dst_row_pitch`, and
/// each tile row is written at `dst_col_offset..dst_col_offset + (img.width() / 4) * 8`.
///
/// Test-only owned-image wrapper over [`encode_bc1_rgba_surface_into_region`],
/// which is what the terrain atlas packer calls.
///
/// # Panics
///
/// Panics if `img` is not 4x4-block-aligned, if `dst` does not match the exact
/// region row span, or if the encoded row would extend past `dst_row_pitch`.
#[cfg(test)]
pub fn encode_bc1_into_region(img: &RgbaImage, dst: &mut [u8], dst_row_pitch: usize, dst_col_offset: usize) {
    encode_bc1_rgba_surface_into_region(
        img.as_raw(),
        img.width(),
        img.height(),
        img.width() as usize * 4,
        dst,
        dst_row_pitch,
        dst_col_offset,
    );
}

/// Encodes an aligned RGBA surface directly into a BC1 region inside a strided block grid.
///
/// `pixels` must contain at least `height` rows with `stride` bytes per row, and
/// the first `width * 4` bytes of each row must be RGBA8 data. `dst` must be the
/// exact row span for the destination region, not the whole atlas buffer.
///
/// # Panics
///
/// Panics if the surface is not 4x4-block-aligned, if the source or destination
/// buffers are too small, or if the encoded row would extend past `dst_row_pitch`.
pub fn encode_bc1_rgba_surface_into_region(
    pixels: &[u8],
    width: u32,
    height: u32,
    stride: usize,
    dst: &mut [u8],
    dst_row_pitch: usize,
    dst_col_offset: usize,
) {
    encode_bcn_rgba_surface_into_region(
        pixels,
        width,
        height,
        stride,
        8,
        intel_tex_2::bc1::compress_blocks_into,
        dst,
        dst_row_pitch,
        dst_col_offset,
    );
}

/// Reports whether `img` must be padded out to the 4×4 block grid before encoding.
///
/// Padding is needed only for surfaces that are both unaligned *and* larger than
/// a single block: a sub-block surface already occupies exactly one block, which
/// the BCn kernels handle directly.
fn needs_bcn_padding(img: &RgbaImage) -> bool {
    let block_aligned = img.width().is_multiple_of(4) && img.height().is_multiple_of(4);
    let single_block = img.width() <= 4 && img.height() <= 4;
    !block_aligned && !single_block
}

/// Pads `img` into `out` up to 4x4-block-aligned dimensions, with edge pixels
/// replicated into the new rows and columns.
fn pad_to_block_grid_into(img: &RgbaImage, out: &mut Vec<u8>) -> (u32, u32, usize) {
    let width = img.width() as usize;
    let height = img.height() as usize;
    let padded_width = img.width().next_multiple_of(4) as usize;
    let padded_height = img.height().next_multiple_of(4) as usize;

    let src = img.as_raw();
    let src_stride = width * 4;
    let dst_stride = padded_width * 4;
    let needed = dst_stride * padded_height;
    if out.len() < needed {
        out.resize(needed, 0);
    }

    for y in 0..padded_height {
        let src_y = y.min(height - 1);
        let src_row = &src[src_y * src_stride..src_y * src_stride + src_stride];
        let dst_row = &mut out[y * dst_stride..y * dst_stride + dst_stride];
        dst_row[..src_stride].copy_from_slice(src_row);
        let edge = &src_row[src_stride - 4..src_stride];
        for x in width..padded_width {
            dst_row[x * 4..x * 4 + 4].copy_from_slice(edge);
        }
    }

    (padded_width as u32, padded_height as u32, needed)
}

/// Encodes `img` into BCn blocks, parallelised over 64-pixel-tall bands.
///
/// `block_bytes` is the compressed size of one 4×4 block (8 for BC1, 16 for BC3).
fn encode_bcn_into(
    img: &RgbaImage,
    block_bytes: usize,
    compress: impl Fn(&RgbaSurface, &mut [u8]) + Sync,
    pad: &mut Vec<u8>,
    out: &mut [u8],
) {
    // Hard assert, not debug_assert: a short `out` would let the block compressor
    // write past the slice in release builds.
    assert_eq!(
        out.len(),
        bcn_level_bytes(img.width(), img.height(), block_bytes),
        "BCn output buffer must be exactly one mip level's worth of blocks"
    );

    // intel-tex's BCn kernels yield a clean geometric block grid only for
    // surfaces with 4×4-block-aligned dimensions. A non-aligned surface larger
    // than a single block must be padded; a sub-block surface is always one
    // block and needs none.
    if needs_bcn_padding(img) {
        let (width, height, len) = pad_to_block_grid_into(img, pad);
        encode_bcn_pixels_into(&pad[..len], width, height, block_bytes, &compress, out);
    } else {
        encode_bcn_pixels_into(img.as_raw(), img.width(), img.height(), block_bytes, &compress, out);
    }
}

fn encode_bcn_pixels_into(
    pixel_data: &[u8],
    width: u32,
    height: u32,
    block_bytes: usize,
    compress: &(impl Fn(&RgbaSurface, &mut [u8]) + Sync),
    out: &mut [u8],
) {
    let blocks_x = width.div_ceil(4) as usize;
    let stride = width as usize * 4;
    let band_height_pixels = 64.min(height) as usize;
    let band_height_blocks = band_height_pixels.div_ceil(4);

    out.par_chunks_mut(band_height_blocks * blocks_x * block_bytes)
        .enumerate()
        .for_each(|(band_idx, out_chunk)| {
            let y_start = band_idx * band_height_pixels;
            let current_band_height = (height as usize - y_start).min(band_height_pixels);
            let data_start = y_start * stride;
            let data_slice = &pixel_data[data_start..];

            let surface = RgbaSurface {
                data: data_slice,
                width,
                height: current_band_height as u32,
                stride: stride as u32,
            };

            compress(&surface, out_chunk);
        });
}

fn encode_bcn_rgba_surface_into_region(
    pixels: &[u8],
    width: u32,
    height: u32,
    stride: usize,
    block_bytes: usize,
    compress: impl Fn(&RgbaSurface, &mut [u8]) + Sync,
    dst: &mut [u8],
    dst_row_pitch: usize,
    dst_col_offset: usize,
) {
    assert!(width > 0);
    assert!(height > 0);
    assert!(width.is_multiple_of(4));
    assert!(height.is_multiple_of(4));
    assert!(stride >= width as usize * 4);
    assert!(stride <= u32::MAX as usize);
    assert!(pixels.len() >= (height as usize - 1) * stride + width as usize * 4);

    let block_rows = height as usize / 4;
    let row_bytes = (width as usize / 4) * block_bytes;
    assert_eq!(dst.len(), block_rows * dst_row_pitch);
    assert!(dst_col_offset + row_bytes <= dst_row_pitch);

    let band_rows = 16;
    dst.par_chunks_mut(dst_row_pitch * band_rows)
        .enumerate()
        .for_each(|(band, dst_band)| {
            let first_by = band * band_rows;
            let rows_in_band = (block_rows - first_by).min(band_rows);
            for local_by in 0..rows_in_band {
                let by = first_by + local_by;
                let dst_row = &mut dst_band[local_by * dst_row_pitch..(local_by + 1) * dst_row_pitch];
                let surface = RgbaSurface {
                    data: &pixels[by * 4 * stride..],
                    width,
                    height: 4,
                    stride: stride as u32,
                };

                compress(&surface, &mut dst_row[dst_col_offset..dst_col_offset + row_bytes]);
            }
        });
}

#[cfg(test)]
mod tests;
