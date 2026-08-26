use std::borrow::Cow;

use glam::Vec3;
use smallvec::smallvec;

use super::*;
use crate::StableRefKey;
use crate::info::DistantReference;

fn usage_info_with_references() -> UsageInfo<'static> {
    let mut usage: UsageInfo<'static> = UsageInfo::default();
    usage.exterior_references_mut().insert(
        StableRefKey::test(1),
        DistantReference {
            id: Cow::Borrowed("mesh_a.nif"),
            deleted: false,
            persistent: false,
            translation: Vec3::new(1.0, 2.0, 3.0),
            rotation: Vec3::new(4.0, 5.0, 6.0),
            scale: 1.5,
            vis_index: 7,
        },
    );
    usage.cells.insert(
        "Interior".to_string(),
        [(
            StableRefKey::test(2),
            DistantReference {
                id: Cow::Borrowed("mesh_b.nif"),
                deleted: false,
                persistent: false,
                translation: Vec3::new(10.0, 20.0, 30.0),
                rotation: Vec3::new(40.0, 50.0, 60.0),
                scale: 2.0,
                vis_index: 3,
            },
        )]
        .into_iter()
        .collect(),
    );
    usage
}

fn distant_statics() -> PackedDistantStatics {
    [
        ("mesh_a.nif".to_string(), Default::default()),
        ("mesh_b.nif".to_string(), Default::default()),
    ]
    .into_iter()
    .collect()
}

#[test]
fn writes_dynamic_vis_headers_and_reference_indices() {
    let usage = usage_info_with_references();
    let dynamic_vis = DynamicVisData {
        groups: vec![
            crate::DynamicVisGroup {
                index: 1,
                kind: DynamicVisKind::Journal {
                    journal_id: "journal_id".to_string(),
                    ranges: smallvec![(10, 20)],
                },
            },
            crate::DynamicVisGroup {
                index: 2,
                kind: DynamicVisKind::UniqueObject {
                    source_id: "source_object".to_string(),
                    linked_ids: vec!["source_object".to_string(), "linked".to_string()],
                },
            },
        ],
        ..Default::default()
    };

    let ordinals = StaticOrdinalView::from_packed(&distant_statics());
    let bytes = serialize_usage_data(&usage, &ordinals, &dynamic_vis, 256.0).unwrap();

    assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), 2);
    assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 2);

    let first_group = &bytes[8..138];
    assert_eq!(first_group.len(), 130);
    assert_eq!(first_group[0], 1);
    assert_eq!(&first_group[1..11], b"journal_id");
    assert_eq!(first_group[65], 1);
    assert_eq!(i32::from_le_bytes(first_group[66..70].try_into().unwrap()), 10);
    assert_eq!(i32::from_le_bytes(first_group[70..74].try_into().unwrap()), 20);
    assert!(first_group[74..130].iter().all(|&byte| byte == 0));

    let second_group = &bytes[138..268];
    assert_eq!(second_group.len(), 130);
    assert_eq!(second_group[0], 3);
    assert_eq!(&second_group[1..14], b"source_object");
    assert_eq!(second_group[65], 1);
    assert_eq!(i32::from_le_bytes(second_group[66..70].try_into().unwrap()), 1);
    assert_eq!(i32::from_le_bytes(second_group[70..74].try_into().unwrap()), 2);
    assert!(second_group[74..130].iter().all(|&byte| byte == 0));

    let exterior_count = u32::from_le_bytes(bytes[268..272].try_into().unwrap());
    assert_eq!(exterior_count, 1);
    assert_eq!(u16::from_le_bytes(bytes[276..278].try_into().unwrap()), 7);

    let interior_header = 268 + 4 + 4 + 2 + 2 + 12 + 12 + 4;
    let interior_count = u32::from_le_bytes(bytes[interior_header..interior_header + 4].try_into().unwrap());
    assert_eq!(interior_count, 1);
    let interior_ref = interior_header + 68;
    assert_eq!(
        u16::from_le_bytes(bytes[interior_ref + 4..interior_ref + 6].try_into().unwrap()),
        3
    );
}

/// Builds a `UsageInfo` whose single interior cell is named `name` and holds one reference.
fn usage_info_with_interior(name: &str) -> UsageInfo<'static> {
    let mut usage: UsageInfo<'static> = UsageInfo::default();
    usage.cells.insert(
        name.to_string(),
        [(
            StableRefKey::test(2),
            DistantReference {
                id: Cow::Borrowed("mesh_b.nif"),
                deleted: false,
                persistent: false,
                translation: Vec3::ZERO,
                rotation: Vec3::ZERO,
                scale: 1.0,
                vis_index: 0,
            },
        )]
        .into_iter()
        .collect(),
    );
    usage
}

/// Returns the 64-byte cell-name field of the first interior world space.
fn interior_name_field(bytes: &[u8]) -> &[u8] {
    // 4-byte mesh count, 4-byte group count, no groups, then the 4-byte exterior count
    // (zero references, so the placeholder stands) before the first interior header.
    let interior_header = 4 + 4 + 4;
    &bytes[interior_header + 4..interior_header + 4 + 64]
}

