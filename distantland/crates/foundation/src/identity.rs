//! Run-scoped content identities captured where source bytes enter the pipeline.

use std::path::{Path, PathBuf};

use crate::IndexMap;
use crate::units::CanonicalWriter;
use anyhow::Result;
use itertools::Itertools;

/// BLAKE3 identity of a consumed source byte buffer.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SourceHash(pub [u8; 32]);

impl SourceHash {
    /// Hashes a consumed byte buffer without reading its source again.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let hash = blake3::hash(bytes);
        Self(*hash.as_bytes())
    }
}

/// Content identity of one consumed source; both the length and the digest feed
/// unit fingerprints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentIdentity {
    pub byte_len: u64,
    pub hash: SourceHash,
}

impl ContentIdentity {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            byte_len: bytes.len() as u64,
            hash: SourceHash::from_bytes(bytes),
        }
    }
}

/// Path-associated content identity for ordered file inputs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileIdentity {
    /// Source path as resolved for this run.
    pub path: PathBuf,
    /// Identity of the bytes read from `path`.
    pub content: ContentIdentity,
}

impl FileIdentity {
    /// Captures a path and bytes that the caller already read.
    pub fn from_bytes(path: &Path, bytes: &[u8]) -> Self {
        Self {
            path: path.to_path_buf(),
            content: ContentIdentity::from_bytes(bytes),
        }
    }
}

/// MGE-XE mesh override rule that selected a resolved asset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeshResolutionRule {
    /// The requested base mesh was selected unchanged.
    Original,
    /// A sibling `_dist.nif` mesh was selected.
    Dist,
    /// An `x*.nif` mesh with a matching `x*.kf` companion was selected.
    XWithKf,
}

/// VFS selection fact for one requested mesh key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshResolutionFact {
    /// Override-selection rule that selected the consumed asset.
    pub rule: MeshResolutionRule,
    /// Final normalized VFS key selected for consumption.
    pub resolved_key: String,
}

/// Content identities accumulated during one generation run.
#[derive(Debug, Default)]
pub struct ContentIdentityCollector {
    plugins: Vec<FileIdentity>,
    grass_plugins: Vec<FileIdentity>,
    meshes: IndexMap<String, ContentIdentity>,
    terrain_textures: IndexMap<String, SourceHash>,
    overrides: Vec<FileIdentity>,
    metadata: Vec<FileIdentity>,
    mesh_resolutions: IndexMap<String, MeshResolutionFact>,
}

impl ContentIdentityCollector {
    /// Replaces the ordered active-plugin identity table.
    pub fn set_plugins(&mut self, plugins: Vec<FileIdentity>) {
        self.plugins = plugins;
    }

    /// Returns active-plugin identities in load order.
    pub fn plugins(&self) -> &[FileIdentity] {
        &self.plugins
    }

    /// Replaces the sorted grass-plugin identity table.
    pub fn set_grass_plugins(&mut self, grass_plugins: Vec<FileIdentity>) {
        self.grass_plugins = grass_plugins;
    }

    /// Returns grass-plugin identities in sorted filename order.
    pub fn grass_plugins(&self) -> &[FileIdentity] {
        &self.grass_plugins
    }

    /// Returns override identities in override precedence order.
    pub fn overrides(&self) -> &[FileIdentity] {
        &self.overrides
    }

    /// Returns discovered metadata identities in plugin load order.
    pub fn metadata(&self) -> &[FileIdentity] {
        &self.metadata
    }

    /// Returns consumed mesh identities keyed by requested normalized mesh key.
    pub fn meshes(&self) -> &IndexMap<String, ContentIdentity> {
        &self.meshes
    }

    /// Returns terrain source identities captured by terrain package preparation.
    pub fn terrain_textures(&self) -> &IndexMap<String, SourceHash> {
        &self.terrain_textures
    }

    /// Returns mesh resolution facts keyed by requested normalized mesh key.
    pub fn mesh_resolutions(&self) -> &IndexMap<String, MeshResolutionFact> {
        &self.mesh_resolutions
    }

    /// Records an override source in override precedence order.
    pub fn record_override(&mut self, identity: FileIdentity) {
        self.overrides.push(identity);
    }

    /// Records a metadata source in plugin load order.
    pub fn record_metadata(&mut self, identity: FileIdentity) {
        self.metadata.push(identity);
    }

    /// Records the resolution decision for a requested mesh, even if reading later fails.
    pub fn record_mesh_resolution(&mut self, requested_key: String, fact: MeshResolutionFact) {
        self.mesh_resolutions.insert(requested_key, fact);
    }

    /// Records bytes consumed for a requested mesh.
    pub fn record_mesh(&mut self, requested_key: String, identity: ContentIdentity) {
        self.meshes.insert(requested_key, identity);
    }

    /// Records bytes consumed specifically by terrain package preparation.
    pub fn record_terrain_texture(&mut self, key: String, identity: SourceHash) {
        if let Some(previous) = self.terrain_textures.insert(key.clone(), identity) {
            debug_assert_eq!(previous, identity, "terrain texture content changed during generation: {key}");
        }
    }
}

struct NormalizedFileIdentity {
    path: String,
    hash: [u8; 32],
}

fn normalize_path(path: &Path) -> String {
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    resolved.to_string_lossy().replace('/', "\\").to_ascii_lowercase()
}

fn normalized_file(identity: &FileIdentity, grass: bool) -> NormalizedFileIdentity {
    let FileIdentity { path, content } = identity;
    let ContentIdentity { byte_len: _, hash } = content;
    let path = if grass {
        path.file_name()
            .unwrap_or_else(|| path.as_os_str())
            .to_string_lossy()
            .replace('/', "\\")
            .to_ascii_lowercase()
    } else {
        normalize_path(path)
    };
    NormalizedFileIdentity { path, hash: hash.0 }
}

fn write_file_identities(writer: &mut CanonicalWriter, identities: &[NormalizedFileIdentity]) {
    writer.write_u64(identities.len() as u64);
    for identity in identities {
        writer.write_str(&identity.path);
        writer.write_bytes(&identity.hash);
    }
}

/// Assembles the content identity shared by generation and startup.
pub fn load_order_identity(
    plugins: &[FileIdentity],
    overrides: &[FileIdentity],
    metadata: &[FileIdentity],
    grass_plugins: &[FileIdentity],
) -> Result<String> {
    let plugins = plugins.iter().map(|identity| normalized_file(identity, false)).collect_vec();
    let overrides = overrides
        .iter()
        .map(|identity| normalized_file(identity, false))
        .collect_vec();
    let metadata = metadata.iter().map(|identity| normalized_file(identity, false)).collect_vec();
    let grass_plugins = grass_plugins
        .iter()
        .map(|identity| normalized_file(identity, true))
        .sorted_unstable_by(|left, right| left.path.cmp(&right.path).then_with(|| left.hash.cmp(&right.hash)))
        .collect_vec();
    let mut writer = CanonicalWriter::new();
    writer.write_str("load_order_identity_v1");
    writer.write_u32(1);
    write_file_identities(&mut writer, &plugins);
    write_file_identities(&mut writer, &overrides);
    write_file_identities(&mut writer, &metadata);
    write_file_identities(&mut writer, &grass_plugins);
    Ok(blake3::Hash::from(writer.finish()).to_hex().to_string())
}

/// Builds a load-order identity from identities captured during generation.
pub fn collected_load_order_identity(collector: &ContentIdentityCollector) -> Result<String> {
    load_order_identity(
        collector.plugins(),
        collector.overrides(),
        collector.metadata(),
        collector.grass_plugins(),
    )
}

#[cfg(test)]
mod tests;
