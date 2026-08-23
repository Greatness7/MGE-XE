//! Discovers MWSE-convention `-metadata.toml` files and parses distant-land directives.
//!
//! Discovered plugin metadata is fail-soft; explicitly configured TOML sources are strict.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use smallvec::SmallVec;
use uncased::Uncased;

use crate::mge_xe::distant_statics::StaticType;
use crate::vfs::normalize_mesh_override_key;
use distantland_foundation::identity::FileIdentity;
use tracing::{info, warn};

use super::overrides::{DynamicVisKind, OverridesBuilder, StaticOverride};

/// Maximum dynamic-visibility ranges per group, fixed by the `usage.data` format.
const MAX_RANGES: usize = 8;

/// Returns the plugin-adjacent `-metadata.toml` path.
pub fn plugin_metadata_path(plugin: &Path) -> PathBuf {
    let stem = plugin.file_stem().unwrap_or(plugin.as_os_str());
    plugin.with_file_name(format!("{}-metadata.toml", stem.to_string_lossy()))
}

/// Returns existing metadata files in plugin order.
pub fn discover_plugin_metadata(plugins: &[PathBuf]) -> Vec<PathBuf> {
    plugins
        .iter()
        .map(|plugin| plugin_metadata_path(plugin))
        .filter(|path| path.is_file())
        .collect()
}

/// Reads and applies plugin metadata, retaining each readable file's identity.
pub fn apply_plugin_metadata_with_identity(path: &Path, builder: &mut OverridesBuilder) -> Option<FileIdentity> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => {
            warn!("Skipping unreadable metadata file {}: {err}", path.display());
            return None;
        }
    };
    let identity = FileIdentity::from_bytes(path, &bytes);
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) => {
            warn!("Skipping unreadable metadata file {}: {err}", path.display());
            return Some(identity);
        }
    };
    match parse_distantland_section(&text) {
        Ok(Some(metadata)) => {
            info!("Applying plugin metadata: {}", path.display());
            builder.begin_source(path.display().to_string());
            metadata.apply(builder);
        }
        Ok(None) => {}
        Err(err) => warn!("Skipping invalid metadata file {}: {err}", path.display()),
    }
    Some(identity)
}

/// Parses one configured override source and returns the identity of the bytes supplied to it.
///
/// Files with a `.toml` extension use the `[tools.mge-xe.distantland]` schema. All other
/// extensions use the legacy override-list parser, retaining support for `.ovr` and `.txt`.
/// Unlike automatically discovered plugin metadata, configured TOML sources are strict.
///
/// # Errors
///
/// Returns an error when the file cannot be read, is not UTF-8 TOML, contains malformed TOML,
/// or does not contain a `[tools.mge-xe.distantland]` table.
pub fn apply_override_source_with_identity(path: &Path, builder: &mut OverridesBuilder) -> io::Result<FileIdentity> {
    let is_toml = path
        .extension()
        .is_some_and(|extension| extension.to_string_lossy().eq_ignore_ascii_case("toml"));
    if !is_toml {
        return builder.add_override_file_with_identity(path);
    }

    let bytes = fs::read(path)?;
    let identity = FileIdentity::from_bytes(path, &bytes);
    let text = String::from_utf8(bytes).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("configured TOML override {} is not UTF-8: {err}", path.display()),
        )
    })?;
    let metadata = parse_distantland_section(&text)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {err}", path.display())))?
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "configured TOML override {} has no [tools.mge-xe.distantland] table",
                    path.display()
                ),
            )
        })?;

    builder.begin_source(path.display().to_string());
    metadata.apply(builder);
    Ok(identity)
}

/// Extracts and deserializes the `[tools.mge-xe.distantland]` section when present.
///
/// Sections owned by other tools are ignored via serde's unknown-field tolerance.
///
/// # Errors
///
/// Returns a TOML error when the document has a syntax error or the section (or its
/// enclosing tables) has the wrong shape.
fn parse_distantland_section(text: &str) -> Result<Option<DistantLandMetadata>, toml::de::Error> {
    // Windows tooling commonly writes a UTF-8 BOM, which the TOML parser rejects.
    let text = text.trim_start_matches('\u{feff}');
    let document: MetadataDocument = toml::from_str(text)?;
    Ok(document.tools.mge_xe.distantland)
}

