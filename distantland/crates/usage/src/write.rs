//! Serialization of MGE-XE `usage.data`.

use anyhow::Context as _;
use bytes_io::Writer;

use crate::info::UsageInfo;
use crate::{DynamicVisData, DynamicVisKind, PackedDistantStatics};

/// An ordered view of the final static-record keys, sufficient to resolve usage ordinals.
///
/// `serialize_usage_data` reads its static map only for the record count and each reference's mesh
/// ordinal, so this exposes exactly `len` and `get_index_of`. Both the full packed map and the
/// owner-partial assembly can build this view, keeping usage serialization identical across build
/// modes. The key order must match the serialized shard-major/key-byte record order so ordinals
/// align with the `static_meshes` shards.
#[derive(Debug, Default)]
pub struct StaticOrdinalView {
    keys: crate::IndexSet<String>,
}

impl StaticOrdinalView {
    /// Builds a view from an ordered sequence of rendered record keys.
    pub fn from_ordered_keys(keys: impl IntoIterator<Item = String>) -> Self {
        Self {
            keys: keys.into_iter().collect(),
        }
    }

    /// Builds a view over a finalized packed static map, preserving its record order.
    pub fn from_packed(distant_statics: &PackedDistantStatics) -> Self {
        Self::from_ordered_keys(distant_statics.keys().cloned())
    }

    /// Number of static records (the `usage.data` mesh count).
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Whether the view contains no records.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Returns the ordinal of `key`, or `None` when it is absent (a dangling reference).
    pub fn get_index_of(&self, key: &str) -> Option<usize> {
        self.keys.get_index_of(key)
    }
}

/// Serializes `UsageInfo` and distant-static references into MGE-XE `usage.data` bytes.
///
/// `ordinals` supplies the mesh count and per-reference ordinals; see [`StaticOrdinalView`].
///
/// # Errors
///
/// Returns an error if serialization into the in-memory writer fails.
#[tracing::instrument(skip_all)]
pub fn serialize_usage_data<'a>(
    usage_info: &UsageInfo<'a>,
    ordinals: &StaticOrdinalView,
    dynamic_vis: &DynamicVisData,
    min_static_size: f32,
) -> anyhow::Result<Vec<u8>> {
    let mut writer = Writer::new(Vec::with_capacity(usage_data_capacity_hint(usage_info, dynamic_vis)));

    let num_meshes = ordinals.len();
    writer.save_as::<u32>(num_meshes)?;

    let num_dynamic_vis_groups = dynamic_vis.groups.len() as u32;
    writer.save(&num_dynamic_vis_groups)?;
    write_dynamic_vis_groups(&mut writer, dynamic_vis)?;

    let position = writer.cursor.position();
    writer.save(&0u32)?; // Placeholder, back-patched below with the exterior reference count.

    if let Some(exterior_references) = usage_info.exterior_references() {
        let mut references_count = 0u32;
        for reference in exterior_references.values() {
            references_count += write_reference(&mut writer, ordinals, reference)?;
        }
        if references_count != 0 {
            let end_position = writer.cursor.position();
            writer.cursor.set_position(position);
            writer.save(&references_count)?; // Back-patch the placeholder.
            writer.cursor.set_position(end_position);
        }
    }

    // Interior worldspaces, each keyed by cell name.
    for (cell_name, references) in usage_info.interior_cells() {
        let position = writer.cursor.position();
        writer.save(&0u32)?; // Placeholder, back-patched with the interior reference count.
        writer.save(&[0u8; 64])?; // Placeholder for the cell name, a fixed 64-byte field.

        let mut reference_count = 0u32;

        for reference in references.values() {
            reference_count += write_reference(&mut writer, ordinals, reference)?;
        }

        if reference_count != 0 {
            // The field holds the engine's own single-byte name bytes, whatever codepage the
            // install uses. The ESM reader recovers those bytes by decoding as WINDOWS-1252,
            // which is lossless because every high byte maps to a distinct codepoint, so
            // encoding back the same way restores them. Writing the UTF-8 form instead both
            // widens non-ASCII names past the field and yields bytes the game never matches.
            //
            // `encode` truncates at an embedded NUL, which would silently shorten the name
            // and could merge two world spaces, so reject one rather than let it through.
            anyhow::ensure!(
                !cell_name.contains('\0'),
                "interior cell name '{}' contains a NUL and cannot be written to the 64-byte usage.data field",
                cell_name.escape_debug()
            );
            let encoded_name = writer
                .encode(cell_name)
                .with_context(|| format!("interior cell name '{cell_name}' is not representable as engine name bytes"))?;
            anyhow::ensure!(
                encoded_name.len() <= 64,
                "interior cell name '{cell_name}' encodes to {} bytes, exceeding the 64-byte usage.data field",
                encoded_name.len()
            );

            let end_position = writer.cursor.position();
            writer.cursor.set_position(position);
            writer.save(&reference_count)?;
            writer.save_bytes_padded::<64>(&encoded_name)?;
            writer.cursor.set_position(end_position);
        } else {
            // Empty interior groups are omitted entirely to match MGE-XE's layout.
            writer.cursor.get_mut().truncate(position as usize);
            writer.cursor.set_position(position);
        }
    }

    writer.save(&0u32)?; // Terminates exterior/interior sections.
    writer.save(&min_static_size)?;
    Ok(writer.cursor.into_inner())
}

