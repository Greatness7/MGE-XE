use image::{Rgba, RgbaImage};
use intel_tex_2::RgbaSurface;

use super::*;

#[test]
fn legacy_d3d_header_uses_width_first_order_and_optional_mip_count() {
    let single_mip = legacy_d3d_header(144, 108, D3DFormat::DXT1, 1).unwrap();
    assert_eq!(single_mip.width, 144);
    assert_eq!(single_mip.height, 108);
    assert_eq!(single_mip.mip_map_count, None);

    let mip_chain = legacy_d3d_header(144, 108, D3DFormat::DXT1, 4).unwrap();
    assert_eq!(mip_chain.mip_map_count, Some(4));
}

#[test]
fn encode_d3d_bcn_dds_with_mips_rejects_non_bcn_formats() {
    let img = test_image(8, 8);

    for format in [D3DFormat::A8R8G8B8, D3DFormat::DXT3, D3DFormat::DXT4] {
        let err = encode_d3d_bcn_dds_with_mips(&img, format).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    // The two supported formats still encode, at their respective block sizes.
    for (format, block_bytes) in [(D3DFormat::DXT1, 8), (D3DFormat::DXT5, 16)] {
        let bytes = encode_d3d_bcn_dds_with_mips(&img, format).unwrap();
        let expected = mip_capacity_hint(8, 8, |width, height| bcn_level_bytes(width, height, block_bytes));
        assert_eq!(bytes.len(), expected);
    }
}

fn test_image(width: u32, height: u32) -> RgbaImage {
    let mut img = RgbaImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let r = (x.wrapping_mul(37) + y.wrapping_mul(11) + 19) as u8;
            let g = (x.wrapping_mul(7) + y.wrapping_mul(41) + 83) as u8;
            let b = (x.wrapping_mul(23) + y.wrapping_mul(3) + 151) as u8;
            let a = 255_u8.saturating_sub((x.wrapping_mul(13) + y.wrapping_mul(17)) as u8 / 2);
            img.put_pixel(x, y, Rgba([r, g, b, a]));
        }
    }
    img
}

fn reference_pad_to_block_grid(img: &RgbaImage) -> RgbaImage {
    let width = img.width() as usize;
    let height = img.height() as usize;
    let padded_width = img.width().next_multiple_of(4) as usize;
    let padded_height = img.height().next_multiple_of(4) as usize;

    let src = img.as_raw();
    let src_stride = width * 4;
    let dst_stride = padded_width * 4;
    let mut out = vec![0u8; dst_stride * padded_height];

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

    RgbaImage::from_raw(padded_width as u32, padded_height as u32, out).expect("padded buffer matches dimensions")
}

fn reference_encode_bcn_allocating(
    img: &RgbaImage,
    block_bytes: usize,
    compress: impl Fn(&RgbaSurface, &mut [u8]) + Sync,
) -> Vec<u8> {
    let aligned;
    let img = if (img.width().is_multiple_of(4) && img.height().is_multiple_of(4)) || (img.width() <= 4 && img.height() <= 4)
    {
        img
    } else {
        aligned = reference_pad_to_block_grid(img);
        &aligned
    };

    let width = img.width();
    let height = img.height();

    let blocks_x = width.div_ceil(4) as usize;
    let blocks_y = height.div_ceil(4) as usize;
    let total_blocks = blocks_x * blocks_y;
    let mut out = vec![0u8; total_blocks * block_bytes];

    let stride = width as usize * 4;
    let band_height_pixels = 64.min(height) as usize;
    let band_height_blocks = band_height_pixels.div_ceil(4);

    let pixel_data = img.as_raw();

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

    out
}

#[test]
fn encode_bc1_into_matches_allocating_reference_encoder() {
    for (width, height) in [(1, 1), (2, 2), (3, 4), (4, 3), (8, 8), (7, 5), (5, 2)] {
        let img = test_image(width, height);
        let expected = reference_encode_bcn_allocating(&img, 8, intel_tex_2::bc1::compress_blocks_into);
        let mut actual = vec![0u8; bcn_level_bytes(width, height, 8)];
        let mut pad = Vec::new();

        encode_bc1_into(&img, &mut pad, &mut actual);

        assert_eq!(actual, expected, "BC1 mismatch for {width}x{height}");
    }
}

#[test]
fn encode_bc3_into_matches_allocating_reference_encoder() {
    for (width, height) in [(1, 1), (3, 4), (4, 3), (8, 8), (7, 5)] {
        let img = test_image(width, height);
        let expected = reference_encode_bcn_allocating(&img, 16, intel_tex_2::bc3::compress_blocks_into);
        let mut actual = vec![0u8; bcn_level_bytes(width, height, 16)];
        let mut pad = Vec::new();

        encode_bc3_into(&img, &mut pad, &mut actual);

        assert_eq!(actual, expected, "BC3 mismatch for {width}x{height}");
    }
}

#[test]
#[should_panic]
fn encode_bc1_region_rejects_unaligned_dimensions() {
    let img = test_image(6, 4);
    let mut dst = vec![0u8; 16];

    encode_bc1_into_region(&img, &mut dst, 16, 0);
}

#[test]
#[should_panic]
fn encode_bc1_region_rejects_wrong_region_length() {
    let img = test_image(8, 8);
    let mut dst = vec![0u8; 16];

    encode_bc1_into_region(&img, &mut dst, 16, 0);
}

#[test]
#[should_panic]
fn encode_bc1_region_rejects_columns_past_pitch() {
    let img = test_image(8, 4);
    let mut dst = vec![0u8; 16];

    encode_bc1_into_region(&img, &mut dst, 16, 8);
}
