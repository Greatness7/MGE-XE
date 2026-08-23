use super::OverridesBuilder;
use super::parse::{parse_ranges, parse_static_keywords, strip_comment, unescape};
use crate::mge_xe::distant_statics::StaticType;

#[test]
fn test_parse_static_keywords() {
    let ov = parse_static_keywords(b"far no_script");
    assert!(matches!(ov.static_type, StaticType::StaticFar));
    assert!(ov.no_script);
    assert!(!ov.ignore);
}

#[test]
fn override_file_identity_matches_parsed_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("test.ovr");
    let bytes = b"far no_script = meshes\\fixture.nif\n";
    std::fs::write(&path, bytes).unwrap();

    let mut builder = OverridesBuilder::new();
    let identity = builder.add_override_file_with_identity(&path).unwrap();

    assert_eq!(identity.path, path);
    assert_eq!(
        identity.content,
        distantland_foundation::identity::ContentIdentity::from_bytes(bytes)
    );
}

#[test]
fn test_parse_static_keywords_grass_density() {
    let ov = parse_static_keywords(b"grass_50");
    assert!(matches!(ov.static_type, StaticType::StaticGrass));
    assert!((ov.density - 0.5).abs() < f32::EPSILON);
}

#[test]
fn test_parse_static_keywords_reduction() {
    let ov = parse_static_keywords(b"very_far reduction_50");
    assert!(matches!(ov.static_type, StaticType::StaticVeryFar));
    assert_eq!(ov.simplify, Some(0.5));
}

#[test]
fn test_parse_static_keywords_building() {
    let ov = parse_static_keywords(b"building far");
    assert!(matches!(ov.static_type, StaticType::StaticFar));
}

#[test]
fn test_parse_static_keywords_building_last_token_wins() {
    let ov = parse_static_keywords(b"far building");
    assert!(matches!(ov.static_type, StaticType::StaticBuilding));
}

#[test]
fn test_strip_comment_simple() {
    assert_eq!(strip_comment(b"foo=bar : comment", false), b"foo=bar ");
    assert_eq!(strip_comment(b": full comment", false), b"");
    assert_eq!(strip_comment(b"no comment here", false), b"no comment here");
}

#[test]
fn test_strip_comment_escaped() {
    assert_eq!(strip_comment(b"foo\\:bar : comment", true), b"foo\\:bar ");
    assert_eq!(strip_comment(b"foo\\:bar\\:baz", true), b"foo\\:bar\\:baz");
}

#[test]
fn test_unescape() {
    assert_eq!(unescape(b"foo\\:bar"), "foo:bar");
    assert_eq!(unescape(b"foo\\\\bar"), "foo\\bar");
    assert_eq!(unescape(b"plain"), "plain");
}

#[test]
fn test_parse_ranges() {
    let tokens: Vec<&[u8]> = vec![b"0-20", b"25", b"50-100"];
    let ranges = parse_ranges(&tokens);
    assert_eq!(ranges.as_slice(), &[(0, 21), (25, 26), (50, 101)]);
}

#[test]
fn test_parse_ranges_max_8() {
    let tokens: Vec<&[u8]> = vec![b"1", b"2", b"3", b"4", b"5", b"6", b"7", b"8", b"9", b"10"];
    let ranges = parse_ranges(&tokens);
    assert_eq!(ranges.len(), 8);
}
