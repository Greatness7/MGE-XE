use bytes_io::{Reader, Writer};

use super::*;

/// Writes the `usage.data` layout field by field, the way `crates/usage/src/write.rs` does.
///
/// The tests build payloads this way rather than through a `Save` impl for the whole struct:
/// production has no such impl, so a round trip against one would only prove the test agrees
/// with itself.
fn write_payload(
    num_meshes: u32,
    vis_groups: &[DynamicVisGroup],
    exterior: &[UsageDataReference],
    interiors: &[Interior],
    min_static_size: f32,
) -> Vec<u8> {
    let mut writer = Writer::new(vec![]);
    writer.save(&num_meshes).unwrap();
    writer.save(&vis_groups.to_vec()).unwrap();
    writer.save(&exterior.to_vec()).unwrap();
    for interior in interiors {
        writer.save_as::<u32>(interior.references.len()).unwrap();
        writer.save_bytes_padded::<64>(interior.name.as_ref()).unwrap();
        writer.save_seq(&interior.references).unwrap();
    }
    writer.save(&0u32).unwrap(); // zero reference count terminates the interior list
    writer.save(&min_static_size).unwrap();
    writer.cursor.into_inner()
}

#[test]
fn usage_data_parses_the_written_layout() {
    let vis_groups = vec![DynamicVisGroup {
        data_source: DataSource::Journal,
        id: "journal_id".into(),
        enabled_ranges: vec![[10, 20]],
    }];
    let exterior = vec![UsageDataReference {
        id: 1,
        vis_index: 7,
        _padding: 0,
        location: Vec3::new(1.0, 2.0, 3.0),
        rotation: Vec3::new(4.0, 5.0, 6.0),
        scale: 1.5,
    }];
    let interiors = vec![Interior {
        name: "Interior".into(),
        references: vec![UsageDataReference {
            id: 0,
            vis_index: 0,
            _padding: 0,
            location: Vec3::new(10.0, 20.0, 30.0),
            rotation: Vec3::new(40.0, 50.0, 60.0),
            scale: 2.0,
        }],
    }];

    let bytes = write_payload(2, &vis_groups, &exterior, &interiors, 256.0);
    let parsed = deserialize_usage_data(&bytes).unwrap();

    assert_eq!(
        parsed,
        UsageData {
            num_meshes: 2,
            vis_groups,
            exterior,
            interiors,
            min_static_size: 256.0,
        }
    );
}

#[test]
fn usage_data_rejects_trailing_bytes() {
    let mut bytes = write_payload(0, &[], &[], &[], 0.0);
    bytes.push(0);

    let error = deserialize_usage_data(&bytes).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("trailing bytes"));
}

#[test]
fn dynamic_visibility_group_rejects_more_than_eight_ranges() {
    let mut bytes = vec![DataSource::Journal as u8];
    bytes.extend_from_slice(&[0; 64]);
    bytes.push(9);
    let mut reader = Reader::new(&bytes);

    let error = reader.load::<DynamicVisGroup>().unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}
