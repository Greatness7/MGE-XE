//! Input-identity computation for generation requests and static mesh shards.

use anyhow::Result;
use bytemuck::NoUninit;
use bytemuck::bytes_of;
use bytemuck::cast_slice;
use rayon::prelude::*;

use crate::generation::job::{GenerationJob, write_generation_settings_canonical};
use crate::generation::output::STATIC_MESH_SHARD_COUNT;
use crate::generation::units::CanonicalWriter;
use crate::mge_xe::distant_statics::StaticType;

#[cfg(test)]
use super::STATIC_MESHES_INPUT_FINGERPRINT_MAGIC;
use super::STATIC_SHARD_INPUT_FINGERPRINT_MAGIC;
use distantland_foundation::units::STATICS_RECIPE_VERSION;

/// Returns a fingerprint for the logical generation request.
///
/// Output location and `force_rebuild` are intentionally ignored so cached or staged
/// generation can still validate against the same requested content.
///
/// `auto_sync_plugins` is ignored for the same reason: it is the policy that *chose* `plugins`, not
/// part of the content. Hashing it would invalidate the cache when the flag is toggled even though
/// the resulting load order is identical.
pub(crate) fn fingerprint_generation_request(job: &GenerationJob) -> Result<String> {
    let GenerationJob {
        morrowind_ini,
        data_dirs,
        plugins,
        grass_plugins,
        output_root: _,
        auto_sync_plugins: _,
        settings,
    } = job;
    let grass_plugins = grass_plugins.as_ref().filter(|plugins| !plugins.is_empty()).map(|plugins| {
        let mut plugins = plugins.clone();
        plugins.sort_unstable_by_key(|path| path.to_string_lossy().to_ascii_lowercase());
        plugins
    });

    let mut writer = CanonicalWriter::new();
    writer.write_str("generation_job_request_v1");
    writer.write_u32(1);
    write_optional_path(&mut writer, morrowind_ini.as_deref());
    write_optional_paths(&mut writer, data_dirs.as_deref());
    write_optional_paths(&mut writer, plugins.as_deref());
    write_optional_paths(&mut writer, grass_plugins.as_deref());
    write_generation_settings_canonical(&mut writer, settings);
    Ok(blake3::Hash::from(writer.finish()).to_hex().to_string())
}

fn write_optional_path(writer: &mut CanonicalWriter, path: Option<&std::path::Path>) {
    writer.write_bool(path.is_some());
    if let Some(path) = path {
        writer.write_str(&path.to_string_lossy());
    }
}

fn write_optional_paths(writer: &mut CanonicalWriter, paths: Option<&[std::path::PathBuf]>) {
    writer.write_bool(paths.is_some());
    if let Some(paths) = paths {
        writer.write_u64(paths.len() as u64);
        for path in paths {
            writer.write_str(&path.to_string_lossy());
        }
    }
}

/// Computes a fingerprint for the final static-mesh serialization inputs.
#[cfg(test)]
pub(crate) fn fingerprint_static_meshes_inputs(distant_statics: &crate::PackedDistantStatics) -> [u8; 32] {
    // Each static is fingerprinted independently in parallel; the per-static digests are then
    // folded into the final hash in iteration order so the result stays order-sensitive.
    let digests: Vec<[u8; 32]> = distant_statics
        .par_iter()
        .map(|(key, static_mesh)| fingerprint_static_mesh_input(key, static_mesh))
        .collect();

    let mut hasher = blake3::Hasher::new();
    hasher.update(STATIC_MESHES_INPUT_FINGERPRINT_MAGIC);
    hash_pod(&mut hasher, &STATICS_RECIPE_VERSION);
    hash_pod(&mut hasher, &(distant_statics.len() as u64));
    for digest in &digests {
        hasher.update(digest);
    }
    *hasher.finalize().as_bytes()
}

