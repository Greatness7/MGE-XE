//! Tests for the BC1 source-atlas builder (`package::atlas`).

use intel_tex_2::RgbaSurface;
use rayon::prelude::*;

use super::*;

fn make_bc1_block_pattern_image() -> RgbaImage {
    let colors = [
        Rgba([255, 0, 0, 255]),
        Rgba([0, 255, 0, 255]),
        Rgba([0, 0, 255, 255]),
        Rgba([255, 255, 0, 255]),
    ];
    let mut image = RgbaImage::new(8, 8);
    for y in 0..8 {
        for x in 0..8 {
            let block = (x / 4) + (y / 4) * 2;
            image.put_pixel(x, y, colors[block as usize]);
        }
    }
    image
}

fn make_bc1_texture(image: RgbaImage) -> TerrainTexture {
    let blocks = encode_bc1(&image);
    TerrainTexture {
        levels: smallvec::smallvec![image],
        bc1_mips: smallvec::smallvec![Some(Bc1Mip {
            width: 8,
            height: 8,
            blocks,
        })],
        bytes_hash: [0; 32],
        pending: None,
    }
}

fn make_bc1_compatible_block(seed: u8) -> [u8; 8] {
    [
        0x00,
        0xf8,
        0xe0,
        0x07,
        seed,
        seed.wrapping_add(1),
        seed.wrapping_add(2),
        seed.wrapping_add(3),
    ]
}

fn make_bc1_compatible_texture(blocks: Vec<u8>) -> TerrainTexture {
    TerrainTexture {
        levels: smallvec::smallvec![RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 255]))],
        bc1_mips: smallvec::smallvec![Some(Bc1Mip {
            width: 8,
            height: 8,
            blocks,
        })],
        bytes_hash: [0; 32],
        pending: None,
    }
}

fn make_texture_cache_with_texture<'a>(default_key: &'a str, texture: TerrainTexture) -> TerrainTextureCache<'a> {
    let mut images: IndexMap<&'a str, TerrainTexture> = Default::default();
    images.insert(default_key, texture.clone());
    TerrainTextureCache { images, default_key }
}

fn single_texture_ids<'a>(path: &'a str) -> TerrainTextureIds<'a> {
    let mut ids_by_path = HashMap::new();
    ids_by_path.insert(path, 0);
    TerrainTextureIds {
        ordered_paths: vec![path],
        ids_by_path,
    }
}

fn multiple_texture_ids<'a>(paths: Vec<&'a str>) -> TerrainTextureIds<'a> {
    let mut ids_by_path = HashMap::new();
    for (index, path) in paths.iter().copied().enumerate() {
        ids_by_path.insert(path, index as u16);
    }
    TerrainTextureIds {
        ordered_paths: paths,
        ids_by_path,
    }
}

fn test_bc1_atlas_spec(gutter_size: u32) -> TerrainAtlasSpec {
    TerrainAtlasSpec {
        logical_tile_size: 8,
        gutter_size,
        physical_tile_size: 8 + gutter_size * 2,
        tiles_per_row: 1,
        atlas_size: 8 + gutter_size * 2,
        atlas_max_lod: 0,
    }
}

fn bc1_block(blocks: &[u8], blocks_x: u32, x: u32, y: u32) -> &[u8] {
    let offset = ((y * blocks_x + x) * 8) as usize;
    &blocks[offset..offset + 8]
}

fn make_bc1_fallback_image(seed: u8) -> RgbaImage {
    let mut image = RgbaImage::new(8, 8);
    for y in 0..8 {
        for x in 0..8 {
            image.put_pixel(
                x,
                y,
                Rgba([
                    seed.wrapping_add((x * 19) as u8),
                    seed.wrapping_mul(3).wrapping_add((y * 29) as u8),
                    seed.wrapping_add(((x + y) * 13) as u8),
                    255,
                ]),
            );
        }
    }
    image
}

fn reference_wrapped_tile_image(tile: &RgbaImage, gutter_size: u32) -> RgbaImage {
    let logical_tile_size = tile.width();
    let physical_tile_size = logical_tile_size + gutter_size * 2;
    let mut wrapped = RgbaImage::from_pixel(physical_tile_size, physical_tile_size, Rgba([0, 0, 0, 255]));
    for y in 0..physical_tile_size {
        let source_y = (y + logical_tile_size - gutter_size) % logical_tile_size;
        for x in 0..physical_tile_size {
            let source_x = (x + logical_tile_size - gutter_size) % logical_tile_size;
            wrapped.put_pixel(x, y, *tile.get_pixel(source_x, source_y));
        }
    }
    wrapped
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

fn reference_encode_bc1(img: &RgbaImage) -> Vec<u8> {
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
    let mut out = vec![0u8; total_blocks * 8];

    let stride = width as usize * 4;
    let band_height_pixels = 64.min(height) as usize;
    let band_height_blocks = band_height_pixels.div_ceil(4);

    let pixel_data = img.as_raw();

    out.par_chunks_mut(band_height_blocks * blocks_x * 8)
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

            intel_tex_2::bc1::compress_blocks_into(&surface, out_chunk);
        });

    out
}

