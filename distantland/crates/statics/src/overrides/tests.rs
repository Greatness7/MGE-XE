use std::sync::{Arc, Mutex};

use super::parse::{parse_ranges, parse_static_keywords, strip_comment, unescape};
use super::{OverridesBuilder, WaterNames};
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

/// Collects the messages of the warnings that are logged while it is the subscriber.
struct WarningLog(Arc<Mutex<Vec<String>>>);

impl tracing::Subscriber for WarningLog {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.is_event() && *metadata.level() == tracing::Level::WARN
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        struct Message<'a>(&'a mut Vec<String>);
        impl tracing::field::Visit for Message<'_> {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0.push(format!("{value:?}"));
                }
            }
        }
        event.record(&mut Message(&mut self.0.lock().unwrap()));
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

#[test]
fn later_water_names_replace_the_table_and_warn_when_they_differ() {
    let names = |surface: &str| WaterNames {
        surface: vec![surface.to_owned()],
        body: vec!["waterbody".to_owned()],
        mask: Vec::new(),
        ..WaterNames::default()
    };
    let warnings = Arc::new(Mutex::new(Vec::new()));

    let overrides = tracing::subscriber::with_default(WarningLog(warnings.clone()), || {
        let mut builder = OverridesBuilder::new();
        builder.begin_source("default.toml");
        builder.set_water_names(names("watervolume"));
        // The same names from another source are not a conflict.
        builder.begin_source("Same-metadata.toml");
        builder.set_water_names(names("watervolume"));
        assert!(warnings.lock().unwrap().is_empty());

        builder.begin_source("Other-metadata.toml");
        builder.set_water_names(names("pond"));
        builder.finish()
    });

    // The later table replaces the earlier one whole.
    assert_eq!(overrides.water_names, names("pond"));
    assert_eq!(
        *warnings.lock().unwrap(),
        ["Water names from Other-metadata.toml replace conflicting names from Same-metadata.toml"]
    );
}
