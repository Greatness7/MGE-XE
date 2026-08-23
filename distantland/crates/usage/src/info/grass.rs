use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use bytes_io::Reader;
use tes3::esp::{Cell, Plugin, Static, TES3Object};

use super::filter::grass_density_for_object;
use super::*;
use crate::vfs::{make_normalized, normalize};
use distantland_foundation::identity::FileIdentity;

/// Grass placements a plugin must exceed before it is suggested as groundcover.
///
/// Bulk placement is what distinguishes a groundcover plugin from an ordinary mod that plants a
/// few decorative grass meshes. Real groundcover passes this within its first cell or two.
const GRASS_PLUGIN_INSTANCE_THRESHOLD: u64 = 50;

/// Classifies `path` as a dedicated groundcover plugin for GUI suggestions.
///
/// Meshes under the conventional `grass\` directory count as grass. Generation remains
/// authoritative: it additionally applies VFS resolution and the job's normal static overrides,
/// so it can disagree with this answer.
///
/// Only `STAT` and `CELL` are decoded, matching the record set OpenMW accepts from a groundcover
/// file, and they are decoded one record at a time. That matters because the GUI runs this across
/// a whole install in parallel: a plugin defining a single `grass\` mesh passes Gate A, and
/// landmass mods that do so carry enormous `CELL` payloads. A cell's compact on-disk references
/// expand several-fold once materialized, so holding a whole decoded plugin per worker is what
/// exhausted memory on a large install.
///
/// Gate B stops at the first reference past the threshold, so a plugin whose records are damaged
/// only after that point now classifies as groundcover where eager parsing reported a failure.
/// That is the right trade for a suggestion the user reviews anyway.
///
/// # Errors
///
/// Returns an error if `path` cannot be read or parsed as a TES3 plugin.
pub fn is_grass_plugin(path: &Path) -> Result<bool> {
    let Ok(bytes) = std::fs::read(path) else {
        bail!("Failed to read grass plugin {}", path.display());
    };
    let (stat_ranges, cell_ranges) = grass_record_ranges(&bytes);

    // Gate A: the plugin must define grass statics at all. Statics are small, and the set is the
    // only thing that outlives this loop.
    let mut grass_object_ids: HashSet<UString> = HashSet::new();
    for range in stat_ranges {
        let Ok(TES3Object::Static(object)) = Reader::new(&bytes[range]).load::<TES3Object>() else {
            bail!("Failed to parse grass plugin {}", path.display());
        };
        if normalize(&object.mesh).starts_with("grass\\") {
            grass_object_ids.insert(UString::new(object.id));
        }
    }
    if grass_object_ids.is_empty() {
        return Ok(false);
    }

    // Gate B: it must place them in bulk. Each cell is dropped before the next is decoded, so peak
    // cost is the file plus one cell rather than the whole decoded plugin.
    //
    // Deliberately coarser than `load_grass_plugins`, which resolves master-addressed and deleted
    // references across the whole grass list. This sees one file with no list context, so it counts
    // only unambiguous new placements. That is enough to tell groundcover apart from an ordinary mod.
    let mut count: u64 = 0;
    for range in cell_ranges {
        let Ok(TES3Object::Cell(cell)) = Reader::new(&bytes[range]).load::<TES3Object>() else {
            bail!("Failed to parse grass plugin {}", path.display());
        };
        if cell.is_interior() {
            continue;
        }
        for ((mast_index, _), reference) in &cell.references {
            if *mast_index != 0 || reference.deleted.is_some() {
                continue;
            }
            if grass_object_ids.contains(reference.id.as_uncased()) {
                count += 1;
                if count > GRASS_PLUGIN_INSTANCE_THRESHOLD {
                    return Ok(true);
                }
            }
        }
    }

    Ok(false)
}

/// Walks a plugin's record framing, yielding each record's tag and full extent without decoding it.
///
/// Uses the same framing as `Plugin::load_bytes_filtered`: every record is an 8-byte `(tag, len)`
/// prefix followed by a further `len + 8` bytes. Walking it here instead of calling
/// `load_bytes_filtered` is what lets callers decode one record at a time, or none at all.
///
/// Unlike that walk, an invalid length or truncated extent ends this one rather than resuming from
/// just past the prefix, which would risk reading payload bytes as the next header. The records
/// found before the damage stay usable.
fn record_headers(bytes: &[u8]) -> impl Iterator<Item = ([u8; 4], Range<usize>)> + '_ {
    let mut stream = Reader::new(bytes);
    std::iter::from_fn(move || {
        let (tag, len) = stream.load::<([u8; 4], u32)>().ok()?;
        let start = stream.cursor.position() - 8;
        let remainder = len.checked_add(8)?;
        let end = stream.skip(remainder).ok()?;
        // Both positions are bounded by `bytes.len()`, which is already a `usize`.
        Some((tag, start as usize..end as usize))
    })
}

