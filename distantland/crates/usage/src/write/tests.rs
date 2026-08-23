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
