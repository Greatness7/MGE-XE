use super::*;

fn encode_bc1(image: &RgbaImage) -> Vec<u8> {
    let mut blocks = vec![0; crate::dds::bcn_level_bytes(image.width(), image.height(), 8)];
    crate::dds::encode_bc1_into(image, &mut Vec::new(), &mut blocks);
    blocks
}

// Helpers for building synthetic TerrainCells
fn make_cell<'a>(grid: (i32, i32), tex_path: &'a str) -> TerrainCell<'a> {
    let mut table: IndexMap<_, _> = Default::default();
    table.insert(0u16, Box::leak(tex_path.to_lowercase().into_boxed_str()) as &'static str);
    let texture_table = Arc::new(table);

    let mut tex_indices = Box::new([[0u16; 16]; 16]);
    // raw index 1 -> ltex key 0 -> tex_path
    for row in tex_indices.iter_mut() {
        for v in row.iter_mut() {
            *v = 1;
        }
    }

    TerrainCell {
        grid,
        heights: Box::new([[0.0f32; 65]; 65]),
        normals: vec![Vec3::Z; 65 * 65],
        colors: vec![Vec4::new(1.0, 1.0, 1.0, 0.0); 65 * 65],
        texture_indices: tex_indices,
        texture_table,
    }
}

fn make_default_cell(grid: (i32, i32)) -> TerrainCell<'static> {
    TerrainCell {
        grid,
        heights: Box::new([[0.0f32; 65]; 65]),
        normals: vec![Vec3::Z; 65 * 65],
        colors: vec![Vec4::new(1.0, 1.0, 1.0, 0.0); 65 * 65],
        texture_indices: Box::new([[0u16; 16]; 16]), // all raw 0 -> default
        texture_table: Arc::new(Default::default()),
    }
}

#[test]
fn vertex_to_patch_origin() {
    // vertex (2,2) should map to patch (0,0)
    assert_eq!(vertex_to_patch(2, 2), (0, 0));
}

#[test]
fn vertex_to_patch_formula() {
    // floor((0-2)/4) = floor(-0.5) = -1, ceil((0-2)/4) = ceil(-0.5) = 0
    let (px, py) = vertex_to_patch(0, 0);
    assert_eq!(px, -1);
    assert_eq!(py, 0);
}

#[test]
fn mod_cell_wraps_negative() {
    let mut cell = 5i32;
    let mut patch = -1i32;
    mod_cell(&mut cell, &mut patch);
    assert_eq!(cell, 4);
    assert_eq!(patch, 15);
}

#[test]
fn mod_cell_wraps_positive() {
    let mut cell = 5i32;
    let mut patch = 16i32;
    mod_cell(&mut cell, &mut patch);
    assert_eq!(cell, 6);
    assert_eq!(patch, 0);
}

#[test]
fn raw_vtex_zero_returns_default() {
    let cell = make_default_cell((0, 0));
    assert_eq!(resolve_raw_vtex(&cell, 0, DEFAULT_LAND_TEXTURE), DEFAULT_LAND_TEXTURE);
}

#[test]
fn raw_vtex_one_looks_up_ltex_key_zero() {
    let cell = make_cell((0, 0), "grass.tga");
    assert_eq!(resolve_raw_vtex(&cell, 1, "grass.tga"), "grass.tga");
}

#[test]
fn missing_neighboring_cell_returns_default() {
    let terrain_cells: TerrainCells<'static> = Default::default();
    let result = sample_vertex_texture(&terrain_cells, 0, 0, 0, 0, DEFAULT_LAND_TEXTURE);
    assert_eq!(result.path, DEFAULT_LAND_TEXTURE);
}

#[test]
fn barycentric_at_vertices() {
    let a = Vec2::new(0.0, 0.0);
    let b = Vec2::new(1.0, 0.0);
    let c = Vec2::new(0.0, 1.0);

    let bary_a = barycentric_weights(a, a, b, c);
    assert!((bary_a.x - 1.0).abs() < 1e-5, "at vertex a, bary.x should be 1");

    let bary_b = barycentric_weights(b, a, b, c);
    assert!((bary_b.y - 1.0).abs() < 1e-5, "at vertex b, bary.y should be 1");
}

fn assert_legacy_dds_header(bytes: &[u8]) {
    // Magic
    assert_eq!(&bytes[0..4], b"DDS ", "DDS magic missing");
    // dwSize = 124
    assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 124);
    // No DX10 extended header: the pixel format fourcc must NOT be "DX10"
    // DDPIXELFORMAT starts at offset 4+4+4+4+4+4+4+44 = 76, dwFourCC is at 76+4+4 = 84
    let fourcc = &bytes[84..88];
    assert_ne!(fourcc, b"DX10", "DX10 extended header found, which D3DX9 cannot read");
}

#[test]
fn bc1_dds_header_mip_count_agrees_with_written_payload() {
    // The header's mip count and the payload that follows it are derived from the same walk of
    // the chain. If they ever disagree the DDS is malformed, so pin them together across the
    // shapes that exercise the odd cases: non-square, non-power-of-two, and sub-block.
    for (width, height) in [(4, 4), (8, 8), (4, 8), (144, 108), (2, 2), (1, 1), (64, 1)] {
        for include_mips in [false, true] {
            let img = RgbaImage::from_pixel(width, height, image::Rgba([9, 8, 7, 255]));
            let mut bytes = Vec::new();
            write_bc1_dds_unflipped(img, &mut bytes, include_mips).unwrap();

            assert_legacy_dds_header(&bytes);
            assert_eq!(&bytes[84..88], b"DXT1");

            let max_lod = include_mips.then_some(u32::MAX);
            assert_eq!(
                bytes.len() as u64,
                128 + bc1_payload_size(width, height, max_lod),
                "payload size mismatch for {width}x{height} include_mips={include_mips}"
            );

            // dwMipMapCount sits at offset 4 + 24 = 28; the header omits it for a single level.
            let header_mip_count = u32::from_le_bytes(bytes[28..32].try_into().unwrap());
            let expected = bc1_mip_count(width, height, max_lod);
            assert_eq!(
                header_mip_count.max(1),
                expected,
                "header mip count mismatch for {width}x{height} include_mips={include_mips}"
            );

            // And the file must still parse as a full, well-formed chain.
            let dds = image_dds::ddsfile::Dds::read(&mut std::io::Cursor::new(&bytes)).unwrap();
            image_dds::image_from_dds(&dds, expected - 1).unwrap();
        }
    }
}

