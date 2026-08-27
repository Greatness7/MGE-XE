use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use bytes_io::Reader;
use tes3::esp::{Cell, Header, Plugin, Static, TES3Object};

use super::filter::grass_density_for_object;
use super::*;
use crate::vfs::{make_normalized, normalize};
use distantland_foundation::identity::FileIdentity;

/// Grass placements a plugin must exceed before it is suggested as groundcover.
///
/// Bulk placement is what distinguishes a groundcover plugin from an ordinary mod that plants a
/// few decorative grass meshes. Real groundcover passes this within its first cell or two.
const GRASS_PLUGIN_INSTANCE_THRESHOLD: u64 = 50;

/// Records outside `TES3`/`STAT`/`CELL` a plugin may carry and still be considered groundcover.
///
/// Sampled, not derived: across a 22-plugin install the worst real groundcover file carried 1 and
/// the nearest content mod 342. Deliberately loose, because Gate B still rejects small content mods
/// and the headroom covers CS-dirtied `GMST` tails. A grass/terrain hybrid exceeds it and is
/// rejected without its placements ever being examined.
const GRASS_PLUGIN_FOREIGN_RECORD_TOLERANCE: usize = 100;

/// Buffer size for [`RecordWalk`].
///
/// The master prefetch seeks far more than it reads, so a large buffer keeps those seeks in memory:
/// 64 KiB ran the test install's four masters in ~38 ms against ~62 ms at the 8 KiB default.
const RECORD_WALK_BUFFER: usize = 64 * 1024;

/// Classifies `path` as a dedicated groundcover plugin for GUI suggestions.
///
/// `data_dirs` is the layered data-directory list, lowest priority first. Prefer
/// [`classify_grass_plugins`] for more than one path: it shares the master prefetch across the
/// batch.
pub fn is_grass_plugin(path: &Path, data_dirs: &[PathBuf]) -> bool {
    let paths = [path.to_path_buf()];
    classify_grass_plugins(&paths, data_dirs)[0]
}

/// Classifies a whole plugin list, one `bool` per input path.
///
/// Meshes under the conventional `grass\` directory count as grass. Generation remains
/// authoritative: it additionally applies VFS resolution and the job's normal static overrides, so
/// it can disagree with this answer.
///
/// Three gates, ordered so the expensive ones see as few files as possible:
///
/// - **Gate 0** counts records outside `TES3`/`STAT`/`CELL` and rejects past
///   [`GRASS_PLUGIN_FOREIGN_RECORD_TOLERANCE`], streaming and without decoding.
/// - **Gate A** requires a grass static to be available: defined by the plugin, or by one of its
///   masters. Groundcover written against a landmass mod commonly defines none of its own.
/// - **Gate B** requires more than [`GRASS_PLUGIN_INSTANCE_THRESHOLD`] surviving exterior
///   placements of one.
///
/// Gate A's union is conservative rather than override resolution: a later non-grass `STAT` sharing
/// an id with a master's grass `STAT` should shadow it, but `MAST` order gives plugin-relative
/// indices, not the global load order. That biases toward false positives, which are cheap here.
///
/// A plugin that cannot be read or parsed classifies as `false`, as does one whose declared master
/// is missing, unreadable, or malformed. An unresolvable master means its grass statics are
/// unknown, not absent, and it fails only the plugins that declare it.
///
/// Verdicts depend on `data_dirs`, so a caller caching them must invalidate on any change to that
/// list, not only on a changed plugin file.
pub fn classify_grass_plugins(paths: &[PathBuf], data_dirs: &[PathBuf]) -> Vec<bool> {
    let mut verdicts = vec![false; paths.len()];

    // Phase 1: Gate 0. The only pass that sees every file, so it must not read them whole.
    let survivors: Vec<usize> = paths
        .par_iter()
        .enumerate()
        .filter(|(_, path)| within_foreign_record_tolerance(path).unwrap_or(false))
        .map(|(index, _)| index)
        .collect();

    // Phase 2: declared masters, then each distinct master's grass ids, sequential. A survivor
    // whose own `TES3` record will not read is not a loadable plugin.
    let survivors: Vec<(usize, Vec<String>)> = survivors
        .into_iter()
        .filter_map(|index| Some((index, declared_masters(&paths[index])?)))
        .collect();
    if survivors.is_empty() {
        return verdicts;
    }

    let by_name = index_data_dirs(data_dirs);
    let mut prefetched: HashMap<PathBuf, Option<HashSet<UString>>> = HashMap::new();
    for (_, masters) in &survivors {
        for name in masters {
            let Some(resolved) = by_name.get(&name.to_ascii_lowercase()) else {
                continue;
            };
            if !prefetched.contains_key(resolved) {
                prefetched.insert(resolved.clone(), master_grass_ids(resolved).ok());
            }
        }
    }

    // Phase 3: Gates A and B. A survivor unions only the masters it declares. One flattened set
    // would lend a master's grass ids to plugins that never named it.
    let classified: Vec<(usize, bool)> = survivors
        .par_iter()
        .map(|(index, masters)| {
            let mut master_ids = Vec::with_capacity(masters.len());
            for name in masters {
                match by_name
                    .get(&name.to_ascii_lowercase())
                    .and_then(|resolved| prefetched.get(resolved))
                {
                    Some(Some(ids)) => master_ids.push(ids),
                    _ => return (*index, false),
                }
            }
            (*index, grass_gates(&paths[*index], &master_ids).unwrap_or(false))
        })
        .collect();
    for (index, verdict) in classified {
        verdicts[index] = verdict;
    }

    verdicts
}