/// Estimates the pre-allocation size for the serialized `usage.data` buffer.
///
/// Each dynamic-vis group is 130 bytes; each reference entry is 36 bytes;
/// interior cells carry an extra 68-byte header (4-byte count + 64-byte name).
fn usage_data_capacity_hint(usage_info: &UsageInfo<'_>, dynamic_vis: &DynamicVisData) -> usize {
    const HEADER_BYTES: usize = 16;
    const DYNAMIC_VIS_GROUP_BYTES: usize = 130;
    const INTERIOR_CELL_HEADER_BYTES: usize = 68;
    const REFERENCE_BYTES: usize = 36;

    let exterior_references = usage_info.exterior_references().map_or(0, |references| references.len());
    let mut interior_cell_count = 0;
    let mut interior_references = 0;
    for (_, references) in usage_info.interior_cells() {
        interior_cell_count += 1;
        interior_references += references.len();
    }

    HEADER_BYTES
        + dynamic_vis.groups.len() * DYNAMIC_VIS_GROUP_BYTES
        + interior_cell_count * INTERIOR_CELL_HEADER_BYTES
        + (exterior_references + interior_references) * REFERENCE_BYTES
}

/// Writes one reference entry: mesh index, vis-group index, and transform. Returns 1.
///
/// Returns 0 without writing anything when `reference.id` is not present in `ordinals`
/// (a dangling reference, e.g. a synthetic merged group whose geometry was empty).
fn write_reference(
    writer: &mut Writer,
    ordinals: &StaticOrdinalView,
    reference: &crate::info::DistantReference<'_>,
) -> std::io::Result<u32> {
    if let Some(mesh_index) = ordinals.get_index_of(reference.id.as_ref()) {
        writer.save_as::<u32>(mesh_index)?;
        writer.save(&reference.vis_index)?;
        writer.save(&0u16)?;
        writer.save(&reference.translation)?;
        writer.save(&reference.rotation)?;
        writer.save(&reference.scale)?;
        Ok(1)
    } else {
        Ok(0)
    }
}

/// Serializes all dynamic visibility groups into the writer.
///
/// Each group is exactly 130 bytes: 1-byte source kind, 64-byte null-padded ID,
/// 1-byte range count, and 8 × 8-byte (begin, end) range pairs.
fn write_dynamic_vis_groups(writer: &mut Writer, dynamic_vis: &DynamicVisData) -> std::io::Result<()> {
    for group in &dynamic_vis.groups {
        let (source, kind, id, ranges): (u8, &str, &str, &[(i32, i32)]) = match &group.kind {
            DynamicVisKind::Journal { journal_id, ranges } => (1, "journal", journal_id, ranges.as_slice()),
            DynamicVisKind::Global { global_id, ranges } => (2, "global", global_id, ranges.as_slice()),
            DynamicVisKind::UniqueObject { source_id, .. } => (3, "unique object", source_id, &[(1, 2)]),
        };

        // As with interior cell names, this field holds engine name bytes rather than UTF-8.
        // Unlike them these ids come from override files, so an embedded NUL is reachable
        // here; `encode` would truncate at it and quietly bind the group to a shorter id.
        if id.contains('\0') {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "dynamic vis {kind} id '{}' contains a NUL and cannot be written to the 64-byte usage.data field",
                    id.escape_debug()
                ),
            ));
        }
        let encoded_id = writer.encode(id).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("dynamic vis {kind} id '{id}' is not representable as engine name bytes"),
            )
        })?;
        if encoded_id.len() > 64 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "dynamic vis {kind} id '{id}' encodes to {} bytes, exceeding the 64-byte usage.data field",
                    encoded_id.len()
                ),
            ));
        }

        let start = writer.cursor.position();
        writer.save(&source)?;
        writer.save_bytes_padded::<64>(&encoded_id)?;
        writer.save(&(ranges.len() as u8))?;
        for i in 0..8 {
            let (begin, end) = ranges.get(i).copied().unwrap_or((0, 0));
            writer.save(&begin)?;
            writer.save(&end)?;
        }
        debug_assert_eq!(writer.cursor.position() - start, 130);
    }

    Ok(())
}

#[cfg(test)]
mod tests;
