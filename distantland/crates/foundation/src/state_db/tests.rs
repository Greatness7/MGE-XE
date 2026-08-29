use std::collections::BTreeMap;

use super::*;
use crate::units::{MergeUnitKey, TerrainCellUnitKey, TerrainChunkUnitKey};
use itertools::Itertools;

fn table(entries: &[(&str, u8)]) -> FingerprintTable<String> {
    FingerprintTable::from_entries(entries.iter().map(|(key, value)| ((*key).to_owned(), [*value; 32])))
}

fn optimized_bounds(radius: f32) -> OptimizedMeshBounds {
    OptimizedMeshBounds {
        center: [1.0, 2.0, 3.0],
        radius,
        box_min: [-radius; 3],
        box_max: [radius; 3],
    }
}

fn parse_coord(display: &str) -> (i32, i32) {
    let (x, y) = display.split_once(',').expect("fixture coordinate");
    (x.parse().unwrap(), y.parse().unwrap())
}

fn merge_table(entries: &[(&str, u8)]) -> FingerprintTable<MergeUnitKey> {
    FingerprintTable::from_entries(entries.iter().map(|(key, value)| {
        let (x, y) = parse_coord(key);
        (MergeUnitKey::new(x, y), [*value; 32])
    }))
}

fn terrain_cell_table(entries: &[(&str, u8)]) -> FingerprintTable<TerrainCellUnitKey> {
    FingerprintTable::from_entries(entries.iter().map(|(key, value)| {
        let (x, y) = parse_coord(key);
        (TerrainCellUnitKey::new(x, y), [*value; 32])
    }))
}

fn terrain_chunk_table(entries: &[(&str, u8)]) -> FingerprintTable<TerrainChunkUnitKey> {
    FingerprintTable::from_entries(entries.iter().map(|(key, value)| {
        let (x, y) = parse_coord(key);
        (TerrainChunkUnitKey::new(x, y), [*value; 32])
    }))
}

#[test]
fn diff_reports_mixed_changes_and_removed_keys() {
    let current = table(&[("a", 1), ("c", 3), ("d", 4)]);
    let previous = table(&[("a", 1), ("b", 2), ("c", 9)]);
    let diff = diff_table(&current, Some(&previous));
    let report = diff.report;
    assert_eq!(
        (report.total, report.dirty, report.added, report.removed, report.changed),
        (3, 3, 1, 1, 1)
    );
    assert_eq!(report.dirty_keys, ["b", "c", "d"]);
}

#[test]
fn state_round_trip_is_deterministic_and_zero_diff() {
    let state = GenerationState {
        units: UnitFingerprintTables {
            mesh: table(&[("a", 1), ("b", 2)]),
            ..UnitFingerprintTables::default()
        },
        optimized_mesh_bounds: vec![("a.nif".to_owned(), optimized_bounds(4.0))],
        terrain_enabled: true,
        ..GenerationState::default()
    };
    let bytes = encode(&state).unwrap();
    let loaded = decode(&bytes);
    assert_eq!(loaded.status(), GenerationStateStatus::Loaded);
    assert_eq!(loaded.state(), Some(&state));
    assert_eq!(encode(loaded.state().unwrap()).unwrap(), bytes);
}

#[test]
fn optimized_bounds_table_must_be_sorted_unique_and_finite() {
    let valid = GenerationState {
        optimized_mesh_bounds: vec![
            ("a.nif".to_owned(), optimized_bounds(1.0)),
            ("b.nif".to_owned(), optimized_bounds(2.0)),
        ],
        ..GenerationState::default()
    };
    assert_eq!(decode(&encode(&valid).unwrap()).status(), GenerationStateStatus::Loaded);

    let mut invalid = valid.clone();
    invalid.optimized_mesh_bounds.reverse();
    assert!(encode(&invalid).is_err());

    invalid = valid;
    invalid.optimized_mesh_bounds[0].1.radius = f32::NAN;
    assert!(encode(&invalid).is_err());
}

fn record(start_cell_x: i32, start_cell_y: i32) -> TerrainMeshWorkKey {
    TerrainMeshWorkKey {
        start_cell_x,
        start_cell_y,
        cells_per_side: 4,
    }
}

