use super::*;
use crate::{STATIC_ERROR_TEXTURE_DDS, STATIC_ERROR_TEXTURE_KEY};

/// Holds the scanned mappings for relevant asset types across all tracked data directories.
///
/// This separates meshes and textures into distinct maps to speed up lookups and avoid
/// collisions if identically-named files exist across different asset directories.
///
#[derive(Debug, Default)]
pub struct DirectoryMaps {
    /// Mesh asset mappings rooted at `Meshes\`.
    pub meshes: AssetMap,
    /// Texture asset mappings rooted at `Textures\`.
    pub textures: AssetMap,
}

/// Physical source for a resolved asset map entry.
#[derive(Debug)]
pub enum AssetSource {
    /// Loose file on disk.
    Loose {
        /// Absolute filesystem path.
        path: PathBuf,
    },
    /// Entry stored inside a BSA archive.
    Bsa {
        /// Index into the loaded archive table.
        archive_index: usize,
        /// Raw archive entry name bytes.
        entry_name: Vec<u8>,
    },
    /// Built-in generator asset.
    Embedded {
        /// Static asset bytes.
        bytes: &'static [u8],
    },
}

impl AssetSource {
    /// Returns the loose-file path when this asset is not backed by a BSA.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Loose { path, .. } => Some(path),
            Self::Bsa { .. } | Self::Embedded { .. } => None,
        }
    }
}

/// A specialized map that associates normalized asset keys with their physical sources.
///
/// Indices are stable for the lifetime of a built VFS: construction only inserts or
/// replaces entries, and the map is read-only after it is published.
///
#[derive(Debug, Default)]
pub struct AssetMap {
    inner: IndexMap<NormalizedString, AssetSource>,
}

/// Outcome of attempting to insert a loose file into an [`AssetMap`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LooseInsertOutcome {
    /// The key was not already present; a new entry was created.
    Inserted,
    /// An existing loose-file entry was overwritten by this newer loose file.
    ReplacedLoose,
    /// An existing BSA entry was displaced because the loose file is newer.
    ReplacedBsa,
    /// The BSA entry is at least as new as the loose file; the loose file was discarded.
    RejectedByBsa,
    /// A reserved embedded generator asset owns this key.
    RejectedReserved,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum AssetDir {
    Meshes,
    Textures,
}

impl AssetDir {
    pub(crate) fn map_mut(self, maps: &mut DirectoryMaps) -> &mut AssetMap {
        match self {
            Self::Meshes => &mut maps.meshes,
            Self::Textures => &mut maps.textures,
        }
    }
}

impl DirectoryMaps {
    /// Force-inserts the generator-owned static error texture after normal discovery.
    pub(crate) fn insert_embedded_error_texture(&mut self) {
        self.textures.insert_reserved_embedded(
            STATIC_ERROR_TEXTURE_KEY,
            AssetSource::Embedded {
                bytes: STATIC_ERROR_TEXTURE_DDS,
            },
        );
    }
}

#[derive(Debug)]
pub(crate) struct LooseCandidate {
    /// Index into the data-directory list; used to enforce inter-directory precedence
    /// (higher index = higher priority).
    pub(crate) data_dir_index: usize,
    pub(crate) asset_dir: AssetDir,
    /// Pre-normalized (lowercase, backslash separators) key relative to the asset directory.
    pub(crate) normalized_key: String,
    pub(crate) absolute_path: PathBuf,
    /// File modification time; compared against a BSA's modification time to decide precedence.
    pub(crate) last_write_time: SystemTime,
}

#[derive(Debug)]
pub(crate) struct LooseScanTask {
    pub(crate) data_dir_index: usize,
    pub(crate) asset_dir: AssetDir,
    pub(crate) root_path: PathBuf,
}

impl AssetMap {
    /// Inserts a new mapping, asserting in debug builds that the key is already fully normalized.
    pub fn insert_normalized(&mut self, key: impl Into<NormalizedString>, value: AssetSource) -> bool {
        self.inner.insert(key.into(), value).is_some()
    }