/// Locates a plugin's `STAT` and `CELL` records without decoding any of them.
fn grass_record_ranges(bytes: &[u8]) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let mut stat_ranges = Vec::new();
    let mut cell_ranges = Vec::new();

    for (tag, range) in record_headers(bytes) {
        match &tag {
            b"STAT" => stat_ranges.push(range),
            b"CELL" => cell_ranges.push(range),
            _ => {}
        }
    }

    (stat_ranges, cell_ranges)
}

/// Classifies a whole plugin list in parallel, one `bool` per input path.
///
/// This is [`is_grass_plugin`] applied across a list, and it lives here rather than in the caller so
/// that the parallelism stays next to the parser it exists for: the cost is dominated by reading and
/// decoding each file, and a GUI scanning an install cannot afford to do that serially.
///
/// A plugin that cannot be read or parsed classifies as `false`. The result drives a suggestion, so
/// there is nothing for a caller to do with a per-file error that it would not do with a `false`.
pub fn classify_grass_plugins(paths: &[PathBuf]) -> Vec<bool> {
    paths.par_iter().map(|path| is_grass_plugin(path).unwrap_or(false)).collect()
}

pub(crate) struct GrassPluginLoad<'a> {
    pub(crate) usage_info: UsageInfo<'a>,
    pub(crate) identities: Vec<FileIdentity>,
    pub(crate) warnings: Vec<UsageWarning>,
}

struct GrassCandidate {
    object_id: String,
    cell: (i32, i32),
    refr_index: u32,
    /// Source that owns this placement's identity: the plugin that first defined it, which is the
    /// resolved master for a master-addressed reference and the placing plugin otherwise.
    source: SourceId,
    translation: [f32; 3],
    rotation: [f32; 3],
    scale: f32,
}

type GrassCandidateSortKey<'a> = ((i32, i32), [u32; 3], [u32; 3], u32, &'a str, u32);

/// Identity of one grass placement while the ordered grass list is being resolved.
///
/// Scoped per cell because groundcover generators restart `refr_index` at 0 in every cell, so a
/// plugin-global refnum would collide. Both reference implementations key the same way: MGE-XE's
/// legacy generator put cell coordinates in the key string, and OpenMW rebuilds its `refs` map for
/// each cell in `Groundcover::collectInstances`.
type GrassRefKey = ((i32, i32), SourceId, u32);

/// One grass plugin after parsing, held while definitions are unioned across the whole list.
struct LoadedGrassPlugin {
    source: SourceId,
    /// Lowercased filename, used to resolve other plugins' `MAST` entries to this one.
    filename: String,
    masters: Vec<String>,
    plugin: Plugin,
    grass_object_ids: HashSet<UString>,
}

