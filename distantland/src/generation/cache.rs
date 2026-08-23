//! Input fingerprinting and compare-then-write support for observability files.

use std::fs;
use std::path::Path;

use crate::generation::metrics::OutputWriteDecision;

mod fingerprint;

pub(crate) use distantland_foundation::record_key::{STATIC_SHARD_ASSIGNMENT_MAGIC, static_mesh_shard_id};
pub(crate) use fingerprint::*;

/// Static-mesh input digest domain tag.
#[cfg(test)]
pub(crate) const STATIC_MESHES_INPUT_FINGERPRINT_MAGIC: &[u8] = b"distantland_static_meshes_input_v2\n";
/// Static-shard input digest domain tag.
pub(crate) const STATIC_SHARD_INPUT_FINGERPRINT_MAGIC: &[u8] = b"tes3-distantland-static-shard-input-v1\0";
/// Terrain-package digest domain tag.
pub(crate) const TERRAIN_PACKAGE_INPUT_FINGERPRINT_MAGIC: &[u8] = b"distantland_terrain_package_input_v2\n";
/// Static-bundle digest domain tag.
///
/// Bumping the version invalidates the embedded index state when optimize/merge/serialize semantics
/// change beyond the other hashed inputs.
pub(crate) const STATIC_BUNDLE_INPUT_FINGERPRINT_MAGIC: &[u8] = b"distantland_static_bundle_input_v4\n";

/// Writes a payload only when its bytes differ from the existing file.
///
/// This is used for non-authoritative observability outputs such as the generation report, which
/// is absent from the committed inventory. The comparison short-circuits on the recorded length so
/// a differently-sized report is never read into memory just to be declared different.
pub(crate) fn write_plain_bytes_if_changed(output_path: &Path, bytes: &[u8]) -> std::io::Result<OutputWriteDecision> {
    let unchanged = fs::metadata(output_path).is_ok_and(|metadata| metadata.len() == bytes.len() as u64)
        && fs::read(output_path).is_ok_and(|existing| existing == bytes);
    if unchanged {
        return Ok(OutputWriteDecision::SkippedUnchanged);
    }
    fs::write(output_path, bytes)?;
    Ok(OutputWriteDecision::Written)
}

#[cfg(test)]
mod tests;