#[test]
fn save_bc1_chain_unflipped_preserves_image_orientation() {
    let mut img = RgbaImage::new(4, 8);
    for y in 0..4 {
        for x in 0..4 {
            img.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
        }
    }
    for y in 4..8 {
        for x in 0..4 {
            img.put_pixel(x, y, image::Rgba([0, 0, 255, 255]));
        }
    }

    let chain = crate::package::Bc1MipChain {
        width: 4,
        height: 8,
        max_lod: 0,
        mips: vec![encode_bc1(&img)],
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("terrain_atlas.dds");
    save_bc1_dds_from_chain_unflipped(&chain, &path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_legacy_dds_header(&bytes);
    assert_eq!(&bytes[84..88], b"DXT1");

    let mut cursor = std::io::Cursor::new(&bytes);
    let dds = image_dds::ddsfile::Dds::read(&mut cursor).unwrap();
    let rgba = image_dds::image_from_dds(&dds, 0).unwrap();
    assert!(rgba.get_pixel(0, 0)[0] > 200);
    assert!(rgba.get_pixel(0, 7)[2] > 200);
}

#[test]
fn control_rgba8_dds_uses_a8b8g8r8_header_and_no_mips() {
    let img = RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 4]));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("terrain_material.dds");
    save_rgba8_dds_unflipped(img, &path, false).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_legacy_dds_header(&bytes);
    assert_eq!(bytes.len(), rgba8_dds_capacity_hint(2, 2, false));
    assert_eq!(u32::from_le_bytes(bytes[88..92].try_into().unwrap()), 32);
    assert_eq!(u32::from_le_bytes(bytes[92..96].try_into().unwrap()), 0x0000_00ff);
    assert_eq!(u32::from_le_bytes(bytes[96..100].try_into().unwrap()), 0x0000_ff00);
    assert_eq!(u32::from_le_bytes(bytes[100..104].try_into().unwrap()), 0x00ff_0000);
    assert_eq!(u32::from_le_bytes(bytes[104..108].try_into().unwrap()), 0xff00_0000);
    assert_eq!(u32::from_le_bytes(bytes[28..32].try_into().unwrap()), 0);
}

#[test]
fn control_rgba8_dds_preserves_rectangular_dimensions() {
    let img = RgbaImage::from_pixel(144, 108, image::Rgba([1, 2, 3, 4]));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("terrain_blend_patterns.dds");
    save_rgba8_dds_unflipped(img, &path, false).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_legacy_dds_header(&bytes);
    assert_eq!(u32::from_le_bytes(bytes[12..16].try_into().unwrap()), 108);
    assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 144);
    assert_eq!(bytes.len(), rgba8_dds_capacity_hint(144, 108, false));
}

#[test]
fn control_rgba8_dds_round_trips_raw_rgba_payload() {
    let mut img = RgbaImage::new(2, 2);
    img.put_pixel(0, 0, image::Rgba([1, 2, 3, 4]));
    img.put_pixel(1, 0, image::Rgba([5, 6, 7, 8]));
    img.put_pixel(0, 1, image::Rgba([9, 10, 11, 12]));
    img.put_pixel(1, 1, image::Rgba([13, 14, 15, 16]));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("terrain_material.dds");
    save_rgba8_dds_unflipped(img.clone(), &path, false).unwrap();
    let bytes = std::fs::read(&path).unwrap();

    assert_eq!(&bytes[128..], img.as_raw());

    let mut cursor = std::io::Cursor::new(&bytes);
    let dds = image_dds::ddsfile::Dds::read(&mut cursor).unwrap();
    let round_tripped = image_dds::image_from_dds(&dds, 0).unwrap();
    assert_eq!(round_tripped.as_raw(), img.as_raw());
}

#[test]
fn terrain_texture_generates_mip_chain_to_one_pixel() {
    let tex = TerrainTexture::from_base(RgbaImage::from_pixel(4, 2, image::Rgba([10, 20, 30, 255])));
    assert_eq!(tex.levels.len(), 3);
    assert_eq!((tex.levels[0].width(), tex.levels[0].height()), (4, 2));
    assert_eq!((tex.levels[1].width(), tex.levels[1].height()), (2, 1));
    assert_eq!((tex.levels[2].width(), tex.levels[2].height()), (1, 1));
}

#[test]
fn mip_downsample_averages_source_pixels_in_linear_space() {
    let mut img = RgbaImage::new(2, 2);
    img.put_pixel(0, 0, image::Rgba([0, 0, 0, 255]));
    img.put_pixel(1, 0, image::Rgba([64, 0, 0, 255]));
    img.put_pixel(0, 1, image::Rgba([128, 0, 0, 255]));
    img.put_pixel(1, 1, image::Rgba([192, 0, 0, 255]));

    let tex = TerrainTexture::from_base(img);
    assert_eq!(*tex.levels[1].get_pixel(0, 0), image::Rgba([123, 0, 0, 255]));
}
