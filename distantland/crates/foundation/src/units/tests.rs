use super::*;

#[test]
fn canonical_stream_preserves_input_order() {
    let mut forward = CanonicalWriter::new();
    forward.write_i32(1);
    forward.write_i32(2);

    let mut reverse = CanonicalWriter::new();
    reverse.write_i32(2);
    reverse.write_i32(1);

    assert_ne!(forward.finish(), reverse.finish());
}

#[test]
fn canonical_stream_distinguishes_distinct_values() {
    let mut first = CanonicalWriter::new();
    first.write_i32(1);

    let mut second = CanonicalWriter::new();
    second.write_i32(2);

    assert_ne!(first.finish(), second.finish());
}

#[test]
fn canonical_domains_and_versions_are_separate() {
    fn digest(domain: &str, version: u32) -> [u8; 32] {
        let mut writer = CanonicalWriter::new();
        writer.write_str(domain);
        writer.write_u32(version);
        writer.write_str("same payload");
        writer.finish()
    }

    assert_ne!(digest("request", 1), digest("settings", 1));
    assert_ne!(digest("request", 1), digest("request", 2));
}

#[test]
fn primitives_use_stable_little_endian_bit_patterns_and_framing() {
    let mut writer = CanonicalWriter::new();
    writer.write_bool(true);
    writer.write_u32(0x0102_0304);
    writer.write_u64(0x0102_0304_0506_0708);
    writer.write_i32(-2);
    writer.write_f32(-0.0);
    writer.write_str("ab");

    assert_eq!(
        writer.bytes,
        [
            &[1][..],
            &0x0102_0304_u32.to_le_bytes(),
            &0x0102_0304_0506_0708_u64.to_le_bytes(),
            &(-2_i32).to_le_bytes(),
            &(-0.0_f32).to_bits().to_le_bytes(),
            &2_u64.to_le_bytes(),
            b"ab",
        ]
        .concat()
    );
}

#[test]
fn finish_and_clear_matches_finish_and_has_no_stale_prefix() {
    let mut reusable = CanonicalWriter::new();
    reusable.write_str("alpha");
    reusable.write_u16(7);
    let first = reusable.finish_and_clear();
    assert!(reusable.bytes.is_empty());

    let mut fresh = CanonicalWriter::new();
    fresh.write_str("alpha");
    fresh.write_u16(7);
    assert_eq!(first, fresh.finish());

    reusable.write_str("beta_payload");
    reusable.write_u32(0xdead_beef);
    let second = reusable.finish_and_clear();
    let mut fresh_second = CanonicalWriter::new();
    fresh_second.write_str("beta_payload");
    fresh_second.write_u32(0xdead_beef);
    assert_eq!(second, fresh_second.finish());
    assert_ne!(first, second);
}

#[test]
fn coordinate_display_is_comma_joined_without_whitespace() {
    assert_eq!(MergeUnitKey::new(-2, 7).to_string(), "-2,7");
    assert_eq!(TerrainCellUnitKey::new(0, 0).to_string(), "0,0");
    assert_eq!(TerrainChunkUnitKey::new(10, -1).to_string(), "10,-1");
}

#[test]
fn coordinate_unit_keys_are_absolute() {
    let merge = MergeUnitKey::new(-2, 7);
    assert_eq!((merge.x, merge.y), (-2, 7));

    assert_eq!(
        TerrainChunkUnitKey::from_cell(TerrainCellUnitKey::new(3, 4)),
        TerrainChunkUnitKey::new(0, 1)
    );
    assert_eq!(
        TerrainChunkUnitKey::from_cell(TerrainCellUnitKey::new(-1, -4)),
        TerrainChunkUnitKey::new(-1, -1)
    );
    assert_eq!(
        TerrainChunkUnitKey::from_cell(TerrainCellUnitKey::new(-5, -5)),
        TerrainChunkUnitKey::new(-2, -2)
    );
}

#[test]
fn misaligned_work_item_overlaps_multiple_absolute_chunks() {
    assert_eq!(
        overlapping_absolute_chunks(2, -1, 4),
        [
            TerrainChunkUnitKey::new(0, -1),
            TerrainChunkUnitKey::new(0, 0),
            TerrainChunkUnitKey::new(1, -1),
            TerrainChunkUnitKey::new(1, 0),
        ]
    );
}

#[test]
fn aligned_work_item_declares_exactly_its_own_chunk() {
    assert_eq!(overlapping_absolute_chunks(4, 8, 4), [TerrainChunkUnitKey::new(1, 2)]);
}

#[test]
fn negative_work_item_coordinates_use_euclidean_chunk_division() {
    assert_eq!(overlapping_absolute_chunks(-4, -4, 4), [TerrainChunkUnitKey::new(-1, -1)]);
    assert_eq!(
        overlapping_absolute_chunks(-2, -2, 4),
        [
            TerrainChunkUnitKey::new(-1, -1),
            TerrainChunkUnitKey::new(-1, 0),
            TerrainChunkUnitKey::new(0, -1),
            TerrainChunkUnitKey::new(0, 0),
        ]
    );
}

#[test]
fn work_item_dependencies_are_not_clipped_by_a_narrow_region() {
    assert_eq!(
        overlapping_absolute_chunks(3, 0, 4),
        [TerrainChunkUnitKey::new(0, 0), TerrainChunkUnitKey::new(1, 0)]
    );
}