fn terrain_state_with_records(records: Vec<TerrainMeshWorkKey>) -> GenerationState {
    GenerationState {
        units: UnitFingerprintTables {
            terrain_cell: terrain_cell_table(&[("0,0", 1)]),
            terrain_chunk: terrain_chunk_table(&[("0,0", 1)]),
            ..UnitFingerprintTables::default()
        },
        terrain_enabled: true,
        terrain_records: records,
        terrain_dds_domain_digest: [7; 32],
        ..GenerationState::default()
    }
}

#[test]
fn terrain_record_metadata_round_trips() {
    let state = terrain_state_with_records(vec![record(0, 0), record(4, 0), record(8, 0)]);
    let bytes = encode(&state).unwrap();
    let loaded = decode(&bytes);
    assert_eq!(loaded.status(), GenerationStateStatus::Loaded);
    assert_eq!(loaded.state(), Some(&state));
    assert_eq!(encode(loaded.state().unwrap()).unwrap(), bytes);
}

#[test]
fn out_of_order_record_keys_are_rejected() {
    let state = terrain_state_with_records(vec![record(4, 0), record(0, 0)]);
    assert!(encode(&state).is_err());
}

#[test]
fn duplicate_record_keys_are_rejected() {
    let state = terrain_state_with_records(vec![record(0, 0), record(0, 0)]);
    assert!(encode(&state).is_err());
}

#[test]
fn disabled_terrain_permits_no_record_metadata_or_dds_digest() {
    let mut state = GenerationState::default();
    state.terrain_records = vec![record(0, 0)];
    assert!(encode(&state).is_err());

    let mut state = GenerationState::default();
    state.terrain_dds_domain_digest = [1; 32];
    assert!(encode(&state).is_err());
}

#[test]
fn version_and_corruption_are_reported_distinctly() {
    let mut bytes = encode(&GenerationState::default()).unwrap();
    bytes[8..12].copy_from_slice(&(STATE_FORMAT_VERSION + 1).to_le_bytes());
    assert_eq!(decode(&bytes).status(), GenerationStateStatus::VersionMismatch);
    bytes = encode(&GenerationState::default()).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    assert_eq!(decode(&bytes).status(), GenerationStateStatus::Corrupt);
}

#[test]
fn superseded_v3_state_is_rejected_as_version_mismatch() {
    let mut bytes = encode(&GenerationState::default()).unwrap();
    bytes[8..12].copy_from_slice(&3u32.to_le_bytes());
    assert_eq!(decode(&bytes).status(), GenerationStateStatus::VersionMismatch);
}

/// Places one record in its assigned shard, leaving all other shards empty.
fn state_with_shard_record(key: StaticRecordKey) -> GenerationState {
    let shard = key.shard_id();
    let mut state = GenerationState::default();
    state.static_mesh_shards[shard] = StaticShardState {
        input_digest: [3; 32],
        record_count: 1,
        subset_count: 2,
        vertex_count: 40,
        triangle_count: 20,
        merged_record_count: 0,
        merged_vertex_count: 0,
        merged_triangle_count: 0,
        records: vec![key],
    };
    state
}

#[test]
fn static_shard_record_metadata_round_trips() {
    let state = state_with_shard_record(StaticRecordKey::Mesh {
        id: "mesh_round_trip".to_owned(),
    });
    let bytes = encode(&state).unwrap();
    let loaded = decode(&bytes);
    assert_eq!(loaded.status(), GenerationStateStatus::Loaded);
    assert_eq!(loaded.state(), Some(&state));
    assert_eq!(encode(loaded.state().unwrap()).unwrap(), bytes);
}

#[test]
fn static_shard_miscounted_or_misassigned_records_are_rejected() {
    let key = StaticRecordKey::Mesh {
        id: "mesh_reject".to_owned(),
    };
    let shard = key.shard_id();

    let mut miscounted = GenerationState::default();
    miscounted.static_mesh_shards[shard] = StaticShardState {
        record_count: 5,
        records: vec![key.clone()],
        ..StaticShardState::default()
    };
    assert!(encode(&miscounted).is_err());

    let mut misassigned = GenerationState::default();
    misassigned.static_mesh_shards[(shard + 1) % 32] = StaticShardState {
        record_count: 1,
        records: vec![key],
        ..StaticShardState::default()
    };
    assert!(encode(&misassigned).is_err());
}

