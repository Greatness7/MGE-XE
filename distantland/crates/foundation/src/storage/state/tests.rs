use super::*;
use crate::state_db::GenerationState;

fn write_empty_static_bundle(root: &Path) {
    let statics_dir = root.join("statics");
    fs::create_dir_all(&statics_dir).unwrap();
    fs::write(root.join("version"), [MGE_DL_VERSION]).unwrap();
    fs::write(statics_dir.join("usage.data"), 0_u32.to_le_bytes()).unwrap();
    let empty_shard = crate::mge_xe::serialize_static_meshes(&crate::mge_xe::PackedDistantStatics::default()).unwrap();
    assert_eq!(empty_shard.len(), crate::mge_xe::distant_statics::HEADER_SIZE);
    for shard_id in 0..STATIC_MESH_SHARD_COUNT {
        fs::write(
            statics_dir.join(crate::output::static_mesh_shard_file_name(shard_id)),
            &empty_shard,
        )
        .unwrap();
    }
}

fn sample_state(terrain_enabled: bool) -> CommittedState {
    let mut generation = GenerationState::default();
    generation.terrain_enabled = terrain_enabled;
    let state_bytes = state_db::encode(&generation).unwrap();
    let mut artifacts = vec![
        artifact_from_written(
            ArtifactKind::Version,
            "version",
            1,
            *blake3::hash(&[MGE_DL_VERSION]).as_bytes(),
        ),
        artifact_from_written(
            ArtifactKind::Usage,
            "statics\\usage.data",
            4,
            *blake3::hash(b"use!").as_bytes(),
        ),
    ];
    artifacts.extend((0..STATIC_MESH_SHARD_COUNT).map(|shard_id| {
        artifact_from_written(
            ArtifactKind::StaticShard,
            static_mesh_shard_relative_path(shard_id),
            crate::mge_xe::distant_statics::HEADER_SIZE as u64,
            [0; 32],
        )
    }));
    if terrain_enabled {
        artifacts.extend([
            artifact_from_written(
                ArtifactKind::TerrainPayload,
                "terrain.bin",
                12,
                *blake3::hash(b"XELAND02\x02\0\0\0").as_bytes(),
            ),
            artifact_from_written(
                ArtifactKind::TerrainDds,
                "terrain_atlas.dds",
                128,
                *blake3::hash(&[0; 128]).as_bytes(),
            ),
            artifact_from_written(
                ArtifactKind::TerrainDds,
                "terrain_blend_patterns.dds",
                128,
                *blake3::hash(&[0; 128]).as_bytes(),
            ),
            artifact_from_written(
                ArtifactKind::TerrainDds,
                "terrain_material.dds",
                128,
                *blake3::hash(&[0; 128]).as_bytes(),
            ),
            artifact_from_written(
                ArtifactKind::TerrainDds,
                "terrain_material_flags.dds",
                128,
                *blake3::hash(&[0; 128]).as_bytes(),
            ),
            artifact_from_written(
                ArtifactKind::TerrainDds,
                "terrain_patch_albedo.dds",
                128,
                *blake3::hash(&[0; 128]).as_bytes(),
            ),
            artifact_from_written(
                ArtifactKind::TerrainOcclusion,
                "terrain_occlusion.bin",
                12,
                *blake3::hash(b"XEOCCL02\x02\0\0\0").as_bytes(),
            ),
        ]);
    }
    CommittedState::new(state_bytes, artifacts).unwrap()
}

#[test]
fn inventory_requires_all_static_shards() {
    let mut state = sample_state(false);
    state
        .artifacts
        .retain(|entry| entry.relative_path != static_mesh_shard_relative_path(STATIC_MESH_SHARD_COUNT - 1));

    let error = state.validate_invariants().unwrap_err().to_string();
    assert!(error.contains("every static shard entry"), "unexpected error: {error}");
}

#[test]
fn routine_validation_rejects_a_shard_header_count_that_disagrees_with_state_bytes() {
    let dir = tempfile::tempdir().unwrap();
    write_empty_static_bundle(dir.path());
    let state = sample_state(false);
    validate_artifacts(dir.path(), &state, ArtifactValidation::Routine).unwrap();

    let shard_path = dir.path().join("statics").join(crate::output::static_mesh_shard_file_name(0));
    let mut shard = fs::read(&shard_path).unwrap();
    shard[32..36].copy_from_slice(&1_u32.to_le_bytes());
    fs::write(&shard_path, shard).unwrap();

    let error = validate_artifacts(dir.path(), &state, ArtifactValidation::Routine)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("declares 1 records but generation state records 0"),
        "unexpected error: {error}"
    );
}

fn record_key(start_cell_x: i32) -> crate::units::TerrainMeshWorkKey {
    crate::units::TerrainMeshWorkKey {
        start_cell_x,
        start_cell_y: 0,
        cells_per_side: 4,
    }
}