/// Computes the complete final-input digest for one static-mesh shard.
pub(crate) fn fingerprint_static_mesh_shard_inputs(
    shard_id: usize,
    distant_statics: &crate::PackedDistantStatics,
) -> [u8; 32] {
    assert!(shard_id < STATIC_MESH_SHARD_COUNT, "static shard id is out of range");
    let digests: Vec<[u8; 32]> = distant_statics
        .par_iter()
        .map(|(key, static_mesh)| fingerprint_static_mesh_input(key, static_mesh))
        .collect();

    let mut hasher = blake3::Hasher::new();
    hasher.update(STATIC_SHARD_INPUT_FINGERPRINT_MAGIC);
    hash_pod(&mut hasher, &STATICS_RECIPE_VERSION);
    hash_pod(&mut hasher, &(STATIC_MESH_SHARD_COUNT as u32));
    hash_pod(&mut hasher, &(shard_id as u32));
    hash_pod(&mut hasher, &(distant_statics.len() as u64));
    for digest in &digests {
        hasher.update(digest);
    }
    *hasher.finalize().as_bytes()
}

/// Computes the input fingerprint digest for a single distant static.
fn fingerprint_static_mesh_input(key: &str, static_mesh: &crate::mge_xe::distant_statics::PackedDistantStatic) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hash_str(&mut hasher, key);
    hash_pod(&mut hasher, &(static_mesh.static_type as u32));
    hash_pod(&mut hasher, &(static_mesh.subsets.len() as u64));
    hash_pod(&mut hasher, &static_mesh.bounding_sphere);
    hash_pod(&mut hasher, &static_mesh.bounding_box);
    let is_grass = static_mesh.static_type == StaticType::StaticGrass;
    for subset in &static_mesh.subsets {
        hash_pod(&mut hasher, &subset.bounding_sphere);
        hash_pod(&mut hasher, &subset.bounding_box);
        hash_pod(&mut hasher, &subset.horizon_footprint);
        hash_pod(&mut hasher, &(subset.vertices.len() as u64));
        hash_pod(&mut hasher, &(subset.triangles.len() as u64));
        // Hash the bytes that are actually serialized: the grass projection for grass, the full
        // vertex for everything else.
        if is_grass {
            for v in &subset.vertices {
                hash_pod(&mut hasher, v.as_grass());
            }
        } else {
            hasher.update(cast_slice(&subset.vertices));
        }
        hasher.update(cast_slice(&subset.triangles));
        hash_pod(&mut hasher, &(subset.components.len() as u64));
        hasher.update(cast_slice(&subset.components));
        // The atlas rect lives here rather than in the vertex, so the palette must be hashed
        // explicitly: without it, repacking the atlas would move a subset's UVs without changing
        // its shard fingerprint, and incremental generation would carry the stale shard.
        hash_pod(&mut hasher, &(subset.palette.len() as u64));
        hasher.update(cast_slice(&subset.palette));
        hasher.update(&[subset.has_alpha]);
        hasher.update(&[subset.has_uv_controller]);
        hash_bytes(&mut hasher, subset.texture.as_ref().as_bytes());
    }
    *hasher.finalize().as_bytes()
}

/// Hashes the raw bytes of a `NoUninit` value into `hasher`.
fn hash_pod<T: NoUninit>(hasher: &mut blake3::Hasher, value: &T) {
    hasher.update(bytes_of(value));
}

/// Hashes a string by first hashing its byte length, then its content.
///
/// The length prefix prevents collisions between adjacent strings with different splits.
fn hash_str(hasher: &mut blake3::Hasher, value: &str) {
    hash_pod(hasher, &(value.len() as u64));
    hasher.update(value.as_bytes());
}

/// Hashes a byte slice by first hashing its length, then its content.
///
/// The length prefix prevents collisions between adjacent byte slices with different splits.
fn hash_bytes(hasher: &mut blake3::Hasher, value: &[u8]) {
    hash_pod(hasher, &(value.len() as u64));
    hasher.update(value);
}