#[test]
fn static_shard_rejects_unsorted_and_duplicate_records() {
    // Find a shard holding at least two distinct mesh keys so we can test intra-shard ordering.
    let mut buckets: BTreeMap<usize, Vec<StaticRecordKey>> = BTreeMap::new();
    let mut sorted = (0..10_000)
        .map(|i| StaticRecordKey::Mesh { id: format!("mesh_{i}") })
        .find_map(|key| {
            let bucket = buckets.entry(key.shard_id()).or_default();
            bucket.push(key);
            (bucket.len() == 2).then(|| bucket.clone())
        })
        .expect("two keys sharing a shard");
    sorted.sort();
    let shard = sorted[0].shard_id();

    let valid = StaticShardState {
        record_count: 2,
        records: sorted.clone(),
        ..StaticShardState::default()
    };
    assert!(validate_static_shard(&valid, shard));

    let unsorted = StaticShardState {
        records: vec![sorted[1].clone(), sorted[0].clone()],
        ..valid.clone()
    };
    assert!(!validate_static_shard(&unsorted, shard));

    let duplicate = StaticShardState {
        records: vec![sorted[0].clone(), sorted[0].clone()],
        ..valid
    };
    assert!(!validate_static_shard(&duplicate, shard));
}

#[test]
fn typed_mesh_and_merge_dirty_lists_are_complete_and_typed() {
    // 100 old mesh units, all removed; 100 fresh mesh units, all added. Both are far beyond the
    // 64-key report truncation. Merge: one changed fingerprint, one added cell.
    let previous = GenerationState {
        units: UnitFingerprintTables {
            mesh: FingerprintTable::from_entries((0..100).map(|i| (format!("m{i:03}"), [1; 32]))),
            merge: merge_table(&[("1,0", 1)]),
            ..UnitFingerprintTables::default()
        },
        ..GenerationState::default()
    };
    let current = GenerationState {
        units: UnitFingerprintTables {
            mesh: FingerprintTable::from_entries((0..100).map(|i| (format!("n{i:03}"), [1; 32]))),
            merge: merge_table(&[("2,-3", 1), ("1,0", 9)]),
            ..UnitFingerprintTables::default()
        },
        ..GenerationState::default()
    };
    let diff = diff(&current, &LoadedGenerationState::Loaded(previous.clone()), false).unwrap();

    // The typed lists are complete even though the manifest report is truncated to 64 keys.
    assert_eq!(diff.mesh_added.len(), 100);
    assert_eq!(diff.mesh_removed.len(), 100);
    assert!(diff.mesh_changed.is_empty());
    assert!(diff.mesh_added.binary_search_by(|key| key.as_str().cmp("n099")).is_ok());
    assert!(diff.mesh_removed.binary_search_by(|key| key.as_str().cmp("m000")).is_ok());
    assert!(diff.report.kinds.mesh.truncated);

    // Merge keys stay typed through the diff; human dirty_keys still render as "x,y".
    assert_eq!(diff.merge_added, [MergeUnitKey::new(2, -3)]);
    assert_eq!(diff.merge_changed, [MergeUnitKey::new(1, 0)]);
    assert!(diff.merge_removed.is_empty());
    assert_eq!(diff.report.kinds.merge.dirty_keys, ["1,0", "2,-3"]);
}

#[test]
fn dirty_keys_are_bounded_after_sorting() {
    let current = FingerprintTable::from_entries((0..65).rev().map(|index| (format!("{index:02}"), [1; 32])));
    let diff = diff_table(&current, None);
    let report = diff.report;
    assert_eq!(report.dirty, 65);
    assert_eq!(report.dirty_keys.len(), 64);
    assert!(report.truncated);
    assert_eq!(report.dirty_keys[0], "00");
    // The complete typed partition is never truncated.
    assert_eq!(diff.added_all.len(), 65);
}