    /// Inserts a reserved embedded asset, ensuring it wins over any loose or BSA entry.
    ///
    /// This uses `shift_remove` to evict any existing entry before inserting the embedded
    /// one, which **temporarily** shifts later entries' indices. That's safe because this
    /// is called during VFS construction, before the map is published and indexed queries
    /// begin. The invariant of stable indices for the published map is preserved.
    fn insert_reserved_embedded(&mut self, key: &'static str, value: AssetSource) {
        debug_assert!(is_normalized(key));
        let normalized = NormalizedStr::from_normalized(key);
        self.inner.shift_remove(normalized);
        self.insert_normalized(NormalizedString::from_normalized(key), value);
    }

    /// Inserts a loose-file mapping, preserving a newer loose file over an older BSA entry.
    ///
    /// `archive_mtimes` maps each `archive_index` to its archive's modification time; it is
    /// used to resolve loose-vs-BSA precedence when the existing entry is BSA-backed.
    pub(crate) fn insert_loose_normalized(
        &mut self,
        key: String,
        path: PathBuf,
        last_write_time: SystemTime,
        archive_mtimes: &[SystemTime],
    ) -> LooseInsertOutcome {
        let key = NormalizedString::from(key);
        let existing = self.get_key_value(&key).map(|(_, source)| source);
        let existing_bsa_modified = match existing {
            Some(AssetSource::Bsa { archive_index, .. }) => archive_mtimes.get(*archive_index).copied(),
            Some(AssetSource::Embedded { .. }) => return LooseInsertOutcome::RejectedReserved,
            _ => None,
        };

        if let Some(archive_modified) = existing_bsa_modified
            && last_write_time <= archive_modified
        {
            return LooseInsertOutcome::RejectedByBsa;
        }

        let replaced = self.inner.insert(key, AssetSource::Loose { path });
        match replaced {
            Some(AssetSource::Loose { .. }) => LooseInsertOutcome::ReplacedLoose,
            Some(AssetSource::Bsa { .. }) => LooseInsertOutcome::ReplacedBsa,
            Some(AssetSource::Embedded { .. }) => LooseInsertOutcome::RejectedReserved,
            None => LooseInsertOutcome::Inserted,
        }
    }

    pub fn get(&self, key: &NormalizedStr) -> Option<&AssetSource> {
        self.get_key_value(key).map(|(_, source)| source)
    }

    pub fn get_key_value(&self, key: &NormalizedStr) -> Option<(&str, &AssetSource)> {
        self.inner.get_key_value(key).map(|(key, source)| (key.as_str(), source))
    }

    pub fn contains_key_parts_normalized(&self, parts: &[&str]) -> bool {
        let hash = self.hash_parts_from_slice(parts);
        self.inner
            .raw_entry_v1()
            .from_hash(hash, |key| key_equals_parts_from_slice(key.as_str(), parts))
            .is_some()
    }

    pub fn get_key_value_parts_normalized(&self, parts: &[&str]) -> Option<(&str, &AssetSource)> {
        let hash = self.hash_parts_from_slice(parts);
        self.inner
            .raw_entry_v1()
            .from_hash(hash, |key| key_equals_parts_from_slice(key.as_str(), parts))
            .map(|(key, value)| (key.as_str(), value))
    }

    pub fn get_index_of(&self, key: &NormalizedStr) -> Option<usize> {
        self.inner.get_index_of(key)
    }

    pub fn get_index(&self, index: usize) -> Option<(&str, &AssetSource)> {
        self.inner.get_index(index).map(|(key, source)| (key.as_str(), source))
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Computes a hash over raw string slices that are already normalized (no per-byte
    /// conversion applied).
    fn hash_parts_from_slice(&self, parts: &[&str]) -> u64 {
        self.inner.hasher().hash_one(NormalizedParts(parts))
    }
}

struct NormalizedParts<'a>(&'a [&'a str]);

impl Hash for NormalizedParts<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for part in self.0 {
            hash_normalized_bytes(state, part);
        }
        finish_asset_key_hash(state);
    }
}
