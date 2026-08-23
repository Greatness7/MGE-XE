use super::*;

use image::Rgba;

fn deterministic_rgba(width: u32, height: u32) -> RgbaImage {
    RgbaImage::from_fn(width, height, |x, y| {
        Rgba([
            ((x * 31 + y * 17) & 0xff) as u8,
            ((x * 11 + y * 43) & 0xff) as u8,
            ((x * 7 + y * 5 + 19) & 0xff) as u8,
            ((x * 53 + y * 29 + 3) & 0xff) as u8,
        ])
    })
}

#[test]
fn max_dimension_prefers_exact_matching_mip_for_rectangular_texture() {
    let plan = plan_dds_mip_load(1024, 512, 10, 512);
    assert_eq!(plan.start_level, 1);
    assert_eq!(plan.source_width, 512);
    assert_eq!(plan.source_height, 256);
    assert_eq!(plan.target_width, 512);
    assert_eq!(plan.target_height, 256);
    assert!(!plan.needs_resize());
}

#[test]
fn max_dimension_uses_next_larger_mip_without_undershooting() {
    let plan = plan_dds_mip_load(1024, 512, 10, 300);
    assert_eq!(plan.start_level, 1);
    assert_eq!(plan.source_width, 512);
    assert_eq!(plan.source_height, 256);
    assert_eq!(plan.target_width, 300);
    assert_eq!(plan.target_height, 150);
    assert!(plan.needs_resize());
}

#[test]
fn max_dimension_prefers_exact_matching_mip_for_large_square_texture() {
    let plan = plan_dds_mip_load(8192, 8192, 14, 512);
    assert_eq!(plan.start_level, 4);
    assert_eq!(plan.source_width, 512);
    assert_eq!(plan.source_height, 512);
    assert_eq!(plan.target_width, 512);
    assert_eq!(plan.target_height, 512);
    assert!(!plan.needs_resize());
}

#[test]
fn max_dimension_falls_back_to_resize_when_needed() {
    let plan = plan_dds_mip_load(768, 512, 10, 256);
    assert_eq!(plan.start_level, 1);
    assert_eq!(plan.source_width, 384);
    assert_eq!(plan.source_height, 256);
    assert_eq!(plan.target_width, 256);
    assert_eq!(plan.target_height, 171);
    assert!(plan.needs_resize());
}

#[test]
fn single_mip_dds_falls_back_to_resize() {
    let plan = plan_dds_mip_load(1024, 1024, 1, 512);
    assert_eq!(plan.start_level, 0);
    assert_eq!(plan.source_width, 1024);
    assert_eq!(plan.source_height, 1024);
    assert_eq!(plan.target_width, 512);
    assert_eq!(plan.target_height, 512);
    assert!(plan.needs_resize());
}

#[test]
fn max_dimension_does_not_upscale_smaller_images() {
    let image = deterministic_rgba(256, 128);
    let original = image.clone();

    let resized = resize_rgba_to_max_dimension(image, 512);

    assert_eq!(resized.dimensions(), (256, 128));
    assert_eq!(resized, original);
}

#[test]
fn resize_to_dimensions_returns_matching_image_unchanged() {
    let image = deterministic_rgba(4, 3);
    let original = image.clone();

    let resized = resize_rgba_to_dimensions(image, 4, 3);

    assert_eq!(resized.dimensions(), (4, 3));
    assert_eq!(resized, original);
}

#[test]
fn resize_to_dimensions_preserves_alpha_channel_variation() {
    let image = deterministic_rgba(8, 8);

    let resized = resize_rgba_to_dimensions(image, 4, 4);

    assert_eq!(resized.dimensions(), (4, 4));
    let mut alpha_values = resized.pixels().map(|pixel| pixel.0[3]);
    let first_alpha = alpha_values.next().expect("resized image should contain pixels");
    assert!(alpha_values.any(|alpha| alpha != first_alpha));
}

fn encode(image: &RgbaImage, format: image::ImageFormat) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image.clone())
        .write_to(&mut Cursor::new(&mut bytes), format)
        .unwrap();
    bytes
}

#[test]
fn tga_decodes_by_extension_and_round_trips_pixels() {
    // TGA carries no magic bytes, so no amount of content sniffing can identify it and the
    // extension is the only usable signal. Asserting pixels, not just success, so that a
    // placeholder substituted for a failed decode cannot pass this.
    let source = deterministic_rgba(8, 6);
    let bytes = encode(&source, image::ImageFormat::Tga);
    assert_eq!(decode_texture_rgba("textures\\x.tga", &bytes, 4096).unwrap(), source);
}

#[test]
fn bmp_decodes_by_extension_and_round_trips_pixels() {
    let source = deterministic_rgba(8, 6);
    let bytes = encode(&source, image::ImageFormat::Bmp);
    assert_eq!(decode_texture_rgba("textures\\x.bmp", &bytes, 4096).unwrap(), source);
}

#[test]
fn decode_rejects_bytes_that_contradict_the_extension() {
    // The engine fails a mismatched file rather than retrying another decoder: only the TGA
    // reader claims `.tga`, and its header check rejects non-TGA content. Match that.
    let bytes = encode(&deterministic_rgba(8, 6), image::ImageFormat::Bmp);
    assert!(decode_texture_rgba("x.tga", &bytes, 4096).is_err());
}

#[test]
fn decode_rejects_extensions_the_engine_does_not_read() {
    let bytes = encode(&deterministic_rgba(8, 6), image::ImageFormat::Bmp);
    assert!(decode_texture_rgba("x.png", &bytes, 4096).is_err());
    assert!(decode_texture_rgba("x", &bytes, 4096).is_err());
}

#[test]
fn texture_format_selection_ignores_extension_case() {
    assert_eq!(texture_format_from_key("X.DDS"), Some(TextureFormat::Dds));
    assert_eq!(
        texture_format_from_key("x.TgA"),
        Some(TextureFormat::Image(image::ImageFormat::Tga))
    );
    assert_eq!(
        texture_format_from_key("x.bmp"),
        Some(TextureFormat::Image(image::ImageFormat::Bmp))
    );
    // The engine's TGA reader also accepts `.targa`, but the VFS never admits that extension.
    assert_eq!(texture_format_from_key("x.targa"), None);
}