/// Top-level metadata document shape; other sections are ignored.
#[derive(Default, Deserialize)]
#[serde(default)]
struct MetadataDocument {
    /// The shared `[tools]` namespace.
    tools: ToolsSection,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ToolsSection {
    /// The `[tools.mge-xe]` table reserved for MGE XE.
    #[serde(rename = "mge-xe")]
    mge_xe: MgeXeSection,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct MgeXeSection {
    distantland: Option<DistantLandMetadata>,
}

/// Parsed `[tools.mge-xe.distantland]` directives.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DistantLandMetadata {
    /// Object IDs whose references are forced into distant-land generation.
    include_objects: Vec<String>,
    /// Object IDs whose references are excluded from distant-land generation.
    exclude_objects: Vec<String>,
    /// Interior cells included in generation (treated like exteriors).
    include_interiors: Vec<String>,
    /// Interior cells excluded from generation.
    exclude_interiors: Vec<String>,
    /// Per-mesh static classification entries keyed by VFS-relative mesh path.
    statics: BTreeMap<String, StaticEntry>,
    /// Dynamic-visibility group declarations.
    dynamic_visibility: Vec<DynamicVisibilityEntry>,
}

impl DistantLandMetadata {
    /// Merges this section's directives into `builder`.
    ///
    /// The caller must have attributed the builder to this file via `begin_source`.
    fn apply(&self, builder: &mut OverridesBuilder) {
        let source = builder.current_source_label().to_owned();

        for (key, entry) in &self.statics {
            let normalized = normalize_mesh_override_key(key).into_owned();
            if normalized.is_empty() {
                continue;
            }
            builder.insert_mesh_override(normalized, entry.to_override(key, &source));
        }

        for (id, enabled) in include_exclude_directives(&self.include_objects, &self.exclude_objects, &source, "object") {
            builder.insert_name(id.to_ascii_lowercase(), enabled);
        }

        for (cell, enabled) in
            include_exclude_directives(&self.include_interiors, &self.exclude_interiors, &source, "interior")
        {
            builder.insert_interior(Uncased::from(cell.to_owned()), enabled);
        }

        for entry in &self.dynamic_visibility {
            if let Some((key, kind)) = entry.to_group(&source) {
                builder.insert_dynamic_vis(key, kind);
            }
        }
    }
}

/// Yields each non-empty include/exclude directive as `(entry, enabled)`, includes first.
///
/// Excludes are emitted last so a conflicting entry ends up disabled, and each conflict warns as it
/// is yielded. `kind` is the singular noun; the TOML keys it reports are `include_{kind}s` and
/// `exclude_{kind}s`.
fn include_exclude_directives<'a>(
    includes: &'a [String],
    excludes: &'a [String],
    source: &'a str,
    kind: &'a str,
) -> impl Iterator<Item = (&'a str, bool)> {
    let included = includes
        .iter()
        .filter(|entry| !entry.is_empty())
        .map(|entry| (entry.as_str(), true));
    let excluded = excludes.iter().filter(|entry| !entry.is_empty()).map(move |entry| {
        if includes.iter().any(|inc| inc.eq_ignore_ascii_case(entry)) {
            warn!("{source}: {kind} '{entry}' is listed in both include_{kind}s and exclude_{kind}s; excluding");
        }
        (entry.as_str(), false)
    });
    included.chain(excluded)
}

/// One per-mesh static classification entry.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct StaticEntry {
    /// Explicit static classification.
    #[serde(rename = "type")]
    static_type: Option<StaticTypeName>,
    /// Excludes the mesh from distant-land generation.
    ignore: bool,
    /// Grass density percentage (0–100); meaningful with `type = "grass"`.
    grass_density: Option<i64>,
    /// Mesh simplification percentage (0–100); `0` disables simplification.
    reduction: Option<i64>,
    /// Classifies the mesh as if its object had no script.
    ignore_script: bool,
}

impl StaticEntry {
    /// Converts to a [`StaticOverride`], clamping percentages with a warning.
    fn to_override(&self, mesh: &str, source: &str) -> StaticOverride {
        let mut result = StaticOverride {
            ignore: self.ignore,
            no_script: self.ignore_script,
            ..StaticOverride::default()
        };
        if let Some(static_type) = self.static_type {
            result.static_type = static_type.into();
        }
        if let Some(pct) = self.grass_density {
            if result.static_type != StaticType::StaticGrass {
                warn!("{source}: '{mesh}' sets grass_density without type = \"grass\"");
            }
            result.density = clamp_percentage(pct, "grass_density", mesh, source) / 100.0;
        }
        if let Some(pct) = self.reduction {
            result.simplify = Some(clamp_percentage(pct, "reduction", mesh, source) / 100.0);
        }
        result
    }
}

/// Clamps a percentage field to `0..=100`, warning when the value was out of range.
fn clamp_percentage(value: i64, field: &str, mesh: &str, source: &str) -> f32 {
    if !(0..=100).contains(&value) {
        warn!("{source}: '{mesh}' {field} = {value} is out of range; clamping to 0-100");
    }
    value.clamp(0, 100) as f32
}

