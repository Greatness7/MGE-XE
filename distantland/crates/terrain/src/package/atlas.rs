use super::*;

use crate::dds::encode_bc1_rgba_surface_into_region;
use crate::texture_io::resize_rgba_to_dimensions_into;

/// Pre-encoded BC1 mip chain for a terrain source atlas.
#[derive(Clone, Debug)]
pub struct Bc1MipChain {
    /// Width of the base mip in pixels.
    pub width: u32,
    /// Height of the base mip in pixels.
    pub height: u32,
    /// Deepest mip level stored in `mips`, where `0` means only the base mip.
    pub max_lod: u32,
    /// BC1 block payloads for mips `0..=max_lod`, in image orientation.
    pub mips: Vec<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TerrainAtlasSpec {
    pub(super) logical_tile_size: u32,
    pub(super) gutter_size: u32,
    pub(super) physical_tile_size: u32,
    pub(super) tiles_per_row: u32,
    pub(super) atlas_size: u32,
    pub(super) atlas_max_lod: u32,
}

#[derive(Default)]
struct TerrainAtlasBuildScratch {
    resized_tile: RgbaScratch,
    wrapped_tile: RgbaScratch,
}

#[derive(Default)]
struct RgbaScratch {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl RgbaScratch {
    fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.pixels.resize(width as usize * height as usize * 4, 0);
    }

    fn stride(&self) -> usize {
        self.width as usize * 4
    }

    fn view(&self) -> RgbaSurfaceView<'_> {
        let len = self.stride() * self.height as usize;
        RgbaSurfaceView {
            width: self.width,
            height: self.height,
            stride: self.stride(),
            pixels: &self.pixels[..len],
        }
    }
}

#[derive(Clone, Copy)]
struct RgbaSurfaceView<'a> {
    width: u32,
    height: u32,
    stride: usize,
    pixels: &'a [u8],
}

pub(super) fn choose_source_atlas(
    texture_count: usize,
    max_atlas_size: u32,
    max_logical_tile_size: u32,
) -> Result<TerrainAtlasSpec> {
    let texture_count = texture_count.max(1);

    for logical_tile_size in [512_u32, 256, 128, 64] {
        if logical_tile_size > max_logical_tile_size {
            continue;
        }
        let gutter_size = logical_tile_size / 16;
        let physical_tile_size = logical_tile_size + gutter_size * 2;
        let capacity_per_row = max_atlas_size / physical_tile_size;
        if texture_count > (capacity_per_row as usize).saturating_mul(capacity_per_row as usize) {
            continue;
        }

        let tiles_per_row = ceil_sqrt_u32(texture_count as u32);
        let atlas_size = tiles_per_row * physical_tile_size;
        if atlas_size > max_atlas_size {
            continue;
        }

        return Ok(TerrainAtlasSpec {
            logical_tile_size,
            gutter_size,
            physical_tile_size,
            tiles_per_row,
            atlas_size,
            atlas_max_lod: terrain_source_atlas_max_lod(logical_tile_size, gutter_size, physical_tile_size),
        });
    }

    bail!("Too many terrain textures for one terrain_atlas.dds page")
}

fn terrain_source_atlas_max_lod(mut logical_tile_size: u32, mut gutter_size: u32, mut physical_tile_size: u32) -> u32 {
    let mut max_lod = 0_u32;
    while gutter_size >= 8 && physical_tile_size.is_multiple_of(4) && logical_tile_size > 1 {
        logical_tile_size /= 2;
        gutter_size /= 2;
        physical_tile_size /= 2;
        max_lod += 1;
    }
    max_lod
}

pub(super) fn build_terrain_atlas_bc1_chain<'a>(
    texture_ids: &TerrainTextureIds<'a>,
    cache: &TerrainTextureCache<'a>,
    atlas_spec: TerrainAtlasSpec,
) -> Bc1MipChain {
    let mut mips = Vec::with_capacity(atlas_spec.atlas_max_lod as usize + 1);
    let mut scratch = TerrainAtlasBuildScratch::default();
    for mip in 0..=atlas_spec.atlas_max_lod {
        mips.push(build_terrain_atlas_bc1_mip(texture_ids, cache, atlas_spec, mip, &mut scratch));
    }

    Bc1MipChain {
        width: atlas_spec.atlas_size,
        height: atlas_spec.atlas_size,
        max_lod: atlas_spec.atlas_max_lod,
        mips,
    }
}

