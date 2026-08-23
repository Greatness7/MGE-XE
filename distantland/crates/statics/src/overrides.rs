//! Static override data model and merge builder for `.ovr` and plugin metadata inputs.

mod parse;

use std::fs;
use std::io::{self, Cursor};
use std::path::Path;

use hashbrown::HashMap;
use uncased::Uncased;

pub use crate::usage::{DynamicVisData, DynamicVisGroup, DynamicVisKind, StaticOverride, StaticOverrides};
use distantland_foundation::identity::FileIdentity;
use tracing::warn;

use parse::parse_override_reader;

/// Deduplication key for a dynamic-visibility group: `(kind tag, id, ranges)`.
type DedupKey = (u8, String, Vec<(i32, i32)>);

type DedupMap = HashMap<DedupKey, usize>;

/// Incrementally merges override sources into one [`StaticOverrides`].
///
/// Sources are `.ovr` override files and plugin `-metadata.toml` sections
/// (see `statics::metadata`). Later sources override earlier sources for scalar
/// settings; dynamic-visibility groups are deduplicated across all sources. When a
/// source replaces a *different* source's directive for the same key with a different
/// value, a warning naming both sources is logged.
#[derive(Default)]
pub struct OverridesBuilder {
    result: StaticOverrides,
    dedup: DedupMap,
    sources: Vec<String>,
    mesh_sources: HashMap<String, usize>,
    name_sources: HashMap<String, usize>,
    interior_sources: HashMap<Uncased<'static>, usize>,
}