pub(super) fn load_grass_plugins<'a>(
    vfs: &'a Vfs,
    paths: &[PathBuf],
    args: &UsageFilterOptions,
    overrides: &StaticOverrides,
    reference_sources: &ReferenceSources,
) -> Result<GrassPluginLoad<'a>> {
    let mut usage_info = UsageInfo {
        reference_sources: reference_sources.clone(),
        ..UsageInfo::default()
    };
    let mut identities = Vec::with_capacity(paths.len());
    let mut warnings = Vec::new();

    // Pass 1: parse every plugin and classify its statics. Job order is the load order; unlike the
    // main list it is not sorted, because a later grass plugin may override an earlier one.
    let mut loaded = Vec::with_capacity(paths.len());
    for path in paths {
        let bytes = std::fs::read(path).with_context(|| format!("Failed to read grass plugin {}", path.display()))?;
        identities.push(FileIdentity::from_bytes(path, &bytes));
        let plugin =
            parse_grass_plugin(&bytes).with_context(|| format!("Failed to parse grass plugin {}", path.display()))?;
        let grass_object_ids = classify_loaded_plugin(&plugin, |mesh| {
            let normalized = normalize(mesh);
            vfs.resolve_model_mesh_key(&normalized).is_some_and(|mesh_key| {
                mesh_key.starts_with("grass\\")
                    || overrides
                        .mesh_overrides
                        .get(mesh_key)
                        .is_some_and(|mesh_override| mesh_override.static_type == StaticType::StaticGrass)
            })
        });

        let source = reference_sources
            .source_id(path)
            .with_context(|| format!("Grass plugin source was not interned: {}", path.display()))?;
        let masters = plugin.header().map_or_else(Vec::new, |header| {
            header.masters.iter().map(|(name, _)| name.to_ascii_lowercase()).collect()
        });
        loaded.push(LoadedGrassPlugin {
            source,
            filename: plugin_filename(path),
            masters,
            plugin,
            grass_object_ids,
        });
    }

    // Pass 2: union the grass static definitions across the whole list, so an addon plugin can place
    // references to grass defined by another. Later plugins win, matching ordinary override rules.
    let mut definitions: HashMap<String, (SourceId, ObjectDefinition<'a>)> = HashMap::new();
    for entry in &mut loaded {
        for object in entry.plugin.objects.iter_mut() {
            let TES3Object::Static(Static { id, mesh, .. }) = object else {
                continue;
            };
            // Definitions are keyed by lowercased object id, matching `UsageInfo::objects`.
            id.make_ascii_lowercase();
            if !entry.grass_object_ids.contains(id.as_uncased()) {
                continue;
            }
            make_normalized(mesh);
            let Some(mesh_key) = vfs.resolve_model_mesh_key(mesh) else {
                continue;
            };
            // Grass STATs carry no script, so they are never script-classified: the empty
            // `script_id` makes `classify_script_disables` a no-op for them.
            let Some(definition) = UsageInfo::build_object_definition(id, "", mesh_key, ObjectKind::Static, args, overrides)
            else {
                continue;
            };
            if grass_density_for_object(&definition, args, overrides).is_some() {
                definitions.insert(id.clone(), (entry.source, definition));
            }
        }
    }

    // Pass 3: resolve placements across the ordered list. Every exterior reference participates,
    // grass-classified or not, so that a later plugin's override or delete lands on its target; the
    // grass filter is applied afterwards, as OpenMW does when it looks up the groundcover model.
    let mut resolved: BTreeMap<GrassRefKey, GrassCandidate> = BTreeMap::new();
    let mut source_by_filename: HashMap<String, SourceId> = HashMap::new();
    let mut unresolved_master_counts: Vec<(String, u64)> = Vec::new();
    for entry in loaded {
        let mut unresolved_masters = 0_u64;
        for cell in entry.plugin.into_objects_of_type::<Cell>() {
            if cell.is_interior() {
                continue;
            }
            for ((mast_index, refr_index), reference) in cell.references {
                // A master outside the grass list (`Morrowind.esm`, typically) cannot be addressed
                // here, so the placement falls back to belonging to this plugin. OpenMW resolves the
                // same way: `resolveParentFileIndices` leaves the index pointing at the reader
                // itself when the master is absent from the groundcover file list.
                let source = if mast_index == 0 {
                    entry.source
                } else {
                    let resolved_master = entry
                        .masters
                        .get(mast_index as usize - 1)
                        .and_then(|name| source_by_filename.get(name))
                        .copied();
                    if resolved_master.is_none() {
                        unresolved_masters += 1;
                    }
                    resolved_master.unwrap_or(entry.source)
                };

                let key = (cell.data.grid, source, refr_index);
                if reference.deleted.is_some() {
                    resolved.remove(&key);
                    continue;
                }
                resolved.insert(
                    key,
                    GrassCandidate {
                        object_id: reference.id.to_ascii_lowercase(),
                        cell: cell.data.grid,
                        refr_index,
                        source,
                        translation: reference.translation,
                        rotation: reference.rotation,
                        scale: reference.scale.unwrap_or(1.0),
                    },
                );
            }
        }
        if unresolved_masters != 0 {
            unresolved_master_counts.push((entry.filename.clone(), unresolved_masters));
        }
        // Only plugins already processed can be addressed as masters, mirroring OpenMW's backwards
        // search over readers with a lower index than the one being resolved.
        source_by_filename.insert(entry.filename, entry.source);
    }
    warnings.extend(report_unresolved_masters(&unresolved_master_counts));

    // Pass 4: keep the grass placements, thin by density, and emit.
    let mut candidates: Vec<GrassCandidate> = resolved
        .into_values()
        .filter(|candidate| definitions.contains_key(&candidate.object_id))
        .collect();
    candidates.sort_unstable_by(|left, right| candidate_sort_key(left).cmp(&candidate_sort_key(right)));

    let mut previous_position = None;
    let mut occurrence_salt = 0_u32;
    for (index, candidate) in candidates.into_iter().enumerate() {
        let position = (candidate.cell, candidate.translation.map(f32::to_bits));
        if previous_position == Some(position) {
            occurrence_salt += 1;
        } else {
            previous_position = Some(position);
            occurrence_salt = 0;
        }

        let (_, definition) = &definitions[&candidate.object_id];
        let source_name = reference_sources
            .name(candidate.source)
            .expect("interned grass plugin source has a filename");
        let density = grass_density_for_object(definition, args, overrides)
            .expect("dedicated grass candidates have grass-classified definitions");
        if density <= 0.0
            || (density < 1.0
                && grass_plugin_density_sample(source_name, candidate.cell, candidate.translation, occurrence_salt)
                    >= density)
        {
            continue;
        }

        // Identity is run-local: nothing downstream addresses a grass placement, so a dense
        // counter is enough to keep the merged exterior map's keys distinct.
        let occurrence = u32::try_from(index + 1).context("Grass plugins have too many references")?;
        usage_info.exterior_references_mut().insert(
            StableRefKey::new(candidate.source, occurrence),
            DistantReference {
                id: Cow::Borrowed(definition.mesh),
                deleted: false,
                // Groundcover files place ordinary temporary references, and grass statics are
                // never `->Disable` targets, so Rule B cannot reach them either way.
                persistent: false,
                translation: candidate.translation.into(),
                rotation: candidate.rotation.into(),
                scale: candidate.scale,
                vis_index: definition.vis_index,
            },
        );
    }

    for (object_id, (source, definition)) in definitions {
        let source_name = reference_sources
            .name(source)
            .expect("interned grass plugin source has a filename");
        usage_info
            .objects
            .insert(format!("\0grass:{source_name}:{object_id}"), definition);
    }

    Ok(GrassPluginLoad {
        usage_info,
        identities,
        warnings,
    })
}