fn reference_scatter_encoded_tile_blocks(
    atlas_blocks: &mut [u8],
    atlas_blocks_x: u32,
    tile_x: u32,
    tile_y: u32,
    physical_tile_size: u32,
    tile_blocks: &[u8],
) {
    let physical_blocks = physical_tile_size / 4;
    for by in 0..physical_blocks {
        for bx in 0..physical_blocks {
            let source_offset = ((by * physical_blocks + bx) * 8) as usize;
            let atlas_block_x = tile_x * physical_blocks + bx;
            let atlas_block_y = tile_y * physical_blocks + by;
            let dest_offset = ((atlas_block_y * atlas_blocks_x + atlas_block_x) * 8) as usize;
            atlas_blocks[dest_offset..dest_offset + 8].copy_from_slice(&tile_blocks[source_offset..source_offset + 8]);
        }
    }
}

#[test]
fn block_copy_wraps_gutter_to_opposite_edge_blocks() {
    let source = make_bc1_block_pattern_image();
    let source_blocks = encode_bc1(&source);
    let cache = make_texture_cache_with_texture("base.dds", make_bc1_texture(source));
    let texture_ids = single_texture_ids("base.dds");
    let chain = build_terrain_atlas_bc1_chain(&texture_ids, &cache, test_bc1_atlas_spec(4));

    let atlas_blocks = &chain.mips[0];
    assert_eq!(bc1_block(atlas_blocks, 4, 0, 0), bc1_block(&source_blocks, 2, 1, 1));
    assert_eq!(bc1_block(atlas_blocks, 4, 3, 3), bc1_block(&source_blocks, 2, 0, 0));
}

#[test]
fn block_copy_preserves_aligned_source_blocks_bit_exact() {
    let source = make_bc1_block_pattern_image();
    let source_blocks = encode_bc1(&source);
    let cache = make_texture_cache_with_texture("base.dds", make_bc1_texture(source));
    let texture_ids = single_texture_ids("base.dds");
    let chain = build_terrain_atlas_bc1_chain(&texture_ids, &cache, test_bc1_atlas_spec(4));

    let atlas_blocks = &chain.mips[0];
    for source_y in 0..2 {
        for source_x in 0..2 {
            assert_eq!(
                bc1_block(atlas_blocks, 4, source_x + 1, source_y + 1),
                bc1_block(&source_blocks, 2, source_x, source_y)
            );
        }
    }
}

#[test]
fn bc1_compatible_blocks_use_direct_atlas_path() {
    let mut source_blocks = Vec::new();
    for seed in [1, 17, 33, 49] {
        source_blocks.extend_from_slice(&make_bc1_compatible_block(seed));
    }
    let cache = make_texture_cache_with_texture("base.dds", make_bc1_compatible_texture(source_blocks.clone()));
    let texture_ids = single_texture_ids("base.dds");
    let chain = build_terrain_atlas_bc1_chain(&texture_ids, &cache, test_bc1_atlas_spec(4));

    let atlas_blocks = &chain.mips[0];
    assert_eq!(bc1_block(atlas_blocks, 4, 1, 1), bc1_block(&source_blocks, 2, 0, 0));
    assert_eq!(bc1_block(atlas_blocks, 4, 2, 1), bc1_block(&source_blocks, 2, 1, 0));
    assert_eq!(bc1_block(atlas_blocks, 4, 1, 2), bc1_block(&source_blocks, 2, 0, 1));
    assert_eq!(bc1_block(atlas_blocks, 4, 2, 2), bc1_block(&source_blocks, 2, 1, 1));
}

#[test]
fn block_copy_refuses_non_aligned_gutters() {
    let source = make_bc1_block_pattern_image();
    let texture = make_bc1_texture(source);
    let bc1 = texture.bc1_mip_at_size(8).unwrap();

    assert!(!block_copy_eligible(bc1, 8, 1, 10));
}