/// Builds a committed state whose only real on-disk artifact is an empty `terrain.bin`.
///
/// The rest of the inventory exists only to satisfy the inventory invariant and is never
/// opened: these tests call `validate_terrain_record_metadata` directly so that the
/// record-count binding is the only thing under test.
fn terrain_metadata_state(dir: &Path, records: Vec<crate::units::TerrainMeshWorkKey>) -> CommittedState {
    let generation = GenerationState {
        terrain_enabled: true,
        terrain_records: records,
        ..GenerationState::default()
    };
    let bytes =
        crate::mge_xe::distant_terrain::serialize_terrain_file(&crate::mge_xe::distant_terrain::TerrainFile::default())
            .unwrap();
    std::fs::write(dir.join("terrain.bin"), &bytes).unwrap();
    let mut artifacts = sample_state(true).artifacts;
    let payload = artifacts
        .iter_mut()
        .find(|entry| entry.kind == ArtifactKind::TerrainPayload)
        .expect("terrain-enabled sample lists a payload");
    payload.byte_length = bytes.len() as u64;
    payload.content_blake3 = *blake3::hash(&bytes).as_bytes();
    CommittedState::new(state_db::encode(&generation).unwrap(), artifacts).unwrap()
}

#[test]
fn record_count_matching_the_terrain_header_validates() {
    let temp = tempfile::tempdir().unwrap();
    let state = terrain_metadata_state(temp.path(), Vec::new());
    assert!(validate_terrain_record_metadata(temp.path(), &state).is_ok());
}

#[test]
fn record_count_disagreeing_with_the_terrain_header_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    // The committed file holds zero meshes; the state claims one record.
    let state = terrain_metadata_state(temp.path(), vec![record_key(0)]);
    let error = validate_terrain_record_metadata(temp.path(), &state).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("declares 0 meshes but generation state records 1 keys"),
        "{error}"
    );
}

#[test]
fn state_round_trip_is_deterministic_and_ordered() {
    let state = sample_state(true);
    let bytes = encode(&state).unwrap();
    let decoded = decode(&bytes).unwrap();
    assert_eq!(decoded, state);
    assert!(
        decoded
            .artifacts
            .windows(2)
            .all(|pair| pair[0].relative_path.as_bytes() <= pair[1].relative_path.as_bytes())
    );
}

#[test]
fn zeroed_magic_is_invalid() {
    let mut bytes = encode(&sample_state(false)).unwrap();
    bytes[..8].fill(0);
    assert!(matches!(decode(&bytes), Err(StateError::InvalidMagic)));
}

#[test]
fn terrain_disabled_rejects_terrain_artifacts() {
    let mut state = sample_state(false);
    state
        .artifacts
        .push(artifact_from_written(ArtifactKind::TerrainPayload, "terrain.bin", 1, [0; 32]));
    assert!(state.validate_invariants().is_err());
}

#[test]
fn path_traversal_is_rejected() {
    let mut state = sample_state(false);
    state.artifacts[0].relative_path = "..\\version".into();
    assert!(state.validate_invariants().is_err());
}

#[test]
fn state_decode_is_memoized_and_shared_across_clones() {
    let state = sample_state(true);

    // Repeated access on one state reuses a single decoded object instead of decoding again.
    let first = state.loaded_state();
    let second = state.loaded_state();
    assert!(
        std::ptr::eq(first, second),
        "repeated loaded_state calls must return the same cached object"
    );

    // Cloning an already-populated state shares the Arc-backed decode rather than re-deriving it.
    let clone = state.clone();
    assert!(
        std::ptr::eq(state.loaded_state(), clone.loaded_state()),
        "a clone of a populated committed state must share the cached decode"
    );
}

#[test]
fn cache_occupancy_changes_neither_equality_nor_encoded_bytes() {
    // `populated` decodes and caches during construction; `unpopulated` is an otherwise identical
    // twin whose runtime cache is deliberately left empty.
    let populated = sample_state(true);
    let unpopulated = CommittedState {
        state_bytes: populated.state_bytes.clone(),
        artifacts: populated.artifacts.clone(),
        state_cache: std::sync::OnceLock::new(),
    };

    // Runtime equality is decided solely by the persisted fields, never by cache occupancy.
    assert_eq!(populated, unpopulated);

    // Serialize directly with rkyv rather than via `encode`, which would run validate_invariants
    // and populate the empty cache before serialization. Comparing the raw archives with one full
    // and one empty cache proves the `Skip` adapter keeps the bytes byte-identical regardless.
    let populated_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&populated).unwrap();
    let unpopulated_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&unpopulated).unwrap();
    assert_eq!(populated_bytes.as_slice(), unpopulated_bytes.as_slice());
}
