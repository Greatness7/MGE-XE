use std::collections::BTreeSet;

use hashbrown::HashSet;

use super::*;
use crate::AtlasTextureSet;
use crate::generation::state_db::{
    FingerprintTable, GenerationState, LoadedGenerationState, MergeReverseIndex, PathReverseIndex, ReverseIndexes,
    UnitFingerprintTables, diff,
};
use crate::generation::units::MergeUnitKey;

/// Builds a real, comparable [`GenerationDiff`] from mesh/merge fingerprint tables.
fn owner_diff(
    prev_mesh: &[(&str, u8)],
    cur_mesh: &[(&str, u8)],
    prev_merge: &[(i32, i32, u8)],
    cur_merge: &[(i32, i32, u8)],
) -> GenerationDiff {
    let state = |mesh: &[(&str, u8)], merge: &[(i32, i32, u8)]| GenerationState {
        units: UnitFingerprintTables {
            mesh: FingerprintTable::from_entries(mesh.iter().map(|(key, value)| ((*key).to_owned(), [*value; 32]))),
            merge: FingerprintTable::from_entries(
                merge.iter().map(|(x, y, value)| (MergeUnitKey::new(*x, *y), [*value; 32])),
            ),
            ..UnitFingerprintTables::default()
        },
        ..GenerationState::default()
    };
    diff(
        &state(cur_mesh, cur_merge),
        &LoadedGenerationState::Loaded(state(prev_mesh, prev_merge)),
        false,
    )
    .unwrap()
}

fn path_reverse(entries: &[(&str, &[&str])]) -> PathReverseIndex {
    let mut index: PathReverseIndex = entries
        .iter()
        .map(|(key, consumers)| ((*key).to_owned(), consumers.iter().map(|value| (*value).to_owned()).collect()))
        .collect();
    index.sort_by(|left, right| left.0.cmp(&right.0));
    index
}

fn merge_reverse(entries: &[(&str, &[(i32, i32)])]) -> MergeReverseIndex {
    let mut index: MergeReverseIndex = entries
        .iter()
        .map(|(key, consumers)| {
            (
                (*key).to_owned(),
                consumers.iter().map(|(x, y)| MergeUnitKey::new(*x, *y)).collect(),
            )
        })
        .collect();
    index.sort_by(|left, right| left.0.cmp(&right.0));
    for (_, values) in &mut index {
        values.sort_unstable();
    }
    index
}

fn reverses(mesh_to_merge: &[(&str, &[(i32, i32)])], texture_to_mesh: &[(&str, &[&str])]) -> ReverseIndexes {
    ReverseIndexes {
        mesh_to_merge: merge_reverse(mesh_to_merge),
        texture_to_mesh: path_reverse(texture_to_mesh),
        texture_to_terrain_cell: Vec::new(),
    }
}

fn binding(added: &[&str], removed: &[&str], changed: &[&str]) -> BindingDelta {
    let owned = |keys: &[&str]| keys.iter().map(|key| (*key).to_owned()).collect();
    BindingDelta {
        added: owned(added),
        removed: owned(removed),
        changed: owned(changed),
        unchanged: 0,
    }
}

fn deltas(opaque: Option<BindingDelta>, alpha: Option<BindingDelta>) -> AtlasTextureSet<Option<BindingDelta>> {
    AtlasTextureSet { opaque, alpha }
}

fn empty_deltas() -> AtlasTextureSet<Option<BindingDelta>> {
    deltas(Some(binding(&[], &[], &[])), Some(binding(&[], &[], &[])))
}

fn shards_with(records: &[StaticRecordKey]) -> [StaticShardState; STATIC_MESH_SHARD_COUNT] {
    let mut shards: [StaticShardState; STATIC_MESH_SHARD_COUNT] = std::array::from_fn(|_| StaticShardState::default());
    for record in records {
        shards[record.shard_id()].records.push(record.clone());
    }
    shards
}

fn mesh(id: &str) -> StaticRecordKey {
    StaticRecordKey::Mesh { id: id.to_owned() }
}

fn merged(cell_x: i32, cell_y: i32, group_idx: u32) -> StaticRecordKey {
    StaticRecordKey::Merged {
        cell_x,
        cell_y,
        group_idx,
    }
}

