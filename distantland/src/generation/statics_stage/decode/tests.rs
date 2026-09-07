use super::*;
use crate::generation::cache::{fingerprint_static_mesh_shard_inputs, static_mesh_shard_id};
use crate::generation::metrics::summarize_distant_statics;
use crate::generation::output::static_mesh_shard_relative_path;
use crate::generation::record_key::StaticRecordKey;
use crate::generation::storage::state::ArtifactKind;
use crate::mge_xe::distant_statics::{PackedDistantStatic, PackedSubset, PackedVertex, StaticType, UvBoundRecord};
use tempfile::tempdir;

/// Two distinct fixture keys that hash into the same shard, so one file exercises both the
/// ordinary and grass paths through a single decode.
fn same_shard_keys() -> (usize, String, String) {
    for left in 0..128 {
        let left_key = format!("ordinary_{left}.nif");
        for right in 0..128 {
            let right_key = format!("grass_{right}.nif");
            let shard = static_mesh_shard_id(&left_key);
            if static_mesh_shard_id(&right_key) == shard && left_key != right_key {
                return (shard, left_key, right_key);
            }
        }
    }
    panic!("failed to find two fixture keys assigned to one shard");
}

fn decoded_fixture() -> (usize, crate::PackedDistantStatics) {
    let (shard, ordinary_key, grass_key) = same_shard_keys();
    // The ordinary record selects palette entry 1 so the ordinal, and not just the palette
    // table, has to survive the round trip.
    let mut ordinary_vertices = vec![PackedVertex::default(); 3];
    for vertex in &mut ordinary_vertices {
        vertex.position[3] = half::f16::ONE;
    }
    let ordinary = PackedDistantStatic {
        subsets: vec![PackedSubset {
            vertices: ordinary_vertices,
            triangles: vec![[0, 1, 2]],
            palette: vec![
                UvBoundRecord {
                    bound: [0.0, 1.0, 0.0, 1.0],
                },
                UvBoundRecord {
                    bound: [0.25, 0.75, 0.125, 0.5],
                },
            ],
            texture: "ordinary.dds".into(),
            ..PackedSubset::default()
        }],
        ..PackedDistantStatic::default()
    };
    // Grass carries no palette and keeps position.w at 1.0.
    let mut grass_vertices = vec![PackedVertex::default(); 3];
    for vertex in &mut grass_vertices {
        vertex.position[3] = half::f16::ONE;
    }
    let grass = PackedDistantStatic {
        static_type: StaticType::StaticGrass,
        subsets: vec![PackedSubset {
            vertices: grass_vertices,
            triangles: vec![[0, 1, 2]],
            texture: "grass.dds".into(),
            ..PackedSubset::default()
        }],
        ..PackedDistantStatic::default()
    };
    let mut packed: crate::PackedDistantStatics = [(ordinary_key, ordinary), (grass_key, grass)].into_iter().collect();
    packed.sort_unstable_by(|left, _, right, _| left.as_bytes().cmp(right.as_bytes()));
    (shard, packed)
}

/// Writes `packed` as the committed shard `shard_id` and returns the matching paths, artifacts,
/// and previous state. These are the exact inputs [`decode_dirty_shards`] expects for a clean readback.
fn decode_inputs(
    shard_id: usize,
    packed: &crate::PackedDistantStatics,
) -> (
    tempfile::TempDir,
    [PathBuf; STATIC_MESH_SHARD_COUNT],
    [Option<RequiredArtifact>; STATIC_MESH_SHARD_COUNT],
    [StaticShardState; STATIC_MESH_SHARD_COUNT],
) {
    let directory = tempdir().unwrap();
    let paths = std::array::from_fn(|id| directory.path().join(format!("static_meshes_{id:02}")));
    let bytes = crate::statics::write::serialize_static_meshes(packed).unwrap();
    std::fs::write(&paths[shard_id], &bytes).unwrap();
    let mut artifacts = std::array::from_fn(|_| None);
    artifacts[shard_id] = Some(RequiredArtifact {
        kind: ArtifactKind::StaticShard,
        relative_path: static_mesh_shard_relative_path(shard_id),
        byte_length: bytes.len() as u64,
        content_blake3: *blake3::hash(&bytes).as_bytes(),
    });
    let mut states = std::array::from_fn(|_| StaticShardState::default());
    let counts = summarize_distant_statics(packed);
    states[shard_id] = StaticShardState {
        input_digest: fingerprint_static_mesh_shard_inputs(shard_id, packed),
        record_count: packed.len() as u32,
        subset_count: counts.0 as u64,
        vertex_count: counts.1 as u64,
        triangle_count: counts.2 as u64,
        merged_record_count: 0,
        merged_vertex_count: 0,
        merged_triangle_count: 0,
        records: packed.keys().map(|key| StaticRecordKey::parse(key)).collect(),
    };
    (directory, paths, artifacts, states)
}

#[test]
fn decoded_ordinary_and_grass_records_refingerprint_identically() {
    let (shard_id, packed) = decoded_fixture();
    let (_directory, paths, artifacts, states) = decode_inputs(shard_id, &packed);
    let artifact_refs = std::array::from_fn(|id| artifacts[id].as_ref());
    let decoded = decode_dirty_shards(&paths, &artifact_refs, &BTreeSet::from([shard_id]), &states).unwrap();
    let restored = decoded.shards[shard_id].as_ref().unwrap();
    assert_eq!(
        fingerprint_static_mesh_shard_inputs(shard_id, restored),
        fingerprint_static_mesh_shard_inputs(shard_id, &packed)
    );
    let grass = restored
        .values()
        .find(|record| record.static_type == StaticType::StaticGrass)
        .unwrap();
    assert!(grass.subsets[0].palette.is_empty());

    let ordinary = restored
        .values()
        .find(|record| record.static_type != StaticType::StaticGrass)
        .unwrap();
    assert_eq!(ordinary.subsets[0].palette.len(), 2);
    assert!(
        ordinary.subsets[0]
            .vertices
            .iter()
            .all(|vertex| vertex.position[3] == half::f16::ONE)
    );
}