#[test]
fn typed_coordinate_dirty_keys_are_truncated_after_numeric_order() {
    // 65 merge cells in reverse numeric order; dirty report must keep complete count while rendering
    // only the first 64 keys in typed (x,y) order after sort.
    let current = FingerprintTable::from_entries((0..65).rev().map(|i| (MergeUnitKey::new(i, -i), [1; 32])));
    let diff = diff_table(&current, None);
    let report = diff.report;
    assert_eq!(report.dirty, 65);
    assert_eq!(report.dirty_keys.len(), 64);
    assert!(report.truncated);
    assert_eq!(diff.added_all.len(), 65);
    assert_eq!(diff.added_all[0], MergeUnitKey::new(0, 0));
    assert_eq!(diff.added_all[64], MergeUnitKey::new(64, -64));
    assert_eq!(report.dirty_keys[0], "0,0");
    assert_eq!(report.dirty_keys[63], "63,-63");
    assert!(!report.dirty_keys.iter().any(|key| key == "64,-64"));
}

#[test]
fn dirty_terrain_chunks_merge_partitions_in_numeric_order() {
    let previous = GenerationState {
        units: UnitFingerprintTables {
            terrain_chunk: terrain_chunk_table(&[("-2,0", 1), ("0,0", 1), ("2,0", 1)]),
            ..UnitFingerprintTables::default()
        },
        terrain_enabled: true,
        ..GenerationState::default()
    };
    let current = GenerationState {
        units: UnitFingerprintTables {
            terrain_chunk: terrain_chunk_table(&[("-1,0", 1), ("0,0", 9), ("2,0", 1)]),
            ..UnitFingerprintTables::default()
        },
        terrain_enabled: true,
        ..GenerationState::default()
    };

    let diff = diff(&current, &LoadedGenerationState::Loaded(previous), false).unwrap();

    assert_eq!(
        diff.dirty_terrain_chunks,
        [
            TerrainChunkUnitKey::new(-2, 0),
            TerrainChunkUnitKey::new(-1, 0),
            TerrainChunkUnitKey::new(0, 0),
        ]
    );
}

#[test]
fn terrain_disabled_state_is_not_carried_forward_or_compared() {
    let previous = GenerationState {
        units: UnitFingerprintTables {
            terrain_cell: terrain_cell_table(&[("0,0", 1)]),
            terrain_chunk: terrain_chunk_table(&[("0,0", 1)]),
            ..UnitFingerprintTables::default()
        },
        terrain_enabled: true,
        ..GenerationState::default()
    };
    let current = GenerationState::default();
    let report = diff(&current, &LoadedGenerationState::Loaded(previous.clone()), false)
        .unwrap()
        .report;
    assert_eq!(report.kinds.terrain_cell.status, UnitKindStatus::Disabled);
    assert_eq!(report.kinds.terrain_chunk.status, UnitKindStatus::Disabled);
}

#[test]
fn reenabled_terrain_is_all_added_after_disabled_baseline() {
    let previous = GenerationState::default();
    let current = GenerationState {
        units: UnitFingerprintTables {
            terrain_cell: terrain_cell_table(&[("0,0", 1), ("1,0", 2)]),
            terrain_chunk: terrain_chunk_table(&[("0,0", 1)]),
            ..UnitFingerprintTables::default()
        },
        terrain_enabled: true,
        ..GenerationState::default()
    };
    let report = diff(&current, &LoadedGenerationState::Loaded(previous.clone()), false)
        .unwrap()
        .report;
    assert_eq!(report.kinds.terrain_cell.added, 2);
    assert_eq!(report.kinds.terrain_chunk.added, 1);
    assert!(report.domains.terrain.dirty);
}

#[test]
fn force_rebuild_ignores_valid_previous_state_for_diff() {
    let state = GenerationState {
        units: UnitFingerprintTables {
            mesh: table(&[("a.nif", 1)]),
            ..UnitFingerprintTables::default()
        },
        ..GenerationState::default()
    };
    let report = diff(&state, &LoadedGenerationState::Loaded(state.clone()), true)
        .unwrap()
        .report;
    assert_eq!(report.kinds.mesh.added, 1);
    assert_eq!(format_rebuild_cause(&report, true, false).as_deref(), Some("forced rebuild"));
}

