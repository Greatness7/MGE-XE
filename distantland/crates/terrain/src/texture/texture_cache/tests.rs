use super::*;

use image_dds::ddsfile::{D3DFormat, Dds, NewD3dParams};

fn make_test_vfs(dir: &std::path::Path) -> Vfs {
    use crate::vfs::directory_map::build_directory_map;

    let maps = build_directory_map(&[dir.to_path_buf()]).unwrap();
    Vfs {
        ini_path: dir.join("Morrowind.ini"),
        data_dirs: vec![dir.to_path_buf()],
        active_plugins: vec![],
        archives: vec![],
        maps,
    }
}

fn make_dds(format: D3DFormat, width: u32, height: u32, data: &[u8]) -> Dds {
    let mut dds = Dds::new_d3d(NewD3dParams {
        height,
        width,
        depth: None,
        format,
        mipmap_levels: None,
        caps2: None,
    })
    .unwrap();
    assert!(dds.data.len() >= data.len());
    dds.data[..data.len()].copy_from_slice(data);
    dds
}

fn safe_bc1_color_block(seed: u8) -> [u8; 8] {
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

fn unsafe_bc1_color_block() -> [u8; 8] {
    [0xe0, 0x07, 0x00, 0xf8, 0, 1, 2, 3]
}

fn extract_blocks(dds: &Dds) -> Option<Vec<u8>> {
    Bc1CompatibleSourceExtractor::new(dds).and_then(|extractor| extractor.extract(0).map(|mip| mip.blocks))
}

#[test]
fn bc1_source_extractor_keeps_blocks_exact() {
    let mut blocks = Vec::new();
    blocks.extend_from_slice(&safe_bc1_color_block(1));
    blocks.extend_from_slice(&unsafe_bc1_color_block());
    let dds = make_dds(D3DFormat::DXT1, 8, 4, &blocks);

    assert_eq!(extract_blocks(&dds).unwrap(), blocks);
}

#[test]
fn bc2_source_extractor_copies_color_half() {
    let alpha_a = [0x10; 8];
    let alpha_b = [0x20; 8];
    let color_a = safe_bc1_color_block(3);
    let color_b = safe_bc1_color_block(17);
    let mut data = Vec::new();
    data.extend_from_slice(&alpha_a);
    data.extend_from_slice(&color_a);
    data.extend_from_slice(&alpha_b);
    data.extend_from_slice(&color_b);
    let dds = make_dds(D3DFormat::DXT3, 8, 4, &data);

    let mut expected = Vec::new();
    expected.extend_from_slice(&color_a);
    expected.extend_from_slice(&color_b);
    assert_eq!(extract_blocks(&dds).unwrap(), expected);
}

#[test]
fn bc3_source_extractor_ignores_alpha_bytes() {
    let color = safe_bc1_color_block(9);
    let mut data_a = Vec::new();
    data_a.extend_from_slice(&[0x33; 8]);
    data_a.extend_from_slice(&color);
    let mut data_b = Vec::new();
    data_b.extend_from_slice(&[0xcc; 8]);
    data_b.extend_from_slice(&color);

    let blocks_a = extract_blocks(&make_dds(D3DFormat::DXT5, 4, 4, &data_a)).unwrap();
    let blocks_b = extract_blocks(&make_dds(D3DFormat::DXT5, 4, 4, &data_b)).unwrap();

    assert_eq!(blocks_a, color);
    assert_eq!(blocks_a, blocks_b);
}

#[test]
fn bc2_bc3_extractor_rejects_unsafe_endpoint_order() {
    let mut data = Vec::new();
    data.extend_from_slice(&[0xff; 8]);
    data.extend_from_slice(&unsafe_bc1_color_block());
    let dxt3 = make_dds(D3DFormat::DXT3, 4, 4, &data);
    let dxt5 = make_dds(D3DFormat::DXT5, 4, 4, &data);

    assert!(extract_blocks(&dxt3).is_none());
    assert!(extract_blocks(&dxt5).is_none());
}

#[test]
fn bc2_bc3_extractor_rejects_unaligned_mips() {
    let mut data = Vec::new();
    data.extend_from_slice(&[0; 8]);
    data.extend_from_slice(&safe_bc1_color_block(5));
    let dds = make_dds(D3DFormat::DXT3, 6, 4, &data);
    let extractor = Bc1CompatibleSourceExtractor::new(&dds).unwrap();

    assert!(extractor.extract(0).is_none());
}

#[test]
fn load_inputs_inserts_default_fallback_into_images() {
    let dir = tempfile::tempdir().unwrap();
    let vfs = make_test_vfs(dir.path());
    let default_key = "missing_default.dds";
    let mut ordered_paths: IndexSet<&str> = Default::default();
    ordered_paths.insert(default_key);
    let inputs = TerrainTextureInputs {
        default_key,
        ordered_paths,
    };

    let cache = TerrainTextureCache::load_inputs(&vfs, &inputs);

    assert_eq!(cache.len(), 1);
    assert!(cache.images.contains_key(default_key));
    assert_eq!(
        *cache.default_texture().base().get_pixel(0, 0),
        image::Rgba([69, 51, 33, 255])
    );
    assert!(std::ptr::eq(cache.get("missing.dds"), cache.default_texture()));
}

#[test]
fn load_inputs_defers_decode_until_decode_is_called() {
    let dir = tempfile::tempdir().unwrap();
    let textures_dir = dir.path().join("textures");
    std::fs::create_dir_all(&textures_dir).unwrap();
    let texture_path = textures_dir.join("terrain_texture.dds");
    let image = RgbaImage::from_pixel(4, 4, image::Rgba([200, 100, 50, 255]));
    save_rgba8_dds_unflipped(image, &texture_path, false).unwrap();
    let expected_hash = *blake3::hash(&std::fs::read(&texture_path).unwrap()).as_bytes();

    let vfs = make_test_vfs(dir.path());
    let default_key = "terrain_texture.dds";
    let mut ordered_paths: IndexSet<&str> = Default::default();
    ordered_paths.insert(default_key);
    let inputs = TerrainTextureInputs {
        default_key,
        ordered_paths,
    };

    let mut cache = TerrainTextureCache::load_inputs(&vfs, &inputs);
    let loaded = &cache.images[default_key];
    assert_eq!(loaded.bytes_hash, expected_hash);
    assert!(loaded.levels.is_empty(), "load_inputs must not decode pixel data");

    cache.decode(256);
    let decoded = &cache.images[default_key];
    assert_eq!(decoded.bytes_hash, expected_hash, "decode must not change the identity hash");
    assert!(!decoded.levels.is_empty(), "decode must populate the mip chain");
    assert_eq!(*decoded.base().get_pixel(0, 0), image::Rgba([200, 100, 50, 255]));
}

#[test]
fn decode_drops_failures_without_reordering_survivors() {
    let dir = tempfile::tempdir().unwrap();
    let textures_dir = dir.path().join("textures");
    std::fs::create_dir_all(&textures_dir).unwrap();
    save_rgba8_dds_unflipped(
        RgbaImage::from_pixel(4, 4, image::Rgba([200, 100, 50, 255])),
        &textures_dir.join("default.dds"),
        false,
    )
    .unwrap();
    std::fs::write(textures_dir.join("broken.dds"), b"not a dds").unwrap();
    save_rgba8_dds_unflipped(
        RgbaImage::from_pixel(4, 4, image::Rgba([50, 100, 200, 255])),
        &textures_dir.join("last.dds"),
        false,
    )
    .unwrap();

    let vfs = make_test_vfs(dir.path());
    let default_key = "default.dds";
    let mut ordered_paths: IndexSet<&str> = Default::default();
    ordered_paths.extend([default_key, "broken.dds", "last.dds"]);
    let inputs = TerrainTextureInputs {
        default_key,
        ordered_paths,
    };

    let mut cache = TerrainTextureCache::load_inputs(&vfs, &inputs);
    cache.decode(256);

    assert_eq!(cache.images.keys().copied().collect::<Vec<_>>(), [default_key, "last.dds"]);
    assert!(std::ptr::eq(cache.get("broken.dds"), cache.default_texture()));
}