impl OwnerPlanOutcome {
    fn expect_plan(self) -> OwnerPlan {
        match self {
            OwnerPlanOutcome::Plan(plan) => plan,
            OwnerPlanOutcome::Fallback(reason) => panic!("expected a plan, got fallback {}", reason.code()),
        }
    }

    fn expect_fallback(self) -> StaticsFallbackReason {
        match self {
            OwnerPlanOutcome::Fallback(reason) => reason,
            OwnerPlanOutcome::Plan(_) => panic!("expected a fallback, got a plan"),
        }
    }
}

#[test]
fn content_dirty_mesh_dirties_its_consuming_cell_and_shards() {
    let diff = owner_diff(&[("a", 1)], &[("a", 9)], &[], &[]);
    let current = reverses(&[("a", &[(1, 0)])], &[]);
    let previous = reverses(&[], &[]);
    let previous_shards = shards_with(&[mesh("a"), merged(1, 0, 0)]);
    let current_merged = [merged(1, 0, 0)];

    let plan = plan_static_owners(
        &diff,
        &empty_deltas(),
        &current,
        &previous,
        &previous_shards,
        &current_merged,
        &[0; 32],
        &[0; 32],
    )
    .expect_plan();

    assert_eq!(plan.dirty_meshes, HashSet::from(["a".to_owned()]));
    assert_eq!(plan.dirty_cells, HashSet::from([MergeUnitKey::new(1, 0)]));
    assert_eq!(
        plan.dirty_shards,
        BTreeSet::from([mesh("a").shard_id(), merged(1, 0, 0).shard_id()])
    );
}

#[test]
fn changed_binding_dirties_consumers_and_their_cells() {
    // No mesh or merge fingerprint changed; only an atlas binding did.
    let diff = owner_diff(&[("b", 1), ("c", 1)], &[("b", 1), ("c", 1)], &[], &[]);
    let current = reverses(&[("b", &[(2, 0)])], &[("tex1", &["b", "c"])]);
    let previous = reverses(&[], &[]);
    let previous_shards = shards_with(&[]);

    let plan = plan_static_owners(
        &diff,
        &deltas(Some(binding(&[], &[], &["tex1"])), Some(binding(&[], &[], &[]))),
        &current,
        &previous,
        &previous_shards,
        &[],
        &[0; 32],
        &[0; 32],
    )
    .expect_plan();

    assert_eq!(plan.dirty_meshes, HashSet::from(["b".to_owned(), "c".to_owned()]));
    assert_eq!(plan.dirty_cells, HashSet::from([MergeUnitKey::new(2, 0)]));
}

#[test]
fn added_binding_with_a_clean_consumer_falls_back() {
    // "d" is clean, but a new binding for its texture implies its packed bytes changed.
    let diff = owner_diff(&[("d", 1)], &[("d", 1)], &[], &[]);
    let current = reverses(&[], &[("texA", &["d"])]);
    let previous = reverses(&[], &[]);

    let reason = plan_static_owners(
        &diff,
        &deltas(Some(binding(&["texA"], &[], &[])), Some(binding(&[], &[], &[]))),
        &current,
        &previous,
        &shards_with(&[]),
        &[],
        &[0; 32],
        &[0; 32],
    )
    .expect_fallback();
    assert_eq!(reason, StaticsFallbackReason::BindingConsumerInvariantViolated);
}

#[test]
fn removed_binding_consumer_must_be_dirty_or_removed() {
    // Removed binding whose only prior consumer was removed: the invariant holds.
    let diff = owner_diff(&[("e", 1)], &[], &[], &[]);
    let previous = reverses(&[], &[("texR", &["e"])]);
    plan_static_owners(
        &diff,
        &deltas(Some(binding(&[], &["texR"], &[])), Some(binding(&[], &[], &[]))),
        &reverses(&[], &[]),
        &previous,
        &shards_with(&[]),
        &[],
        &[0; 32],
        &[0; 32],
    )
    .expect_plan();

    // Removed binding whose prior consumer "f" is neither dirty nor removed: fail closed.
    let diff = owner_diff(&[("e", 1), ("f", 1)], &[("f", 1)], &[], &[]);
    let previous = reverses(&[], &[("texR", &["e", "f"])]);
    let reason = plan_static_owners(
        &diff,
        &deltas(Some(binding(&[], &["texR"], &[])), Some(binding(&[], &[], &[]))),
        &reverses(&[], &[]),
        &previous,
        &shards_with(&[]),
        &[],
        &[0; 32],
        &[0; 32],
    )
    .expect_fallback();
    assert_eq!(reason, StaticsFallbackReason::BindingConsumerInvariantViolated);
}

