use super::*;

fn encode(format: image::ImageFormat, width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image::RgbaImage::new(width, height))
        .write_to(&mut std::io::Cursor::new(&mut bytes), format)
        .unwrap();
    bytes
}

#[test]
fn probe_dds_reads_embedded_error_texture_dimensions() {
    let (w, h, mips, is_dds) = probe_dimensions(crate::vfs::STATIC_ERROR_TEXTURE_KEY, crate::vfs::STATIC_ERROR_TEXTURE_DDS);
    assert_eq!((w, h, is_dds), (4, 4, true));
    assert!(mips >= 1);
}

#[test]
fn probe_non_dds_reads_bmp_dimensions_without_decode() {
    let bmp = encode(image::ImageFormat::Bmp, 8, 4);
    assert_eq!(probe_dimensions("x.bmp", &bmp), (8, 4, 1, false));
}

#[test]
fn probe_non_dds_reads_tga_dimensions_without_decode() {
    // TGA has no magic bytes, so it is only probeable via its extension.
    let tga = encode(image::ImageFormat::Tga, 8, 4);
    assert_eq!(probe_dimensions("x.tga", &tga), (8, 4, 1, false));
}

#[test]
fn probe_rejects_bytes_that_contradict_the_extension() {
    let bmp = encode(image::ImageFormat::Bmp, 8, 4);
    assert_eq!(probe_dimensions("x.tga", &bmp), (0, 0, 1, false));
}

#[test]
fn probe_rejects_unreadable_extension() {
    let bmp = encode(image::ImageFormat::Bmp, 8, 4);
    assert_eq!(probe_dimensions("x.png", &bmp), (0, 0, 1, false));
}

#[test]
fn probe_rejects_garbage_bytes() {
    assert_eq!(probe_dimensions("x.tga", b"not an image"), (0, 0, 1, false));
}

#[test]
fn read_header_reuses_capacity_and_replaces_contents() {
    let dir = tempfile::tempdir().unwrap();
    let large = dir.path().join("large.bin");
    let small = dir.path().join("small.bin");
    std::fs::write(&large, vec![0xabu8; HEADER_PROBE_LEN + 1]).unwrap();
    std::fs::write(&small, b"short").unwrap();

    let mut scratch = Vec::new();
    let header = read_header(&large, HEADER_PROBE_LEN, &mut scratch).unwrap();
    assert_eq!(header.len(), HEADER_PROBE_LEN);
    assert!(header.iter().all(|&byte| byte == 0xab));
    let capacity = scratch.capacity();

    let header = read_header(&small, HEADER_PROBE_LEN, &mut scratch).unwrap();
    assert_eq!(header, b"short");
    assert_eq!(scratch.capacity(), capacity);
}

#[test]
fn lowest_density_use_wins_and_blocks_reduction_independently() {
    let mut agg = TextureUsageAggregate::default();
    // A dense use and a sparse use of the same texture: the sparse (smaller) one limits.
    agg.record_valid(10.0, "b.nif", 0, 0);
    agg.record_valid(2.0, "a.nif", 1, 3);
    assert_eq!(agg.area_density_min, 2.0);
    let limiting = agg.limiting_use.as_ref().unwrap();
    assert_eq!(
        (limiting.static_key.as_str(), limiting.subset_index, limiting.triangle_index),
        ("a.nif", 1, 3)
    );
    assert_eq!(agg.valid_count, 2);
}

#[test]
fn limiting_use_ties_break_on_stable_id_regardless_of_merge_order() {
    // Two uses with identical density tie-break on (static_key, subset, triangle).
    let mut forward = TextureUsageAggregate::default();
    forward.record_valid(1.0, "z.nif", 0, 0);
    forward.record_valid(1.0, "a.nif", 9, 9);

    let mut reverse = TextureUsageAggregate::default();
    reverse.record_valid(1.0, "a.nif", 9, 9);
    reverse.record_valid(1.0, "z.nif", 0, 0);

    for agg in [&forward, &reverse] {
        let limiting = agg.limiting_use.as_ref().unwrap();
        assert_eq!(limiting.static_key, "a.nif");
    }
}

#[test]
fn merge_is_commutative_for_density_and_flags() {
    let mut left = TextureUsageAggregate::default();
    left.record_valid(4.0, "m.nif", 0, 0);

    let mut right = TextureUsageAggregate::default();
    right.record_valid(1.0, "a.nif", 0, 0);
    right.needs_baseline = true;
    right.uncertain_count = 2;

    let mut a = left.clone();
    a.merge_from(right.clone());
    let mut b = right;
    b.merge_from(left);

    assert_eq!(a.area_density_min, b.area_density_min);
    assert_eq!(a.area_density_min, 1.0);
    assert!(a.needs_baseline && b.needs_baseline);
    assert_eq!(a.valid_count, b.valid_count);
    assert_eq!(a.uncertain_count, b.uncertain_count);
}