fn build_terrain_atlas_bc1_mip<'a>(
    texture_ids: &TerrainTextureIds<'a>,
    cache: &TerrainTextureCache<'a>,
    atlas_spec: TerrainAtlasSpec,
    mip: u32,
    scratch: &mut TerrainAtlasBuildScratch,
) -> Vec<u8> {
    let logical_tile_size = atlas_spec.logical_tile_size >> mip;
    let gutter_size = atlas_spec.gutter_size >> mip;
    let physical_tile_size = atlas_spec.physical_tile_size >> mip;
    let atlas_size = atlas_spec.atlas_size >> mip;
    let atlas_blocks_x = atlas_size / 4;
    let atlas_blocks_y = atlas_size / 4;
    let mut atlas_blocks = vec![0_u8; atlas_blocks_x as usize * atlas_blocks_y as usize * 8];

    for (index, path) in texture_ids.ordered_paths.iter().copied().enumerate() {
        let texture = if index == 0 {
            cache.default_texture()
        } else {
            cache.get(path)
        };
        let tile_x = u32::try_from(index).expect("terrain texture index fits in u32") % atlas_spec.tiles_per_row;
        let tile_y = u32::try_from(index).expect("terrain texture index fits in u32") / atlas_spec.tiles_per_row;
        if let Some(bc1) = texture.bc1_mip_at_size(logical_tile_size)
            && block_copy_eligible(bc1, logical_tile_size, gutter_size, physical_tile_size)
        {
            copy_wrapped_bc1_tile(
                &mut atlas_blocks,
                atlas_blocks_x,
                tile_x,
                tile_y,
                logical_tile_size,
                gutter_size,
                physical_tile_size,
                bc1,
            );
        } else {
            let physical_blocks = (physical_tile_size / 4) as usize;
            let atlas_pitch = atlas_blocks_x as usize * 8;
            let region_start = (tile_y as usize * physical_blocks) * atlas_pitch;
            let region = &mut atlas_blocks[region_start..region_start + physical_blocks * atlas_pitch];
            let col_offset = (tile_x as usize * physical_blocks) * 8;
            let tile = texture_image_for_logical_size(texture, logical_tile_size, &mut scratch.resized_tile);
            encode_wrapped_rgba_tile_into_region(
                tile,
                gutter_size,
                physical_tile_size,
                region,
                atlas_pitch,
                col_offset,
                &mut scratch.wrapped_tile,
            );
        }
    }

    atlas_blocks
}

fn texture_image_for_logical_size<'a>(
    texture: &'a TerrainTexture,
    logical_tile_size: u32,
    scratch: &'a mut RgbaScratch,
) -> RgbaSurfaceView<'a> {
    if let Some(level) = texture
        .levels
        .iter()
        .find(|level| level.width() == logical_tile_size && level.height() == logical_tile_size)
    {
        return rgba_image_view(level);
    }

    scratch.width = logical_tile_size;
    scratch.height = logical_tile_size;
    resize_rgba_to_dimensions_into(texture.base(), logical_tile_size, logical_tile_size, &mut scratch.pixels);
    scratch.view()
}

fn rgba_image_view(image: &RgbaImage) -> RgbaSurfaceView<'_> {
    RgbaSurfaceView {
        width: image.width(),
        height: image.height(),
        stride: image.width() as usize * 4,
        pixels: image.as_raw(),
    }
}