/// Gate 0: whether `path` carries few enough records outside `TES3`/`STAT`/`CELL` to be groundcover.
///
/// Returns as soon as the tolerance is exceeded, which for a content mod is within a few kilobytes.
fn within_foreign_record_tolerance(path: &Path) -> Result<bool> {
    let mut walk = RecordWalk::open(path)?;
    let mut foreign = 0usize;
    while let Some(frame) = walk.next_frame()? {
        if !matches!(&frame.tag, b"TES3" | b"STAT" | b"CELL") {
            foreign += 1;
            if foreign > GRASS_PLUGIN_FOREIGN_RECORD_TOLERANCE {
                return Ok(false);
            }
        }
        walk.skip_body(&frame)?;
    }
    Ok(true)
}

/// The master filenames a plugin declares, read from its `TES3` record alone.
///
/// `None` means the record is missing or malformed, which makes the plugin unloadable.
fn declared_masters(path: &Path) -> Option<Vec<String>> {
    let header = Header::from_path(path).ok()?;
    Some(header.masters.into_iter().map(|(name, _)| name).collect())
}

/// Indexes the data directories by lowercased filename, for resolving bare master names.
///
/// `data_dirs` is lowest priority first, so the last directory holding a name wins, matching
/// `scan_universe` and bare-name VFS lookup. Enumerating rather than probing `dir.join(name)`
/// avoids depending on the platform's filename case rules.
fn index_data_dirs(data_dirs: &[PathBuf]) -> HashMap<String, PathBuf> {
    let mut by_name = HashMap::new();
    for dir in data_dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().map(|name| name.to_string_lossy().to_ascii_lowercase()) else {
                continue;
            };
            by_name.insert(name, path);
        }
    }
    by_name
}

/// The grass static ids a master defines, streaming its `STAT` records.
///
/// Not `Plugin::load_path_filtered`: that reads the file whole and retains every decoded record,
/// which on a 227 MB master is ~80 MB of avoidable peak, and it stops framing silently on damage.
fn master_grass_ids(path: &Path) -> Result<HashSet<UString>> {
    let mut walk = RecordWalk::open(path)?;
    let mut ids = HashSet::new();
    while let Some(frame) = walk.next_frame()? {
        if &frame.tag != b"STAT" {
            walk.skip_body(&frame)?;
            continue;
        }
        let bytes = walk.read_record(&frame)?;
        let Ok(TES3Object::Static(object)) = Reader::new(&bytes).load::<TES3Object>() else {
            bail!("Failed to parse static in grass plugin master {}", path.display());
        };
        if is_grass_mesh(&object.mesh) {
            ids.insert(UString::new(object.id));
        }
    }
    Ok(ids)
}