#[test]
fn non_dds_texture_uses_decode_resample_fallback() {
    let cache = make_texture_cache_with_texture(
        "base.tga",
        TerrainTexture::from_base(RgbaImage::from_pixel(8, 8, Rgba([128, 64, 32, 255]))),
    );
    let texture_ids = single_texture_ids("base.tga");
    let chain = build_terrain_atlas_bc1_chain(&texture_ids, &cache, test_bc1_atlas_spec(4));

    assert_eq!(chain.width, 16);
    assert_eq!(chain.height, 16);
    assert_eq!(chain.max_lod, 0);
    assert_eq!(chain.mips.len(), 1);
    assert_eq!(chain.mips[0].len(), 4 * 4 * 8);
    assert!(chain.mips[0].iter().any(|&byte| byte != 0));
}

#[test]
fn encode_fallback_matches_per_tile_wrap_encode_scatter_reference() {
    let images = vec![
        ("a.tga", make_bc1_fallback_image(17)),
        ("b.tga", make_bc1_fallback_image(53)),
        ("c.tga", make_bc1_fallback_image(97)),
        ("d.tga", make_bc1_fallback_image(149)),
    ];
    let cache = make_texture_cache_from_images("a.tga", images.clone());
    let texture_ids = multiple_texture_ids(images.iter().map(|(path, _)| *path).collect());
    let atlas_spec = TerrainAtlasSpec {
        logical_tile_size: 8,
        gutter_size: 4,
        physical_tile_size: 16,
        tiles_per_row: 2,
        atlas_size: 32,
        atlas_max_lod: 0,
    };

    assert!(
        texture_ids
            .ordered_paths
            .iter()
            .all(|path| cache.get(path).bc1_mip_at_size(atlas_spec.logical_tile_size).is_none())
    );

    let chain = build_terrain_atlas_bc1_chain(&texture_ids, &cache, atlas_spec);
    let atlas_blocks_x = atlas_spec.atlas_size / 4;
    let atlas_blocks_y = atlas_spec.atlas_size / 4;
    let mut expected = vec![0_u8; atlas_blocks_x as usize * atlas_blocks_y as usize * 8];

    for (index, (_, image)) in images.iter().enumerate() {
        let tile_x = u32::try_from(index).unwrap() % atlas_spec.tiles_per_row;
        let tile_y = u32::try_from(index).unwrap() / atlas_spec.tiles_per_row;
        let wrapped = reference_wrapped_tile_image(image, atlas_spec.gutter_size);
        let tile_blocks = reference_encode_bc1(&wrapped);
        reference_scatter_encoded_tile_blocks(
            &mut expected,
            atlas_blocks_x,
            tile_x,
            tile_y,
            atlas_spec.physical_tile_size,
            &tile_blocks,
        );
    }

    assert_eq!(chain.mips[0], expected);
}

#[test]
fn choose_tile_size_uses_physical_tile_capacity() {
    let atlas = choose_source_atlas(785, 8192, 512).unwrap();
    assert_eq!(atlas.logical_tile_size, 128);
    assert_eq!(atlas.physical_tile_size, 144);
    assert_eq!(atlas.atlas_max_lod, 1);
}

#[test]
fn choose_source_atlas_honors_configured_tile_size_limits() {
    let ultra_tile = choose_source_atlas(1, 8192, 512).unwrap();
    assert_eq!(ultra_tile.logical_tile_size, 512);
    assert_eq!(ultra_tile.gutter_size, 32);
    assert_eq!(ultra_tile.physical_tile_size, 576);
    assert_eq!(ultra_tile.atlas_max_lod, 3);

    let high_tile = choose_source_atlas(1, 8192, 256).unwrap();
    assert_eq!(high_tile.logical_tile_size, 256);
    assert_eq!(high_tile.gutter_size, 16);
    assert_eq!(high_tile.physical_tile_size, 288);
    assert_eq!(high_tile.atlas_max_lod, 2);

    let low_tile = choose_source_atlas(1, 8192, 64).unwrap();
    assert_eq!(low_tile.logical_tile_size, 64);
    assert_eq!(low_tile.gutter_size, 4);
    assert_eq!(low_tile.physical_tile_size, 72);
    assert_eq!(low_tile.atlas_max_lod, 0);
}

#[test]
fn choose_source_atlas_downshifts_when_requested_max_cannot_fit() {
    let downshifted = choose_source_atlas(197, 8192, 512).unwrap();
    assert_eq!(downshifted.logical_tile_size, 256);

    let smaller = choose_source_atlas(3_137, 8192, 512).unwrap();
    assert_eq!(smaller.logical_tile_size, 64);
}