fn encode_wrapped_rgba_tile_into_region(
    tile: RgbaSurfaceView<'_>,
    gutter_size: u32,
    physical_tile_size: u32,
    region: &mut [u8],
    atlas_pitch: usize,
    col_offset: usize,
    scratch: &mut RgbaScratch,
) {
    let wrapped = wrapped_tile_image(tile, gutter_size, physical_tile_size, scratch);
    encode_bc1_rgba_surface_into_region(
        wrapped.pixels,
        wrapped.width,
        wrapped.height,
        wrapped.stride,
        region,
        atlas_pitch,
        col_offset,
    );
}

fn wrapped_tile_image<'a>(
    tile: RgbaSurfaceView<'_>,
    gutter_size: u32,
    physical_tile_size: u32,
    scratch: &'a mut RgbaScratch,
) -> RgbaSurfaceView<'a> {
    let logical_tile_size = tile.width as usize;
    let gutter_size = gutter_size as usize;
    let physical_tile_size = physical_tile_size as usize;
    assert_eq!(tile.height as usize, logical_tile_size);
    assert!(gutter_size <= logical_tile_size);
    assert_eq!(physical_tile_size, logical_tile_size + gutter_size * 2);

    scratch.resize(physical_tile_size as u32, physical_tile_size as u32);
    let src_row_bytes = logical_tile_size * 4;
    let gutter_bytes = gutter_size * 4;
    let dst_stride = scratch.stride();

    for y in 0..physical_tile_size {
        let source_y = (y + logical_tile_size - gutter_size) % logical_tile_size;
        let src_row_start = source_y * tile.stride;
        let src_row = &tile.pixels[src_row_start..src_row_start + src_row_bytes];
        let dst_row = &mut scratch.pixels[y * dst_stride..y * dst_stride + physical_tile_size * 4];

        if gutter_bytes > 0 {
            dst_row[..gutter_bytes].copy_from_slice(&src_row[src_row_bytes - gutter_bytes..src_row_bytes]);
        }
        dst_row[gutter_bytes..gutter_bytes + src_row_bytes].copy_from_slice(src_row);
        if gutter_bytes > 0 {
            let right_start = gutter_bytes + src_row_bytes;
            dst_row[right_start..right_start + gutter_bytes].copy_from_slice(&src_row[..gutter_bytes]);
        }
    }

    scratch.view()
}

pub(super) fn block_copy_eligible(bc1: &Bc1Mip, logical_tile_size: u32, gutter_size: u32, physical_tile_size: u32) -> bool {
    bc1.width == logical_tile_size
        && bc1.height == logical_tile_size
        && logical_tile_size.is_multiple_of(4)
        && gutter_size.is_multiple_of(4)
        && physical_tile_size.is_multiple_of(4)
        && bc1.blocks.len() >= (logical_tile_size / 4) as usize * (logical_tile_size / 4) as usize * 8
}

fn copy_wrapped_bc1_tile(
    atlas_blocks: &mut [u8],
    atlas_blocks_x: u32,
    tile_x: u32,
    tile_y: u32,
    logical_tile_size: u32,
    gutter_size: u32,
    physical_tile_size: u32,
    bc1: &Bc1Mip,
) {
    let source_blocks_x = logical_tile_size / 4;
    let physical_blocks = physical_tile_size / 4;
    for by in 0..physical_blocks {
        let pixel_y = by * 4;
        let logical_y = (pixel_y + logical_tile_size - gutter_size) % logical_tile_size;
        let source_block_y = logical_y / 4;
        for bx in 0..physical_blocks {
            let pixel_x = bx * 4;
            let logical_x = (pixel_x + logical_tile_size - gutter_size) % logical_tile_size;
            let source_block_x = logical_x / 4;
            let source_offset = ((source_block_y * source_blocks_x + source_block_x) * 8) as usize;
            let atlas_block_x = tile_x * physical_blocks + bx;
            let atlas_block_y = tile_y * physical_blocks + by;
            let dest_offset = ((atlas_block_y * atlas_blocks_x + atlas_block_x) * 8) as usize;
            atlas_blocks[dest_offset..dest_offset + 8].copy_from_slice(&bc1.blocks[source_offset..source_offset + 8]);
        }
    }
}