#[test]
fn interior_cell_name_is_written_as_windows_1252() {
    // `Балмора` decoded from WINDOWS-1251 bytes as WINDOWS-1252 mojibake, which is what the
    // ESM reader produces on a Russian install. Serializing must restore the original bytes.
    let cp1251 = b"\xc1\xe0\xeb\xec\xee\xf0\xe0";
    let name: String = cp1251.iter().map(|&byte| byte as char).collect();

    let usage = usage_info_with_interior(&name);
    let ordinals = StaticOrdinalView::from_packed(&distant_statics());
    let bytes = serialize_usage_data(&usage, &ordinals, &DynamicVisData::default(), 256.0).unwrap();

    let field = interior_name_field(&bytes);
    assert_eq!(&field[..cp1251.len()], cp1251);
    assert!(field[cp1251.len()..].iter().all(|&byte| byte == 0));
    assert_eq!(
        name.len(),
        14,
        "the UTF-8 form is twice as wide, which is what used to overflow"
    );
}

#[test]
fn interior_cell_name_may_fill_the_whole_field() {
    let name = "a".repeat(64);

    let usage = usage_info_with_interior(&name);
    let ordinals = StaticOrdinalView::from_packed(&distant_statics());
    let bytes = serialize_usage_data(&usage, &ordinals, &DynamicVisData::default(), 256.0).unwrap();

    assert_eq!(interior_name_field(&bytes), name.as_bytes());
}

#[test]
fn oversized_interior_cell_name_fails_naming_the_cell() {
    let name = "a".repeat(65);

    let usage = usage_info_with_interior(&name);
    let ordinals = StaticOrdinalView::from_packed(&distant_statics());
    let error = serialize_usage_data(&usage, &ordinals, &DynamicVisData::default(), 256.0)
        .expect_err("a 65-byte name does not fit the 64-byte field");

    let message = format!("{error:#}");
    assert!(message.contains(&name), "{message}");
    assert!(message.contains("65 bytes"), "{message}");
}

#[test]
fn dynamic_vis_id_may_fill_the_whole_field() {
    let id = "j".repeat(64);
    let dynamic_vis = DynamicVisData {
        groups: vec![crate::DynamicVisGroup {
            index: 1,
            kind: DynamicVisKind::Journal {
                journal_id: id.clone(),
                ranges: smallvec![(1, 2)],
            },
        }],
        ..Default::default()
    };

    let ordinals = StaticOrdinalView::from_packed(&distant_statics());
    let bytes = serialize_usage_data(&UsageInfo::default(), &ordinals, &dynamic_vis, 256.0).unwrap();

    assert_eq!(&bytes[9..73], id.as_bytes());
    assert_eq!(bytes[73], 1, "the range count still lands at its fixed offset");
}

#[test]
fn oversized_dynamic_vis_id_fails_naming_the_kind_and_id() {
    let id = "g".repeat(65);
    let dynamic_vis = DynamicVisData {
        groups: vec![crate::DynamicVisGroup {
            index: 1,
            kind: DynamicVisKind::Global {
                global_id: id.clone(),
                ranges: smallvec![(1, 2)],
            },
        }],
        ..Default::default()
    };

    let ordinals = StaticOrdinalView::from_packed(&distant_statics());
    let error = serialize_usage_data(&UsageInfo::default(), &ordinals, &dynamic_vis, 256.0)
        .expect_err("a 65-byte id does not fit the 64-byte field");

    let message = format!("{error:#}");
    assert!(message.contains("global"), "{message}");
    assert!(message.contains(&id), "{message}");
}

#[test]
fn dynamic_vis_id_containing_a_nul_is_rejected_rather_than_truncated() {
    // The encoder truncates at an embedded NUL, so without an explicit check this id would
    // pass the length test and serialize as "foo", silently binding the group to a shorter id.
    let dynamic_vis = DynamicVisData {
        groups: vec![crate::DynamicVisGroup {
            index: 1,
            kind: DynamicVisKind::Journal {
                journal_id: "foo\0bar".to_string(),
                ranges: smallvec![(1, 2)],
            },
        }],
        ..Default::default()
    };

    let ordinals = StaticOrdinalView::from_packed(&distant_statics());
    let error = serialize_usage_data(&UsageInfo::default(), &ordinals, &dynamic_vis, 256.0)
        .expect_err("an embedded NUL must not be silently truncated");

    let message = format!("{error:#}");
    assert!(message.contains("journal"), "{message}");
    assert!(message.contains("NUL"), "{message}");
}

#[test]
fn interior_cell_name_containing_a_nul_is_rejected_rather_than_truncated() {
    let usage = usage_info_with_interior("foo\0bar");
    let ordinals = StaticOrdinalView::from_packed(&distant_statics());
    let error = serialize_usage_data(&usage, &ordinals, &DynamicVisData::default(), 256.0)
        .expect_err("an embedded NUL must not be silently truncated");

    assert!(format!("{error:#}").contains("NUL"), "{error:#}");
}
