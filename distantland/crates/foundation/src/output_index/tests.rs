use super::*;
use crate::output::static_mesh_shard_relative_path;
use crate::state_db::{self, GenerationState};
use crate::storage::state::{ArtifactKind, CommittedState, artifact_from_written, encode};

fn write_minimal_tree(root: &Path, terrain: bool) {
    let paths = OutputPaths::new(root);
    fs::create_dir_all(&paths.atlas_texture_dir).unwrap();
    fs::write(&paths.version_path, [MGE_DL_VERSION]).unwrap();
    let usage_bytes = 0_u32.to_le_bytes();
    fs::write(&paths.usage_data_path, usage_bytes).unwrap();
    let static_bytes = crate::mge_xe::serialize_static_meshes(&crate::mge_xe::PackedDistantStatics::default()).unwrap();
    for path in &paths.static_mesh_shard_paths {
        fs::write(path, &static_bytes).unwrap();
    }
    // Touch the lock file so shared acquisition has a target.
    fs::write(&paths.writer_lock_path, []).unwrap();

    let mut generation = GenerationState::default();
    generation.terrain_enabled = terrain;
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
            usage_bytes.len() as u64,
            *blake3::hash(&usage_bytes).as_bytes(),
        ),
    ];
    artifacts.extend(paths.static_mesh_shard_paths.iter().enumerate().map(|(shard_id, _)| {
        artifact_from_written(
            ArtifactKind::StaticShard,
            static_mesh_shard_relative_path(shard_id),
            static_bytes.len() as u64,
            *blake3::hash(&static_bytes).as_bytes(),
        )
    }));
    if terrain {
        // A real (empty) terrain file rather than a magic-only stub: state validation binds the
        // persisted record-key count to the header's `mesh_count`, so it reads the full header.
        let terrain_bytes =
            crate::mge_xe::distant_terrain::serialize_terrain_file(&crate::mge_xe::distant_terrain::TerrainFile::default())
                .unwrap();
        fs::write(&paths.terrain_path, &terrain_bytes).unwrap();
        let mut occlusion = b"XEOCCL02".to_vec();
        occlusion.extend_from_slice(&2_u32.to_le_bytes());
        fs::write(&paths.terrain_occlusion_path, &occlusion).unwrap();
        let mut dds = b"DDS ".to_vec();
        dds.resize(128, 0);
        for (path, name) in [
            (&paths.terrain_atlas_path, "terrain_atlas.dds"),
            (&paths.terrain_blend_patterns_path, "terrain_blend_patterns.dds"),
            (&paths.terrain_material_path, "terrain_material.dds"),
            (&paths.terrain_material_flags_path, "terrain_material_flags.dds"),
            (&paths.terrain_patch_albedo_path, "terrain_patch_albedo.dds"),
        ] {
            fs::write(path, &dds).unwrap();
            artifacts.push(artifact_from_written(
                ArtifactKind::TerrainDds,
                name,
                dds.len() as u64,
                *blake3::hash(&dds).as_bytes(),
            ));
        }
        artifacts.push(artifact_from_written(
            ArtifactKind::TerrainPayload,
            "terrain.bin",
            terrain_bytes.len() as u64,
            *blake3::hash(&terrain_bytes).as_bytes(),
        ));
        artifacts.push(artifact_from_written(
            ArtifactKind::TerrainOcclusion,
            "terrain_occlusion.bin",
            occlusion.len() as u64,
            *blake3::hash(&occlusion).as_bytes(),
        ));
    }
    let state = CommittedState::new(state_db::encode(&generation).unwrap(), artifacts).unwrap();
    fs::write(&paths.generation_state_path, encode(&state).unwrap()).unwrap();
}

#[test]
fn valid_trees_resolve_under_a_shared_pin() {
    let temp = tempfile::tempdir().unwrap();
    write_minimal_tree(temp.path(), true);
    let snapshot = open_output_snapshot(temp.path(), Duration::ZERO, OutputValidation::Full).unwrap();
    assert_eq!(snapshot.version(), MGE_DL_VERSION);
    assert!(snapshot.terrain_available());
    assert!(snapshot.paths().static_mesh_shard_paths.iter().all(|path| path.is_file()));
    assert!(snapshot.paths().terrain_path.is_file());
}

#[test]
fn terrain_disabled_tree_is_valid_without_terrain_files() {
    let temp = tempfile::tempdir().unwrap();
    write_minimal_tree(temp.path(), false);
    let snapshot = open_output_snapshot(temp.path(), Duration::ZERO, OutputValidation::Routine).unwrap();
    assert!(!snapshot.terrain_available());
    assert!(!snapshot.paths().terrain_path.exists());
}

#[test]
fn older_versions_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let paths = OutputPaths::new(temp.path());
    fs::create_dir_all(&paths.distantland_dir).unwrap();
    fs::write(&paths.version_path, [MGE_DL_VERSION - 1]).unwrap();
    let error = open_output_snapshot(temp.path(), Duration::ZERO, OutputValidation::Routine).unwrap_err();
    assert_eq!(error.code(), "incompatible_older_version");
}

#[test]
fn full_validation_detects_content_hash_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    write_minimal_tree(temp.path(), false);
    let paths = OutputPaths::new(temp.path());
    // Preserve the bounded header/count fields while changing body bytes covered only by Full hashing.
    let mut bytes = fs::read(&paths.static_mesh_shard_paths[0]).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    fs::write(&paths.static_mesh_shard_paths[0], bytes).unwrap();
    open_output_snapshot(temp.path(), Duration::ZERO, OutputValidation::Routine).unwrap();
    let error = open_output_snapshot(temp.path(), Duration::ZERO, OutputValidation::Full).unwrap_err();
    assert_eq!(error.code(), "artifact_hash_mismatch");
}