#[test]
fn dirty_shard_decode_rejects_positional_count_mismatch() {
    let (shard_id, packed) = decoded_fixture();
    let (_directory, paths, artifacts, mut states) = decode_inputs(shard_id, &packed);
    states[shard_id].records.pop();
    let artifact_refs = std::array::from_fn(|id| artifacts[id].as_ref());
    let result = decode_dirty_shards(&paths, &artifact_refs, &BTreeSet::from([shard_id]), &states);
    assert_eq!(result.unwrap_err(), StaticsFallbackReason::ShardCountMismatch);
}

#[test]
fn dirty_shard_decode_checks_full_hash_even_when_length_is_unchanged() {
    let (shard_id, packed) = decoded_fixture();
    let (_directory, paths, artifacts, states) = decode_inputs(shard_id, &packed);
    let mut bytes = std::fs::read(&paths[shard_id]).unwrap();
    *bytes.last_mut().unwrap() ^= 0x80;
    std::fs::write(&paths[shard_id], bytes).unwrap();
    let artifact_refs = std::array::from_fn(|id| artifacts[id].as_ref());
    let result = decode_dirty_shards(&paths, &artifact_refs, &BTreeSet::from([shard_id]), &states);
    assert_eq!(result.unwrap_err(), StaticsFallbackReason::ShardHashMismatch);
}

#[test]
fn two_valid_dirty_shards_populate_both_slots_and_sum_bytes() {
    let (first_shard_id, packed) = decoded_fixture();
    let second_shard_id = (first_shard_id + 1) % STATIC_MESH_SHARD_COUNT;
    let (_directory, paths, mut artifacts, mut states) = decode_inputs(first_shard_id, &packed);
    let bytes = std::fs::read(&paths[first_shard_id]).unwrap();
    std::fs::write(&paths[second_shard_id], &bytes).unwrap();
    artifacts[second_shard_id] = Some(RequiredArtifact {
        kind: ArtifactKind::StaticShard,
        relative_path: static_mesh_shard_relative_path(second_shard_id),
        byte_length: bytes.len() as u64,
        content_blake3: *blake3::hash(&bytes).as_bytes(),
    });
    states[second_shard_id] = states[first_shard_id].clone();
    let artifact_refs = std::array::from_fn(|id| artifacts[id].as_ref());

    let decoded = decode_dirty_shards(
        &paths,
        &artifact_refs,
        &BTreeSet::from([first_shard_id, second_shard_id]),
        &states,
    )
    .unwrap();

    assert!(decoded.shards[first_shard_id].is_some());
    assert!(decoded.shards[second_shard_id].is_some());
    assert_eq!(decoded.bytes_read, (bytes.len() as u64).saturating_mul(2));
}

#[test]
fn multiple_decode_failures_return_the_lowest_shard_id_error() {
    let directory = tempdir().unwrap();
    let paths = std::array::from_fn(|id| directory.path().join(format!("static_meshes_{id:02}")));
    let lower_shard_id = 1;
    let higher_shard_id = 2;
    let higher_bytes = [0x80];
    std::fs::write(&paths[higher_shard_id], higher_bytes).unwrap();
    let mut artifacts: [Option<RequiredArtifact>; STATIC_MESH_SHARD_COUNT] = std::array::from_fn(|_| None);
    artifacts[higher_shard_id] = Some(RequiredArtifact {
        kind: ArtifactKind::StaticShard,
        relative_path: static_mesh_shard_relative_path(higher_shard_id),
        byte_length: higher_bytes.len() as u64,
        content_blake3: [0; 32],
    });
    let artifact_refs = std::array::from_fn(|id| artifacts[id].as_ref());
    let states = std::array::from_fn(|_| StaticShardState::default());

    let result = decode_dirty_shards(
        &paths,
        &artifact_refs,
        &BTreeSet::from([lower_shard_id, higher_shard_id]),
        &states,
    );

    assert_eq!(result.unwrap_err(), StaticsFallbackReason::BaseArtifactMissing);
}

#[test]
fn clean_shard_paths_are_not_opened() {
    let (dirty_shard_id, packed) = decoded_fixture();
    let clean_shard_id = (dirty_shard_id + 1) % STATIC_MESH_SHARD_COUNT;
    let (_directory, paths, mut artifacts, states) = decode_inputs(dirty_shard_id, &packed);
    assert!(!paths[clean_shard_id].exists());
    artifacts[clean_shard_id] = Some(RequiredArtifact {
        kind: ArtifactKind::StaticShard,
        relative_path: static_mesh_shard_relative_path(clean_shard_id),
        byte_length: 0,
        content_blake3: [0; 32],
    });
    let artifact_refs = std::array::from_fn(|id| artifacts[id].as_ref());

    let decoded = decode_dirty_shards(&paths, &artifact_refs, &BTreeSet::from([dirty_shard_id]), &states).unwrap();

    assert!(decoded.shards[dirty_shard_id].is_some());
    assert!(decoded.shards[clean_shard_id].is_none());
}
