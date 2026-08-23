//! MGE-XE `usage.data` binary structures and serializers.

use std::fs;
use std::io;
use std::path::Path;

use bstr::BString;
use bytemuck::NoUninit;
use bytes_io::{Load, LoadSave, Reader, Save, Writer};
use glam::Vec3;

/// Visibility-group data source used by MGE-XE `usage.data`.
#[repr(u8)]
#[derive(LoadSave, NoUninit, Clone, Copy, Default, Debug, PartialEq, Eq)]
pub enum DataSource {
    /// Group visibility is gated by a journal entry.
    #[default]
    Journal = 1,
    /// Group visibility is gated by a global variable.
    Global = 2,
    /// Group visibility is gated by a unique object state.
    UniqueObject = 3,
}

/// One dynamic-visibility group declaration in `usage.data`.
#[derive(Clone, Debug, PartialEq)]
pub struct DynamicVisGroup {
    /// Source that controls the group.
    pub data_source: DataSource,
    /// Source identifier, stored as a 64-byte padded string.
    pub id: BString,
    /// Half-open enabled ranges for journal/global data sources.
    pub enabled_ranges: Vec<[i32; 2]>,
}

/// One distant reference entry stored in `usage.data`.
#[derive(LoadSave, Clone, Debug, PartialEq)]
pub struct UsageDataReference {
    /// Index into the shard-major logical concatenation of this build's static-mesh shards.
    pub id: u32,
    /// Dynamic-visibility group index, or zero for always-visible statics.
    pub vis_index: u16,
    /// Padding to 4-byte-align the transform that follows on disk.
    pub _padding: u16,
    /// World-space translation.
    pub location: Vec3,
    /// World-space Euler rotation.
    pub rotation: Vec3,
    /// Uniform reference scale.
    pub scale: f32,
}

/// One interior cell section in `usage.data`.
#[derive(Clone, Debug, PartialEq)]
pub struct Interior {
    /// Interior cell name stored as a padded 64-byte string.
    pub name: BString,
    /// References that belong to the interior.
    pub references: Vec<UsageDataReference>,
}

/// Top-level MGE-XE `usage.data` file container.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageData {
    /// Total static mesh count written at the start of the file.
    pub num_meshes: u32,
    /// Dynamic-visibility group table.
    pub vis_groups: Vec<DynamicVisGroup>,
    /// Exterior references.
    pub exterior: Vec<UsageDataReference>,
    /// Interior references grouped by cell.
    pub interiors: Vec<Interior>,
    /// Minimum static size recorded in the file trailer.
    pub min_static_size: f32,
}

impl Load for DynamicVisGroup {
    fn load(stream: &mut Reader<'_>) -> io::Result<Self> {
        let data_source = stream.load()?;
        let id = stream.load_bytes_padded(64)?.into();
        let num_enabled_ranges: u8 = stream.load()?;
        if num_enabled_ranges > 8 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("usage.data dynamic visibility group has {num_enabled_ranges} enabled ranges, maximum is 8"),
            ));
        }
        let enabled_ranges = stream.load_seq(num_enabled_ranges)?;
        let padding_amount = 8 * (8 - num_enabled_ranges) as u32;
        stream.skip(padding_amount)?;
        Ok(Self {
            data_source,
            id,
            enabled_ranges,
        })
    }
}

impl Save for DynamicVisGroup {
    fn save(&self, stream: &mut Writer) -> io::Result<()> {
        stream.save(&self.data_source)?;
        stream.save_bytes_padded::<64>(self.id.as_ref())?;
        stream.save(&(self.enabled_ranges.len() as u8))?;
        stream.save_seq(&self.enabled_ranges)?;
        for _ in self.enabled_ranges.len()..8 {
            stream.save(&0i64)?;
        }
        Ok(())
    }
}

impl Load for UsageData {
    fn load(stream: &mut Reader<'_>) -> io::Result<Self> {
        let num_meshes = stream.load()?;
        let vis_groups = stream.load()?;
        let exterior = stream.load()?;
        let mut interiors = vec![];
        loop {
            let num_references: u32 = stream.load()?;
            if num_references == 0 {
                break;
            }
            let name = stream.load_bytes_padded(64)?.into();
            let references = stream.load_seq(num_references)?;
            interiors.push(Interior { name, references });
        }
        let min_static_size = stream.load()?;
        Ok(Self {
            num_meshes,
            vis_groups,
            exterior,
            interiors,
            min_static_size,
        })
    }
}

/// Deserializes a complete MGE-XE `usage.data` payload.
///
/// # Errors
///
/// Returns an error if the payload is malformed, truncated, or contains trailing bytes.
pub fn deserialize_usage_data(bytes: &[u8]) -> io::Result<UsageData> {
    let mut reader = Reader::new(bytes);
    let usage_data = reader.load()?;
    let position = usize::try_from(reader.cursor.position())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "usage.data reader position exceeds usize"))?;
    if position != bytes.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("usage.data contains {} unexpected trailing bytes", bytes.len() - position),
        ));
    }
    Ok(usage_data)
}

/// Loads and deserializes an MGE-XE `usage.data` file.
///
/// # Errors
///
/// Returns an I/O error if the file cannot be read or if its contents fail
/// [`deserialize_usage_data`] validation.
pub fn load_usage_data(path: &Path) -> io::Result<UsageData> {
    let file = fs::File::open(path)?;
    // SAFETY: The mapping is read-only and remains alive for the complete decode.
    let bytes = unsafe { memmap2::MmapOptions::new().map(&file)? };
    deserialize_usage_data(&bytes)
}

#[cfg(test)]
mod tests;
