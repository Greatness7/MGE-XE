//! Dirty-shard readback and validation.

use std::collections::BTreeSet;
use std::path::PathBuf;

use rayon::prelude::*;

use super::plan::StaticsFallbackReason;
use crate::generation::output::STATIC_MESH_SHARD_COUNT;
use crate::generation::state_db::StaticShardState;
use crate::generation::storage::state::RequiredArtifact;

/// Validated prior records for the dirty shards, keyed positionally from state.
#[derive(Debug)]
pub(super) struct DecodedShards {
    pub(super) shards: [Option<crate::PackedDistantStatics>; STATIC_MESH_SHARD_COUNT],
    pub(super) bytes_read: u64,
}

/// Distinguishes a structurally invalid shard header from a payload that failed to decode.
///
/// The deserializer reports both as `io::Error`, so the message is the only signal available.
fn decode_failure_reason(error: &std::io::Error) -> StaticsFallbackReason {
    let message = error.to_string();
    if message.contains("magic mismatch")
        || message.contains("version mismatch")
        || message.contains("header")
        || message.contains("record size")
        || message.contains("vertex stride")
        || message.contains("index element size")
    {
        StaticsFallbackReason::ShardHeaderInvalid
    } else {
        StaticsFallbackReason::ShardDecodeFailed
    }
}

/// Verifies one committed shard and rebuilds its records under the previous state's keys.
///
/// Returns the shard's byte length alongside the restored records so the caller can total the
/// bytes actually read back.
fn decode_one_dirty_shard(
    shard_path: &std::path::Path,
    artifact: Option<&RequiredArtifact>,
    previous_shard: &StaticShardState,
) -> std::result::Result<(u64, crate::PackedDistantStatics), StaticsFallbackReason> {
    let artifact = artifact.ok_or(StaticsFallbackReason::BaseArtifactMissing)?;
    let file = std::fs::File::open(shard_path).map_err(|_| StaticsFallbackReason::ShardUnreadable)?;
    // SAFETY: The map is read-only and lives only for this decode. The exclusive writer session
    // prevents replacement or mutation of the shard while the map is live.
    let bytes = unsafe { memmap2::MmapOptions::new().map(&file) }.map_err(|_| StaticsFallbackReason::ShardUnreadable)?;
    let actual_length = bytes.len() as u64;
    if actual_length != artifact.byte_length || *blake3::hash(&bytes).as_bytes() != artifact.content_blake3 {
        return Err(StaticsFallbackReason::ShardHashMismatch);
    }

    let records =
        crate::mge_xe::distant_statics::deserialize_static_meshes(&bytes).map_err(|error| decode_failure_reason(&error))?;
    let keys = &previous_shard.records;
    if records.len() != keys.len() {
        return Err(StaticsFallbackReason::ShardCountMismatch);
    }

    let mut shard = crate::PackedDistantStatics::default();
    for (key, record) in keys.iter().zip(records) {
        shard.insert(key.render(), record);
    }
    Ok((actual_length, shard))
}

/// Verifies and decodes every dirty shard, leaving clean shards untouched on disk.
///
/// # Errors
///
/// Returns the failure reason belonging to the lowest-numbered failing shard, so a run's reported
/// degradation cause does not depend on parallel completion order.
pub(super) fn decode_dirty_shards(
    shard_paths: &[PathBuf; STATIC_MESH_SHARD_COUNT],
    base_shards: &[Option<&RequiredArtifact>; STATIC_MESH_SHARD_COUNT],
    dirty_shards: &BTreeSet<usize>,
    previous_shards: &[StaticShardState; STATIC_MESH_SHARD_COUNT],
) -> std::result::Result<DecodedShards, StaticsFallbackReason> {
    let outcomes: Vec<_> = dirty_shards
        .par_iter()
        .map(|&shard_id| {
            (
                shard_id,
                decode_one_dirty_shard(&shard_paths[shard_id], base_shards[shard_id], &previous_shards[shard_id]),
            )
        })
        .collect();

    if let Some((_, reason)) = outcomes
        .iter()
        .filter_map(|(shard_id, outcome)| outcome.as_ref().err().map(|reason| (*shard_id, *reason)))
        .min_by_key(|(shard_id, _)| *shard_id)
    {
        return Err(reason);
    }

    let mut decoded = DecodedShards {
        shards: std::array::from_fn(|_| None),
        bytes_read: 0,
    };
    for (shard_id, outcome) in outcomes {
        let (bytes_read, shard) = outcome.expect("all dirty-shard decode failures returned above");
        decoded.bytes_read = decoded.bytes_read.saturating_add(bytes_read);
        decoded.shards[shard_id] = Some(shard);
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests;
