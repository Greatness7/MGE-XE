//! Shared test builders plus package-level guard tests.
//!
//! Submodules mirror the production layout: [`atlas`], [`material`].

use std::sync::Arc;

use super::*;
use crate::IndexMap;

mod atlas;
mod material;

fn encode_bc1(image: &RgbaImage) -> Vec<u8> {
    let mut blocks = vec![0; crate::dds::bcn_level_bytes(image.width(), image.height(), 8)];
    crate::dds::encode_bc1_into(image, &mut Vec::new(), &mut blocks);
    blocks
}

fn make_cell<'a>(grid: (i32, i32), texture: &'a str) -> crate::texture::TerrainCell<'a> {
    let mut texture_table: IndexMap<u16, &'a str> = Default::default();
    texture_table.insert(0, texture);
    crate::texture::TerrainCell {
        grid,
        heights: Box::new([[0.0; 65]; 65]),
        normals: Default::default(),
        colors: Default::default(),
        texture_indices: Box::new([[1; 16]; 16]),
        texture_table: Arc::new(texture_table),
    }
}

fn make_texture_cache<'a>(default_key: &'a str, colors: &[(&'a str, [u8; 4])]) -> TerrainTextureCache<'a> {
    let mut images: IndexMap<&'a str, TerrainTexture> = Default::default();
    for (path, rgba) in colors {
        images.insert(*path, TerrainTexture::from_base(RgbaImage::from_pixel(1, 1, Rgba(*rgba))));
    }
    images
        .entry(default_key)
        .or_insert_with(|| TerrainTexture::from_base(RgbaImage::from_pixel(1, 1, Rgba([69, 51, 33, 255]))));
    TerrainTextureCache { images, default_key }
}

fn make_texture_cache_from_images<'a>(default_key: &'a str, images: Vec<(&'a str, RgbaImage)>) -> TerrainTextureCache<'a> {
    let mut cache_images: IndexMap<&'a str, TerrainTexture> = Default::default();
    for (path, image) in images {
        cache_images.insert(path, TerrainTexture::from_base(image));
    }
    cache_images
        .entry(default_key)
        .or_insert_with(|| TerrainTexture::from_base(RgbaImage::from_pixel(1, 1, Rgba([69, 51, 33, 255]))));
    TerrainTextureCache {
        images: cache_images,
        default_key,
    }
}

#[test]
fn control_texture_guard_reports_dimension_and_memory_failures() {
    let error = validate_control_texture_region(
        TerrainControlRegion {
            origin_cell: [0, 0],
            cell_size_xy: [1024, 1024],
            material_size_xy: [16384, 16384],
            populated_cell_count: 1,
        },
        TerrainControlTextureLimits {
            max_size: 8192,
            max_bytes: 1,
        },
        8,
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("dimension limit exceeded"));
    assert!(error.contains("memory limit exceeded"));
    assert!(error.contains("fill_ratio="));
}

#[test]
fn control_texture_guard_reports_dimension_only_failures() {
    let error = validate_control_texture_region(
        TerrainControlRegion {
            origin_cell: [0, 0],
            cell_size_xy: [128, 32],
            material_size_xy: [2048, 512],
            populated_cell_count: 4_096,
        },
        TerrainControlTextureLimits {
            max_size: 1024,
            max_bytes: u64::MAX,
        },
        8,
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("dimension limit exceeded"));
    assert!(!error.contains("memory limit exceeded"));
}

#[test]
fn control_texture_guard_low_fill_ratio_recommends_sparse_region_or_cell_clip() {
    let error = validate_control_texture_region(
        TerrainControlRegion {
            origin_cell: [0, 0],
            cell_size_xy: [256, 256],
            material_size_xy: [4096, 4096],
            populated_cell_count: 1,
        },
        TerrainControlTextureLimits {
            max_size: 8192,
            max_bytes: 1,
        },
        8,
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("user-provided cell clip"));
    assert!(error.contains("fill_ratio=0.000"));
}