/// Static classification names accepted by the `type` field.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StaticTypeName {
    /// Infer the class at runtime.
    Auto,
    /// Draw close to the player.
    Near,
    /// Draw at medium distance.
    Far,
    /// Draw at the farthest distance bucket.
    VeryFar,
    /// Treat the mesh as grass.
    Grass,
    /// Treat the mesh as a tree.
    Tree,
    /// Treat the mesh as a building.
    Building,
}

impl From<StaticTypeName> for StaticType {
    fn from(value: StaticTypeName) -> Self {
        match value {
            StaticTypeName::Auto => StaticType::StaticAuto,
            StaticTypeName::Near => StaticType::StaticNear,
            StaticTypeName::Far => StaticType::StaticFar,
            StaticTypeName::VeryFar => StaticType::StaticVeryFar,
            StaticTypeName::Grass => StaticType::StaticGrass,
            StaticTypeName::Tree => StaticType::StaticTree,
            StaticTypeName::Building => StaticType::StaticBuilding,
        }
    }
}

/// One dynamic-visibility group declaration.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum DynamicVisibilityEntry {
    /// Visibility gated by a journal index range.
    Journal {
        /// Script carried by the objects that join this group.
        script: String,
        /// Journal ID whose index is tested against `ranges`.
        journal: String,
        /// Inclusive `[lo, hi]` index ranges that enable the group.
        #[serde(default)]
        ranges: Vec<[i32; 2]>,
    },
    /// Visibility gated by a global-variable range.
    Global {
        /// Script carried by the objects that join this group.
        script: String,
        /// Global variable whose value is tested against `ranges`.
        global: String,
        /// Inclusive `[lo, hi]` value ranges that enable the group.
        #[serde(default)]
        ranges: Vec<[i32; 2]>,
    },
    /// Visibility shared with a controlling unique object.
    UniqueObject {
        /// Controlling object ID; implicitly linked to the group.
        object: String,
        /// Additional object IDs that share the controlling object's visibility.
        #[serde(default)]
        linked_objects: Vec<String>,
    },
}

impl DynamicVisibilityEntry {
    /// Converts to the override key and [`DynamicVisKind`] used by the merge builder.
    ///
    /// Returns `None` (with a warning) when the entry's identifying field is empty.
    fn to_group(&self, source: &str) -> Option<(String, DynamicVisKind)> {
        match self {
            Self::Journal { script, journal, ranges } => {
                if script.is_empty() || journal.is_empty() {
                    warn!("{source}: skipping dynamic_visibility journal entry with an empty script or journal");
                    return None;
                }
                let kind = DynamicVisKind::Journal {
                    journal_id: journal.to_ascii_lowercase(),
                    ranges: convert_ranges(ranges, source),
                };
                Some((script.to_ascii_lowercase(), kind))
            }
            Self::Global { script, global, ranges } => {
                if script.is_empty() || global.is_empty() {
                    warn!("{source}: skipping dynamic_visibility global entry with an empty script or global");
                    return None;
                }
                let kind = DynamicVisKind::Global {
                    global_id: global.to_ascii_lowercase(),
                    ranges: convert_ranges(ranges, source),
                };
                Some((script.to_ascii_lowercase(), kind))
            }
            Self::UniqueObject { object, linked_objects } => {
                if object.is_empty() {
                    warn!("{source}: skipping dynamic_visibility unique_object entry with an empty object");
                    return None;
                }
                let object = object.to_ascii_lowercase();
                let mut linked_ids = Vec::with_capacity(linked_objects.len() + 1);
                linked_ids.push(object.clone());
                linked_ids.extend(linked_objects.iter().map(|id| id.to_ascii_lowercase()));
                let kind = DynamicVisKind::UniqueObject {
                    source_id: object.clone(),
                    linked_ids,
                };
                Some((object, kind))
            }
        }
    }
}

/// Converts inclusive `[lo, hi]` pairs into the runtime's half-open tuple form, enforcing the
/// format's maximum of [`MAX_RANGES`] ranges per group.
fn convert_ranges(ranges: &[[i32; 2]], source: &str) -> SmallVec<[(i32, i32); 8]> {
    if ranges.len() > MAX_RANGES {
        warn!(
            "{source}: dynamic_visibility group has {} ranges; only the first {MAX_RANGES} are used",
            ranges.len()
        );
    }
    ranges
        .iter()
        .take(MAX_RANGES)
        .filter_map(|&[lo, hi]| {
            let Some(end_exclusive) = hi.checked_add(1) else {
                warn!("{source}: dynamic_visibility range [{lo}, {hi}] exceeds the supported integer range");
                return None;
            };
            Some((lo, end_exclusive))
        })
        .collect()
}

#[cfg(test)]
mod tests;