/// Gates A and B over one Gate 0 survivor, given the grass ids of the masters it declares.
///
/// Only `STAT` and `CELL` are decoded, one record at a time. A cell's on-disk references expand
/// several-fold once materialized, and this runs across a whole install in parallel, so holding a
/// whole decoded plugin per worker is what exhausted memory on a large install.
///
/// Gate B stops at the first reference past the threshold, so damage later in the file does not
/// prevent a verdict.
fn grass_gates(path: &Path, master_grass_ids: &[&HashSet<UString>]) -> Result<bool> {
    let Ok(bytes) = std::fs::read(path) else {
        bail!("Failed to read grass plugin {}", path.display());
    };
    let (stat_ranges, cell_ranges) = grass_record_ranges(&bytes);

    // Gate A: a grass static must be available, from the plugin itself or from its masters.
    let mut grass_object_ids: HashSet<UString> = HashSet::new();
    for range in stat_ranges {
        let Ok(TES3Object::Static(object)) = Reader::new(&bytes[range]).load::<TES3Object>() else {
            bail!("Failed to parse grass plugin {}", path.display());
        };
        if is_grass_mesh(&object.mesh) {
            grass_object_ids.insert(UString::new(object.id));
        }
    }
    for ids in master_grass_ids {
        grass_object_ids.extend(ids.iter().cloned());
    }
    if grass_object_ids.is_empty() {
        return Ok(false);
    }

    // Gate B: bulk placement. Each cell is dropped before the next is decoded, bounding peak at
    // the file plus one cell.
    //
    // `mast_index` is not consulted: it names the source of the reference, not of the base object's
    // definition, so filtering on it would drop the overrides and moved placements groundcover
    // legitimately carries. Without list context, `deleted` is the only reference state available.
    let mut count: u64 = 0;
    for range in cell_ranges {
        let Ok(TES3Object::Cell(cell)) = Reader::new(&bytes[range]).load::<TES3Object>() else {
            bail!("Failed to parse grass plugin {}", path.display());
        };
        if cell.is_interior() {
            continue;
        }
        for reference in cell.references.values() {
            if reference.deleted.is_some() {
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

/// Whether a mesh path is under the conventional `grass\` directory.
fn is_grass_mesh(mesh: &str) -> bool {
    normalize(mesh).starts_with("grass\\")
}

/// One record's framing, with its [`RecordWalk`] positioned at the start of its body.
struct RecordFrame {
    tag: [u8; 4],
    /// The `(tag, len)` prefix as read, so the record can be reassembled for decoding.
    prefix: [u8; 8],
    /// Bytes following the prefix: the declared length plus 8.
    body_len: u64,
}

/// Streaming walk over a plugin's top-level record framing.
///
/// [`record_headers`] needs the whole file in memory; this reads only what it uses, which is what
/// lets Gate 0 reject a plugin after one buffer fill.
///
/// The offset is tracked here and every declared extent checked against the file length, because
/// `File::seek` past the end succeeds silently where `Reader::skip` refuses. Without that a
/// truncated final record reads as a clean end of file.
struct RecordWalk {
    reader: BufReader<File>,
    /// Offset of the next record's prefix.
    offset: u64,
    length: u64,
}

impl RecordWalk {
    fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("Failed to open plugin {}", path.display()))?;
        let length = file
            .metadata()
            .with_context(|| format!("Failed to stat plugin {}", path.display()))?
            .len();
        Ok(Self {
            reader: BufReader::with_capacity(RECORD_WALK_BUFFER, file),
            offset: 0,
            length,
        })
    }

    /// Reads the next record's framing, or `None` at a clean end of file.
    fn next_frame(&mut self) -> Result<Option<RecordFrame>> {
        if self.offset == self.length {
            return Ok(None);
        }
        ensure!(self.length - self.offset >= 8, "Truncated record header");
        let mut prefix = [0u8; 8];
        self.reader.read_exact(&mut prefix)?;

        let tag = [prefix[0], prefix[1], prefix[2], prefix[3]];
        // Every top-level record is an 8-byte `(tag, len)` prefix followed by a further `len + 8`.
        let body_len = u64::from(u32::from_le_bytes([prefix[4], prefix[5], prefix[6], prefix[7]])) + 8;
        let end = self
            .offset
            .checked_add(body_len + 8)
            .context("Record extent overflows the file offset")?;
        ensure!(end <= self.length, "Record extent runs past end of file");
        self.offset = end;

        Ok(Some(RecordFrame { tag, prefix, body_len }))
    }

    fn skip_body(&mut self, frame: &RecordFrame) -> Result<()> {
        let body_len = i64::try_from(frame.body_len).context("Record body too large to skip")?;
        self.reader.seek_relative(body_len)?;
        Ok(())
    }

    /// Reads the current record whole, prefix included, ready to decode.
    ///
    /// [`Self::next_frame`] has already checked the declared extent against the file length, so the
    /// allocation is bounded by the file size.
    fn read_record(&mut self, frame: &RecordFrame) -> Result<Vec<u8>> {
        let body_len = usize::try_from(frame.body_len).context("Record body too large to read")?;
        let mut bytes = vec![0u8; body_len + 8];
        bytes[..8].copy_from_slice(&frame.prefix);
        self.reader.read_exact(&mut bytes[8..])?;
        Ok(bytes)
    }
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
/// Scoped per cell because some groundcover generators restart `refr_index` at 0 in every cell,
/// so a plugin-global refnum would collide. Both reference implementations key the same way:
/// MGE-XE's legacy generator put cell coordinates in the key string, and OpenMW rebuilds its `refs`
/// map for each cell in `Groundcover::collectInstances`. The final field distinguishes unresolved
/// master references from local placements while they share the declaring plugin's fallback source.
type GrassRefKey = ((i32, i32), SourceId, u32, u32);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum UnresolvedMasterTarget {
    Named(String),
    InvalidIndex(u32),
}

#[derive(Clone, Copy, Debug, Default)]
struct UnresolvedReferenceCounts {
    placements: u64,
    deletes: u64,
}

struct UnresolvedMasterCount {
    declaring_plugin: String,
    declaring_index: usize,
    target: UnresolvedMasterTarget,
    references: UnresolvedReferenceCounts,
}

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
    main_objects: &HashMap<String, ObjectDefinition<'a>>,
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

    // Pass 2: seed grass definitions from the already-merged active load order, then union the
    // dedicated list over them. Later grass plugins win, matching ordinary override rules.
    let mut definitions: HashMap<String, (Option<SourceId>, ObjectDefinition<'a>)> = main_objects
        .iter()
        .filter(|(_, definition)| grass_density_for_object(definition, args, overrides).is_some())
        .map(|(object_id, definition)| (object_id.clone(), (None, definition.clone())))
        .collect();
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
                definitions.insert(id.clone(), (Some(entry.source), definition));
            }
        }
    }

    // Pass 3: resolve placements across the ordered list. Every exterior reference participates,
    // grass-classified or not, so that a later plugin's override or delete lands on its target; the
    // grass filter is applied afterwards, as OpenMW does when it looks up the groundcover model.
    let mut resolved: BTreeMap<GrassRefKey, GrassCandidate> = BTreeMap::new();
    let mut source_by_filename: HashMap<String, SourceId> = HashMap::new();
    let active_plugin_names: HashSet<String> = vfs.active_plugins().iter().map(|path| plugin_filename(path)).collect();
    let grass_plugin_positions: HashMap<String, usize> = paths
        .iter()
        .enumerate()
        .map(|(index, path)| (plugin_filename(path), index))
        .collect();
    let mut unresolved_master_counts = Vec::new();
    for (declaring_index, entry) in loaded.into_iter().enumerate() {
        let mut unresolved_masters: BTreeMap<UnresolvedMasterTarget, UnresolvedReferenceCounts> = BTreeMap::new();
        for cell in entry.plugin.into_objects_of_type::<Cell>() {
            if cell.is_interior() {
                continue;
            }
            for ((mast_index, refr_index), reference) in cell.references {
                let is_delete = reference.deleted.is_some();
                // Only an earlier grass-list entry can own override/delete identity. An unresolved
                // target falls back to this plugin for placement ownership, with the master index
                // retained in the temporary key so it cannot collide with a local placement.
                let (source, unresolved_mast_index) = if mast_index == 0 {
                    (entry.source, 0)
                } else if let Some(resolved_master) = entry
                    .masters
                    .get(mast_index as usize - 1)
                    .and_then(|name| source_by_filename.get(name))
                    .copied()
                {
                    (resolved_master, 0)
                } else {
                    let target = entry.masters.get(mast_index as usize - 1).cloned().map_or(
                        UnresolvedMasterTarget::InvalidIndex(mast_index),
                        UnresolvedMasterTarget::Named,
                    );
                    let counts = unresolved_masters.entry(target).or_default();
                    if is_delete {
                        counts.deletes += 1;
                    } else {
                        counts.placements += 1;
                    }
                    (entry.source, mast_index)
                };

                let key = (cell.data.grid, source, refr_index, unresolved_mast_index);
                if is_delete {
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
        unresolved_master_counts.extend(
            unresolved_masters
                .into_iter()
                .map(|(target, references)| UnresolvedMasterCount {
                    declaring_plugin: entry.filename.clone(),
                    declaring_index,
                    target,
                    references,
                }),
        );
        // Only plugins already processed can be addressed as masters, mirroring OpenMW's backwards
        // search over readers with a lower index than the one being resolved.
        source_by_filename.insert(entry.filename, entry.source);
    }
    warnings.extend(report_unresolved_masters(
        &unresolved_master_counts,
        &active_plugin_names,
        &grass_plugin_positions,
    ));

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
        let Some(source) = source else {
            continue;
        };
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

fn report_unresolved_masters(
    counts: &[UnresolvedMasterCount],
    active_plugin_names: &HashSet<String>,
    grass_plugin_positions: &HashMap<String, usize>,
) -> Vec<UsageWarning> {
    let mut warnings = Vec::new();
    for count in counts {
        let placements = count.references.placements;
        let deletes = count.references.deletes;
        match &count.target {
            UnresolvedMasterTarget::Named(master) if active_plugin_names.contains(master) => {
                if deletes != 0 {
                    warnings.push(UsageWarning {
                        code: "grass_plugin_content_master_delete_ignored".to_owned(),
                        message: format!(
                            concat!(
                                "Grass plugin {} contains {} delete reference(s) targeting active content master ",
                                "{}. Dedicated grass loading does not import main-load placements, so these ",
                                "deletes cannot modify them and are ignored."
                            ),
                            count.declaring_plugin, deletes, master
                        ),
                    });
                }
            }
            UnresolvedMasterTarget::Named(master) if grass_plugin_positions.contains_key(master) => {
                debug_assert!(
                    grass_plugin_positions[master] > count.declaring_index,
                    "an earlier grass master should already have resolved"
                );
                warnings.push(UsageWarning {
                    code: "grass_plugin_master_after_dependent".to_owned(),
                    message: format!(
                        concat!(
                            "Grass plugin {} addresses master {}, but that master appears later in ",
                            "grass_plugins. {} non-delete reference(s) are treated as new placements; ",
                            "{} delete reference(s) cannot apply. Move {} before {}."
                        ),
                        count.declaring_plugin, master, placements, deletes, master, count.declaring_plugin
                    ),
                });
            }
            UnresolvedMasterTarget::Named(master) => {
                warnings.push(UsageWarning {
                    code: "grass_plugin_master_unselected".to_owned(),
                    message: format!(
                        concat!(
                            "Grass plugin {} addresses unselected master {}. {} non-delete reference(s) are ",
                            "eligible only as fallback placements; {} delete reference(s) are ignored. Enable ",
                            "a content master under plugins, or put an actual groundcover master before {} ",
                            "under grass_plugins."
                        ),
                        count.declaring_plugin, master, placements, deletes, count.declaring_plugin
                    ),
                });
            }
            UnresolvedMasterTarget::InvalidIndex(mast_index) => {
                warnings.push(UsageWarning {
                    code: "grass_plugin_master_index_invalid".to_owned(),
                    message: format!(
                        concat!(
                            "Grass plugin {} contains malformed reference data: MAST index {} is outside its ",
                            "declared master table. {} non-delete reference(s) are eligible only as fallback ",
                            "placements; {} delete reference(s) are ignored."
                        ),
                        count.declaring_plugin, mast_index, placements, deletes
                    ),
                });
            }
        }
    }
    warnings
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