fn parse_grass_plugin(bytes: &[u8]) -> std::io::Result<Plugin> {
    let mut plugin = Plugin::new();
    plugin.load_bytes_filtered(bytes, |tag| matches!(&tag, b"TES3" | b"STAT" | b"CELL"))?;
    Ok(plugin)
}

fn classify_loaded_plugin(plugin: &Plugin, mut is_grass_mesh: impl FnMut(&str) -> bool) -> HashSet<UString> {
    let mut grass_object_ids = HashSet::new();

    for object in &plugin.objects {
        if let TES3Object::Static(object) = object
            && is_grass_mesh(&object.mesh)
        {
            grass_object_ids.insert(UString::new(object.id.clone()));
        }
    }

    grass_object_ids
}

/// Reports references whose `MAST` target is not itself in the grass list.
///
/// Such a reference cannot address what it names, so it is kept as a placement belonging to the
/// plugin that declared it. Grass plugins routinely master `Morrowind.esm` without addressing it;
/// this fires only when a reference actually targets an out-of-list master.
fn report_unresolved_masters(counts: &[(String, u64)]) -> Vec<UsageWarning> {
    counts
        .iter()
        .map(|(filename, count)| UsageWarning {
            code: "grass_plugin_master_not_in_list".to_owned(),
            message: format!(
                "Kept {count} reference(s) in grass plugin {filename} as new placements: they address a \
                 master that is not itself listed under grass_plugins, so the plugin they were meant \
                 to modify could not be found. Add that master to the grass list, before this plugin."
            ),
        })
        .collect()
}

fn candidate_sort_key(candidate: &GrassCandidate) -> GrassCandidateSortKey<'_> {
    (
        candidate.cell,
        candidate.translation.map(f32::to_bits),
        candidate.rotation.map(f32::to_bits),
        candidate.scale.to_bits(),
        &candidate.object_id,
        candidate.refr_index,
    )
}

fn grass_plugin_density_sample(source_name: &str, cell: (i32, i32), translation: [f32; 3], salt: u32) -> f32 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(source_name.as_bytes());
    hasher.update(&cell.0.to_le_bytes());
    hasher.update(&cell.1.to_le_bytes());
    for value in translation {
        hasher.update(&value.to_bits().to_le_bytes());
    }
    hasher.update(&salt.to_le_bytes());
    let bytes: [u8; 4] = hasher.finalize().as_bytes()[..4]
        .try_into()
        .expect("BLAKE3 digest has four bytes");
    u32::from_le_bytes(bytes) as f32 / (u32::MAX as f32 + 1.0)
}

fn plugin_filename(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .to_ascii_lowercase()
}