#[test]
fn dirty_shard_set_covers_prior_records_added_owners_and_moved_merged_keys() {
    // "new_mesh" is added (no prior record); cell (5,5) is content-dirty and gains a second group.
    let diff = owner_diff(&[], &[("new_mesh", 1)], &[(5, 5, 1)], &[(5, 5, 9)]);
    let current = reverses(&[], &[]);
    let previous = reverses(&[], &[]);
    // Prior records: cell (5,5) group 0 (dirty), plus a genuinely clean ordinary owner.
    let previous_shards = shards_with(&[merged(5, 5, 0), mesh("clean")]);
    // The current merge plan re-emits group 0 and adds a new group 1 in the same dirty cell.
    let current_merged = [merged(5, 5, 0), merged(5, 5, 1)];

    let plan = plan_static_owners(
        &diff,
        &empty_deltas(),
        &current,
        &previous,
        &previous_shards,
        &current_merged,
        &[0; 32],
        &[0; 32],
    )
    .expect_plan();

    assert_eq!(plan.dirty_meshes, HashSet::from(["new_mesh".to_owned()]));
    assert_eq!(plan.dirty_cells, HashSet::from([MergeUnitKey::new(5, 5)]));
    // (a) prior merged(5,5,0), (b) added new_mesh, (c) moved merged(5,5,1). The clean owner's
    // shard is only present if it happens to collide with one of these targets.
    let expected = BTreeSet::from([
        merged(5, 5, 0).shard_id(),
        merged(5, 5, 1).shard_id(),
        mesh("new_mesh").shard_id(),
    ]);
    assert_eq!(plan.dirty_shards, expected);
}

#[test]
fn removed_owners_dirty_their_prior_shards() {
    let diff = owner_diff(&[("gone", 1)], &[], &[(7, 7, 1)], &[]);
    let previous_shards = shards_with(&[mesh("gone"), merged(7, 7, 0), mesh("kept")]);

    let plan = plan_static_owners(
        &diff,
        &empty_deltas(),
        &reverses(&[], &[]),
        &reverses(&[], &[]),
        &previous_shards,
        &[],
        &[0; 32],
        &[0; 32],
    )
    .expect_plan();

    assert_eq!(plan.removed_meshes, HashSet::from(["gone".to_owned()]));
    assert_eq!(plan.removed_cells, HashSet::from([MergeUnitKey::new(7, 7)]));
    assert_eq!(
        plan.dirty_shards,
        BTreeSet::from([mesh("gone").shard_id(), merged(7, 7, 0).shard_id()])
    );
}

#[test]
fn preconditions_fall_back_with_stable_reasons() {
    let clean = owner_diff(&[("a", 1)], &[("a", 1)], &[], &[]);

    // Not comparable (forced rebuild).
    let forced = diff(
        &GenerationState::default(),
        &LoadedGenerationState::Loaded(GenerationState::default()),
        true,
    )
    .unwrap();
    assert_eq!(
        plan_static_owners(
            &forced,
            &empty_deltas(),
            &reverses(&[], &[]),
            &reverses(&[], &[]),
            &shards_with(&[]),
            &[],
            &[0; 32],
            &[0; 32]
        )
        .expect_fallback(),
        StaticsFallbackReason::NoComparablePreviousState
    );

    // Record recipe changed.
    assert_eq!(
        plan_static_owners(
            &clean,
            &empty_deltas(),
            &reverses(&[], &[]),
            &reverses(&[], &[]),
            &shards_with(&[]),
            &[],
            &[1; 32],
            &[0; 32]
        )
        .expect_fallback(),
        StaticsFallbackReason::RecordRecipeChanged
    );

    // A missing family binding delta.
    assert_eq!(
        plan_static_owners(
            &clean,
            &deltas(None, Some(binding(&[], &[], &[]))),
            &reverses(&[], &[]),
            &reverses(&[], &[]),
            &shards_with(&[]),
            &[],
            &[0; 32],
            &[0; 32]
        )
        .expect_fallback(),
        StaticsFallbackReason::BindingDeltaUnavailable
    );
}