#[test]
fn settings_change_is_reported_as_a_rebuild_cause() {
    let previous = GenerationState::default();
    let mut current = previous.clone();
    current.settings_identity[0] = 1;

    let report = diff(&current, &LoadedGenerationState::Loaded(previous.clone()), false)
        .unwrap()
        .report;

    assert!(report.settings_changed);
    assert_eq!(
        format_rebuild_cause(&report, false, false).as_deref(),
        Some("settings changed")
    );
}

#[test]
fn texture_only_change_is_explained_without_dirtying_the_statics_domain() {
    let previous = GenerationState::default();
    let mut current = previous.clone();
    current.units.texture = table(&[("textures\\changed.dds", 1)]);

    let report = diff(&current, &LoadedGenerationState::Loaded(previous.clone()), false)
        .unwrap()
        .report;

    assert!(!report.domains.statics.dirty);
    assert_eq!(report.kinds.texture.added, 1);
    assert_eq!(
        format_rebuild_cause(&report, false, false).as_deref(),
        Some("textures +1/-0/~0")
    );
}

#[test]
fn coordinate_keys_are_sorted_numerically() {
    let table = FingerprintTable::from_entries([
        (MergeUnitKey::new(10, -1), [1; 32]),
        (MergeUnitKey::new(2, 0), [1; 32]),
        (MergeUnitKey::new(-1, 4), [1; 32]),
    ]);
    assert_eq!(
        table.entries.iter().map(|(key, _)| *key).collect_vec(),
        [MergeUnitKey::new(-1, 4), MergeUnitKey::new(2, 0), MergeUnitKey::new(10, -1)]
    );
    assert_eq!(
        table.entries.iter().map(|(key, _)| key.to_string()).collect_vec(),
        ["-1,4", "2,0", "10,-1"]
    );
}

#[test]
fn typed_coordinate_state_round_trips_and_renders_dirty_keys() {
    let state = GenerationState {
        units: UnitFingerprintTables {
            merge: merge_table(&[("10,-1", 1), ("2,0", 2)]),
            terrain_cell: terrain_cell_table(&[("0,0", 3)]),
            terrain_chunk: terrain_chunk_table(&[("1,0", 4)]),
            ..UnitFingerprintTables::default()
        },
        reverse: ReverseIndexes {
            mesh_to_merge: vec![("mesh.nif".to_owned(), vec![MergeUnitKey::new(2, 0)])],
            texture_to_mesh: vec![("tex.dds".to_owned(), vec!["mesh.nif".to_owned()])],
            texture_to_terrain_cell: vec![("land.dds".to_owned(), vec![TerrainCellUnitKey::new(0, 0)])],
        },
        terrain_enabled: true,
        ..GenerationState::default()
    };
    let bytes = encode(&state).unwrap();
    let loaded = decode(&bytes);
    assert_eq!(loaded.status(), GenerationStateStatus::Loaded);
    assert_eq!(loaded.state(), Some(&state));

    let empty = GenerationState {
        terrain_enabled: true,
        ..GenerationState::default()
    };
    let report = diff(&state, &LoadedGenerationState::Loaded(empty), false).unwrap().report;
    assert_eq!(report.kinds.merge.dirty_keys, ["2,0", "10,-1"]);
    assert_eq!(report.kinds.terrain_cell.dirty_keys, ["0,0"]);
    assert_eq!(report.kinds.terrain_chunk.dirty_keys, ["1,0"]);
}

#[test]
fn prior_state_format_version_is_rejected_as_version_mismatch() {
    // Frame layout: magic (8) + version (u32 LE). Rewrite version 5 → 4 without re-encoding body.
    let mut bytes = encode(&GenerationState::default()).unwrap();
    bytes[8..12].copy_from_slice(&4u32.to_le_bytes());
    assert_eq!(decode(&bytes).status(), GenerationStateStatus::VersionMismatch);
}