impl OverridesBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn begin_source(&mut self, label: impl Into<String>) {
        self.sources.push(label.into());
    }

    pub(crate) fn current_source_label(&self) -> &str {
        self.sources.last().map_or("<unknown>", String::as_str)
    }

    /// Parses one override file and returns the identity of the same bytes supplied to the parser.
    pub fn add_override_file_with_identity(&mut self, path: &Path) -> io::Result<FileIdentity> {
        self.begin_source(path.display().to_string());
        let bytes = fs::read(path)?;
        let identity = FileIdentity::from_bytes(path, &bytes);
        let mut reader = Cursor::new(bytes.as_slice());
        parse_override_reader(&mut reader, self)?;
        Ok(identity)
    }

    /// Parses and merges `.ovr` override content supplied in memory.
    ///
    /// # Errors
    ///
    /// Returns any buffered-read error encountered while parsing `text`.
    pub fn add_override_text(&mut self, text: &[u8]) -> io::Result<()> {
        self.begin_source(format!("<override text {}>", self.sources.len() + 1));
        let mut reader = Cursor::new(text);
        parse_override_reader(&mut reader, self)
    }

    /// Finalizes the merge.
    ///
    /// MGE-XE uses 1-based dynamic-visibility indices, with 0 reserved for ordinary
    /// statics, so indices are assigned here once all sources are merged.
    pub fn finish(mut self) -> StaticOverrides {
        for (i, group) in self.result.dynamic_vis.groups.iter_mut().enumerate() {
            group.index = (i + 1) as u16;
        }
        self.result
    }

    fn current_source_index(&self) -> usize {
        self.sources.len().saturating_sub(1)
    }

    fn source_label(&self, index: usize) -> &str {
        self.sources.get(index).map_or("<unknown>", String::as_str)
    }

    /// Inserts a per-mesh override and warns on conflicting cross-source replacement.
    pub(crate) fn insert_mesh_override(&mut self, key: String, value: StaticOverride) {
        let source = self.current_source_index();
        if let (Some(prev), Some(&prev_source)) = (self.result.mesh_overrides.get(&key), self.mesh_sources.get(&key))
            && prev_source != source
            && *prev != value
        {
            warn!(
                "Mesh override '{key}' from {} replaces conflicting value from {}",
                self.source_label(source),
                self.source_label(prev_source)
            );
        }
        self.mesh_sources.insert(key.clone(), source);
        self.result.mesh_overrides.insert(key, value);
    }

    /// Inserts an object-name override and warns on conflicting cross-source replacement.
    pub(crate) fn insert_name(&mut self, key: String, enabled: bool) {
        let source = self.current_source_index();
        if let (Some(&prev), Some(&prev_source)) = (self.result.names.get(&key), self.name_sources.get(&key))
            && prev_source != source
            && prev != enabled
        {
            warn!(
                "Object override '{key}' from {} replaces conflicting value from {}",
                self.source_label(source),
                self.source_label(prev_source)
            );
        }
        self.name_sources.insert(key.clone(), source);
        self.result.names.insert(key, enabled);
    }

    /// Inserts an interior-cell override and warns on conflicting cross-source replacement.
    pub(crate) fn insert_interior(&mut self, key: Uncased<'static>, enabled: bool) {
        let source = self.current_source_index();
        if let (Some(&prev), Some(&prev_source)) = (self.result.interiors.get(&key), self.interior_sources.get(&key))
            && prev_source != source
            && prev != enabled
        {
            warn!(
                "Interior override '{key}' from {} replaces conflicting value from {}",
                self.source_label(source),
                self.source_label(prev_source)
            );
        }
        self.interior_sources.insert(key.clone(), source);
        self.result.interiors.insert(key, enabled);
    }

    /// Inserts or merges a dynamic-visibility group.
    ///
    /// `key` is the script ID for journal/global groups and the controlling object ID
    /// for unique-object groups. Duplicate groups (same kind, id, and ranges) are merged
    /// rather than inserted again; script and unique-object lookup tables are kept in sync.
    pub(crate) fn insert_dynamic_vis(&mut self, key: String, kind: DynamicVisKind) {
        let data = &mut self.result.dynamic_vis;
        let dk = dedup_key(&kind);

        let group_idx = if let Some(&idx) = self.dedup.get(&dk) {
            if let DynamicVisKind::UniqueObject { linked_ids, .. } = &kind
                && let DynamicVisKind::UniqueObject {
                    linked_ids: existing, ..
                } = &mut data.groups[idx].kind
            {
                for id in linked_ids {
                    if !existing.contains(id) {
                        existing.push(id.clone());
                    }
                }
            }
            idx
        } else {
            let idx = data.groups.len();
            self.dedup.insert(dk, idx);
            data.groups.push(DynamicVisGroup { index: 0, kind });
            idx
        };

        let group_index_u16 = (group_idx + 1) as u16;

        match &data.groups[group_idx].kind {
            DynamicVisKind::Journal { .. } | DynamicVisKind::Global { .. } => {
                data.scripts.insert(key, group_index_u16);
            }
            DynamicVisKind::UniqueObject { linked_ids, .. } => {
                for id in linked_ids {
                    data.unique_objects.insert(id.clone(), group_index_u16);
                }
            }
        }
    }
}

/// Parses override file contents supplied in memory.
///
/// # Errors
///
/// Returns any buffered-read error encountered while parsing the provided byte slices.
pub fn parse_overrides_texts(texts: &[impl AsRef<[u8]>]) -> io::Result<StaticOverrides> {
    let mut builder = OverridesBuilder::new();
    for text in texts {
        builder.add_override_text(text.as_ref())?;
    }
    Ok(builder.finish())
}

/// Produces a deduplication key for a [`DynamicVisKind`].
///
/// Journal groups are keyed by `(1, journal_id, ranges)`, global groups by
/// `(2, global_id, ranges)`, and unique-object groups by `(3, source_id, [(1,2)])`.
/// The fixed range `[(1, 2)]` means unique-object groups with the same source ID are always
/// merged regardless of how many linked IDs are listed.
fn dedup_key(kind: &DynamicVisKind) -> DedupKey {
    match kind {
        DynamicVisKind::Journal { journal_id, ranges, .. } => (1, journal_id.clone(), ranges.to_vec()),
        DynamicVisKind::Global { global_id, ranges, .. } => (2, global_id.clone(), ranges.to_vec()),
        DynamicVisKind::UniqueObject { source_id, .. } => (3, source_id.clone(), vec![(1, 2)]),
    }
}

#[cfg(test)]
mod tests;
