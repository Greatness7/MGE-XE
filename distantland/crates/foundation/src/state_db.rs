//! Deterministic unit state embedded in the committed generation state, and its diffing.

use anyhow::{Context, Result, bail};
use itertools::Itertools;
use serde::{Deserialize, Serialize};

use crate::framed_archive;
use crate::record_key::StaticRecordKey;
use crate::units::TerrainMeshWorkKey;
use crate::units::{MergeUnitKey, TerrainCellUnitKey, TerrainChunkUnitKey};

const STATE_MAGIC: &[u8; 8] = b"TES3GST1";
/// Inner generation state layout version.
///
/// Version 7 drops the diagnostic `request_identity`, which nothing read. Version 6 adds the
/// post-optimize static bounds used by selective mesh optimization. Version 5
/// stores merge/terrain unit keys and coordinate reverse-index values as typed coordinates rather
/// than `"{x},{y}"` strings. Version 4 added per-shard record-key lists and geometry totals plus the
/// record-global recipe digest. Version 3 replaced the monolithic static payload digest with fixed
/// shard state. Older inner state is incompatible regenerable data: it must force a clean rebuild,
/// never be adopted.
const STATE_FORMAT_VERSION: u32 = 8;
const DIRTY_KEY_LIMIT: usize = 64;

/// One sorted unit-key to fingerprint table.
///
/// Mesh and texture tables use [`String`] path keys. Merge and terrain tables use the typed
/// coordinate keys ([`MergeUnitKey`], [`TerrainCellUnitKey`], [`TerrainChunkUnitKey`]).
#[derive(Clone, Debug, Eq, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct FingerprintTable<K> {
    /// Entries sorted by key order (`Ord` on `K`).
    pub entries: Vec<(K, [u8; 32])>,
}

impl<K> Default for FingerprintTable<K> {
    fn default() -> Self {
        Self { entries: Vec::new() }
    }
}

impl<K: Ord> FingerprintTable<K> {
    /// Builds a deterministic table from an iterator, retaining the smallest duplicate fingerprint.
    pub fn from_entries(entries: impl IntoIterator<Item = (K, [u8; 32])>) -> Self {
        // Sorting fingerprint-ascending within a key makes `dedup_by`, which retains the first of
        // each run, keep the smallest duplicate.
        let entries = entries
            .into_iter()
            .sorted_unstable_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)))
            .dedup_by(|left, right| left.0 == right.0)
            .collect_vec();
        Self { entries }
    }

    fn validate(&self) -> bool {
        self.entries.windows(2).all(|pair| pair[0].0 < pair[1].0)
    }
}

/// The five unit fingerprint tables.
#[derive(Clone, Debug, Default, Eq, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct UnitFingerprintTables {
    /// Fingerprints for mesh work units keyed by normalized mesh path.
    pub mesh: FingerprintTable<String>,
    /// Fingerprints for merged-static work units.
    pub merge: FingerprintTable<MergeUnitKey>,
    /// Fingerprints for texture work units keyed by normalized texture path.
    pub texture: FingerprintTable<String>,
    /// Fingerprints for terrain-cell work units.
    pub terrain_cell: FingerprintTable<TerrainCellUnitKey>,
    /// Fingerprints for terrain-chunk work units.
    pub terrain_chunk: FingerprintTable<TerrainChunkUnitKey>,
}

/// Path-keyed reverse index with owned path consumers (static texture → mesh).
pub type PathReverseIndex = Vec<(String, Vec<String>)>;
/// Path-keyed reverse index with typed merge-cell consumers.
pub type MergeReverseIndex = Vec<(String, Vec<MergeUnitKey>)>;
/// Path-keyed reverse index with typed terrain-cell consumers.
pub type TerrainCellReverseIndex = Vec<(String, Vec<TerrainCellUnitKey>)>;

/// The three unit reverse-dependency indexes.
#[derive(Clone, Debug, Default, Eq, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct ReverseIndexes {
    /// Mesh unit to dependent merge-cell keys.
    pub mesh_to_merge: MergeReverseIndex,
    /// Static texture unit to dependent mesh-unit keys.
    pub texture_to_mesh: PathReverseIndex,
    /// Terrain texture key to consuming terrain-cell keys.
    pub texture_to_terrain_cell: TerrainCellReverseIndex,
}

/// Complete final-input identity, geometry totals, and ordered record keys for one fixed static-mesh shard.
#[derive(Clone, Debug, Default, Eq, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct StaticShardState {
    /// Digest over every final packed value passed to the XESTAT05 serializer.
    pub input_digest: [u8; 32],
    /// Number of static records declared by the shard header.
    pub record_count: u32,
    /// Total subset count across the shard's records (observability).
    pub subset_count: u64,
    /// Total packed-vertex count across the shard's records (observability).
    pub vertex_count: u64,
    /// Total triangle count across the shard's records (observability).
    pub triangle_count: u64,
    /// Typed record keys, positionally aligned with the shard file's record order.
    ///
    /// Strictly ascending by rendered bytes (the shard's intra-record order), unique, and each
    /// assigned to this shard; `record_count == records.len()`.
    pub records: Vec<StaticRecordKey>,
}

/// Post-optimize bounds for one ordinary static mesh.
///
/// These derived values let an incremental statics miss reproduce the previous successful run's
/// global size-filter and merge-membership inputs without reopening clean static shards.
#[derive(Clone, Copy, Debug, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct OptimizedMeshBounds {
    /// Optimized whole-mesh bounding-sphere center.
    pub center: [f32; 3],
    /// Optimized whole-mesh bounding-sphere radius.
    pub radius: f32,
    /// Optimized whole-mesh axis-aligned minimum.
    pub box_min: [f32; 3],
    /// Optimized whole-mesh axis-aligned maximum.
    pub box_max: [f32; 3],
}

impl OptimizedMeshBounds {
    fn validate(self) -> bool {
        self.center.into_iter().all(f32::is_finite)
            && self.radius.is_finite()
            && self.radius >= 0.0
            && self.box_min.into_iter().all(f32::is_finite)
            && self.box_max.into_iter().all(f32::is_finite)
            && self.box_min.into_iter().zip(self.box_max).all(|(min, max)| min <= max)
    }
}

/// Complete immutable state for one generation request.
#[derive(Clone, Debug, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct GenerationState {
    /// Content-affecting settings identity used by the no-op check and state equality.
    ///
    /// Excludes execution-only `force_rebuild` (same rule as the request fingerprint): a forced
    /// clean rebuild must not make a later cached generate look like a settings change.
    pub settings_identity: [u8; 32],
    /// Unit fingerprints.
    pub units: UnitFingerprintTables,
    /// Deterministic reverse-dependency indexes.
    pub reverse: ReverseIndexes,
    /// Statics inputs not exclusively owned by a mesh or merge unit.
    pub statics_global: [u8; 32],
    /// Terrain package-global inputs.
    pub terrain_global: [u8; 32],
    /// Whether terrain tables and the terrain global are present in this state.
    pub terrain_enabled: bool,
    /// Serialized atlas evidence formerly stored in `atlas_cache.bin`.
    pub atlas_cache: Vec<u8>,
    /// Statics-domain digest used for payload carryover.
    pub statics_domain_digest: [u8; 32],
    /// Digest over the static record-production recipe (behavior and key/layout versions).
    ///
    /// Compared directly by the owner-partial planner (a change forces a full static rebuild) and
    /// folded into `statics_domain_digest` (so a change likewise busts whole-bundle carry).
    pub statics_record_global_digest: [u8; 32],
    /// Final packed-input identity for each fixed static-mesh shard produced by this build.
    pub static_mesh_shards: [StaticShardState; crate::output::STATIC_MESH_SHARD_COUNT],
    /// Post-optimize bounds of surviving ordinary meshes, sorted by mesh key.
    pub optimized_mesh_bounds: Vec<(String, OptimizedMeshBounds)>,
    /// Terrain-domain digest used for payload carryover.
    pub terrain_domain_digest: [u8; 32],
    /// Work key of every `terrain.bin` mesh record, positionally aligned with the committed file.
    ///
    /// Empty when terrain is disabled. Sorted and unique, which is exactly the order
    /// `terrain::mesh::assemble_terrain_mesh_set` writes records in.
    pub terrain_records: Vec<TerrainMeshWorkKey>,
    /// Coarse digest over the inputs of the five height-independent terrain DDS products.
    pub terrain_dds_domain_digest: [u8; 32],
}

impl Default for GenerationState {
    fn default() -> Self {
        Self {
            settings_identity: [0; 32],
            units: UnitFingerprintTables::default(),
            reverse: ReverseIndexes::default(),
            statics_global: [0; 32],
            terrain_global: [0; 32],
            terrain_enabled: false,
            atlas_cache: Vec::new(),
            statics_domain_digest: [0; 32],
            statics_record_global_digest: [0; 32],
            static_mesh_shards: std::array::from_fn(|_| StaticShardState::default()),
            optimized_mesh_bounds: Vec::new(),
            terrain_domain_digest: [0; 32],
            terrain_records: Vec::new(),
            terrain_dds_domain_digest: [0; 32],
        }
    }
}

impl GenerationState {
    fn validate(&self) -> bool {
        self.units.mesh.validate()
            && self.units.merge.validate()
            && self.units.texture.validate()
            && self.units.terrain_cell.validate()
            && self.units.terrain_chunk.validate()
            && validate_reverse_index(&self.reverse.mesh_to_merge)
            && validate_reverse_index(&self.reverse.texture_to_mesh)
            && validate_reverse_index(&self.reverse.texture_to_terrain_cell)
            && (self.terrain_enabled
                || (self.units.terrain_cell.entries.is_empty()
                    && self.units.terrain_chunk.entries.is_empty()
                    && self.terrain_records.is_empty()
                    && self.terrain_dds_domain_digest == [0; 32]))
            && self.terrain_records.windows(2).all(|pair| pair[0] < pair[1])
            && self
                .static_mesh_shards
                .iter()
                .enumerate()
                .all(|(shard_index, shard)| validate_static_shard(shard, shard_index))
            && self.optimized_mesh_bounds.windows(2).all(|pair| pair[0].0 < pair[1].0)
            && self.optimized_mesh_bounds.iter().all(|(_, bounds)| bounds.validate())
    }
}

/// Validates one shard's persisted record keys: correct count, strictly ascending by rendered bytes
/// (hence unique and matching the shard's intra-record order), and each key assigned to this shard.
fn validate_static_shard(shard: &StaticShardState, shard_index: usize) -> bool {
    shard.record_count as usize == shard.records.len()
        && shard.records.windows(2).all(|pair| pair[0] < pair[1])
        && shard.records.iter().all(|record| record.shard_id() == shard_index)
}

fn validate_reverse_index<V: Ord>(index: &[(String, Vec<V>)]) -> bool {
    index.windows(2).all(|pair| pair[0].0 < pair[1].0)
        && index
            .iter()
            .all(|(_, values)| values.windows(2).all(|pair| pair[0] < pair[1]))
}

/// Validation status of the previous generation state file.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationStateStatus {
    /// A valid previous state was loaded.
    Loaded,
    /// No previous state file existed.
    Absent,
    /// The frame was valid but its schema version was unsupported.
    VersionMismatch,
    /// The file was unreadable, truncated, checksum-invalid, archive-invalid, or structurally invalid.
    Corrupt,
}

/// Result of loading the previous state.
///
/// The decoded body is carried by [`LoadedGenerationState::Loaded`], so a status can never disagree
/// with the presence of a state: the two are one value, not a pair to be kept consistent.
#[derive(Debug)]
pub enum LoadedGenerationState {
    /// A valid previous state was loaded.
    Loaded(GenerationState),
    /// No previous state file existed.
    Absent,
    /// The frame was valid but its schema version was unsupported.
    VersionMismatch,
    /// The file was unreadable, truncated, checksum-invalid, archive-invalid, or structurally invalid.
    Corrupt,
}

impl LoadedGenerationState {
    /// Returns the status tag carried by the advisory unit report.
    pub fn status(&self) -> GenerationStateStatus {
        match self {
            Self::Loaded(_) => GenerationStateStatus::Loaded,
            Self::Absent => GenerationStateStatus::Absent,
            Self::VersionMismatch => GenerationStateStatus::VersionMismatch,
            Self::Corrupt => GenerationStateStatus::Corrupt,
        }
    }

    /// Returns the decoded state, which is present exactly when the load succeeded.
    pub fn state(&self) -> Option<&GenerationState> {
        match self {
            Self::Loaded(state) => Some(state),
            Self::Absent | Self::VersionMismatch | Self::Corrupt => None,
        }
    }
}

/// Decodes generation-state bytes embedded in index section 2.
pub fn decode(bytes: &[u8]) -> LoadedGenerationState {
    let payload = match framed_archive::decode(bytes, STATE_MAGIC, STATE_FORMAT_VERSION) {
        Ok(payload) => payload,
        Err(framed_archive::FrameError::VersionMismatch { .. }) => return LoadedGenerationState::VersionMismatch,
        Err(_) => return LoadedGenerationState::Corrupt,
    };
    match rkyv::from_bytes::<GenerationState, rkyv::rancor::Error>(payload) {
        Ok(state) if state.validate() => LoadedGenerationState::Loaded(state),
        _ => LoadedGenerationState::Corrupt,
    }
}

/// Serializes a state deterministically into its checksummed archive frame.
pub fn encode(state: &GenerationState) -> Result<Vec<u8>> {
    if !state.validate() {
        bail!("generation state is not sorted");
    }
    // Serialize straight into the reserved frame header so the payload is never copied again.
    let mut bytes = rkyv::api::high::to_bytes_in::<_, rkyv::rancor::Error>(state, vec![0; framed_archive::HEADER_LEN])
        .context("failed to serialize generation state")?;
    framed_archive::seal(&mut bytes, STATE_MAGIC, STATE_FORMAT_VERSION);
    Ok(bytes)
}

/// Enabled/disabled state of one unit kind in a manifest report.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitKindStatus {
    /// The unit table was computed and compared.
    Enabled,
    /// The unit kind was intentionally not constructed for this run.
    Disabled,
}

/// Deterministic dirty-set report for one unit kind.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UnitKindReport {
    /// Whether this kind was constructed.
    pub status: UnitKindStatus,
    /// Current table cardinality.
    pub total: usize,
    /// Added plus removed plus changed cardinality. This may exceed `total` when keys were removed.
    pub dirty: usize,
    /// Number of keys present only in current state.
    pub added: usize,
    /// Number of keys present only in previous state.
    pub removed: usize,
    /// Number of surviving keys whose fingerprint changed.
    pub changed: usize,
    /// Sorted dirty keys, bounded to 64 entries.
    pub dirty_keys: Vec<String>,
    /// Whether additional dirty keys were omitted.
    pub truncated: bool,
}

impl UnitKindReport {
    fn disabled() -> Self {
        Self {
            status: UnitKindStatus::Disabled,
            total: 0,
            dirty: 0,
            added: 0,
            removed: 0,
            changed: 0,
            dirty_keys: Vec::new(),
            truncated: false,
        }
    }
}

/// Reports for all five unit kinds.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UnitKindReports {
    /// Static mesh units.
    pub mesh: UnitKindReport,
    /// Exterior merge units.
    pub merge: UnitKindReport,
    /// Static texture units.
    pub texture: UnitKindReport,
    /// Terrain material-cell units.
    pub terrain_cell: UnitKindReport,
    /// Absolute terrain mesh-chunk units.
    pub terrain_chunk: UnitKindReport,
}

/// Dirty state of one coarse domain.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UnitDomainReport {
    /// Whether the domain was enabled for this run.
    pub enabled: bool,
    /// Whether its global or any domain-owning unit changed.
    pub dirty: bool,
    /// Whether the domain-global fingerprint changed.
    pub global_changed: bool,
}

/// Statics and terrain domain reports.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UnitDomainReports {
    /// Static bundle domain.
    pub statics: UnitDomainReport,
    /// Terrain package domain.
    pub terrain: UnitDomainReport,
}

/// State-db status wrapper in the units manifest section.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UnitStateReport {
    /// Validation result for the previous state file.
    pub status: GenerationStateStatus,
}

/// Top-level non-authoritative unit report written to `generation_report.toml`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UnitsReport {
    /// Units report schema version.
    pub schema: u32,
    /// Previous state validation status.
    pub state: UnitStateReport,
    /// Whether the effective generation settings changed from the comparable previous state.
    #[serde(default)]
    pub settings_changed: bool,
    /// Five per-kind reports.
    pub kinds: UnitKindReports,
    /// Coarse unit-model domain dirty states.
    pub domains: UnitDomainReports,
}

impl Default for UnitsReport {
    fn default() -> Self {
        let empty = UnitKindReport::disabled();
        Self {
            schema: 3,
            state: UnitStateReport {
                status: GenerationStateStatus::Absent,
            },
            settings_changed: false,
            kinds: UnitKindReports {
                mesh: empty.clone(),
                merge: empty.clone(),
                texture: empty.clone(),
                terrain_cell: empty.clone(),
                terrain_chunk: empty,
            },
            domains: UnitDomainReports {
                statics: UnitDomainReport {
                    enabled: false,
                    dirty: false,
                    global_changed: false,
                },
                terrain: UnitDomainReport {
                    enabled: false,
                    dirty: false,
                    global_changed: false,
                },
            },
        }
    }
}

/// One state comparison: the bounded manifest report plus the exhaustive sorted lists generation plans from.
///
/// Every key list is sorted and duplicate-free.
///
/// The report's `dirty_keys` are truncated for readability and must never drive builder decisions;
/// [`GenerationDiff::dirty_terrain_chunks`] is the complete typed list for that.
pub struct GenerationDiff {
    /// Non-authoritative manifest report.
    pub report: UnitsReport,
    /// Mesh unit keys present only in the current state (complete, unbounded).
    pub mesh_added: Vec<String>,
    /// Mesh unit keys whose fingerprint changed (complete, unbounded).
    pub mesh_changed: Vec<String>,
    /// Mesh unit keys present only in the previous state (complete, unbounded).
    pub mesh_removed: Vec<String>,
    /// Exterior-cell merge unit keys present only in the current state (complete, unbounded).
    pub merge_added: Vec<MergeUnitKey>,
    /// Exterior-cell merge unit keys whose fingerprint changed (complete, unbounded).
    pub merge_changed: Vec<MergeUnitKey>,
    /// Exterior-cell merge unit keys present only in the previous state (complete, unbounded).
    pub merge_removed: Vec<MergeUnitKey>,
    /// Every dirty absolute terrain chunk, unbounded and typed.
    pub dirty_terrain_chunks: Vec<TerrainChunkUnitKey>,
    /// Whether any terrain cell was added or removed, which invalidates work-key stability.
    pub terrain_cells_added_or_removed: bool,
    /// Whether the terrain package-global fingerprint changed.
    pub terrain_global_changed: bool,
    /// Whether a previous state was available and comparable at all.
    pub comparable: bool,
}

/// Computes the bounded unit report and the complete typed dirty sets in one comparison.
pub fn diff(current: &GenerationState, previous: &LoadedGenerationState, force_rebuild: bool) -> Result<GenerationDiff> {
    let status = previous.status();
    // A forced rebuild is the only way a loadable previous state becomes non-comparable; every
    // other non-comparable case is already expressed by the absence of a decoded body.
    let previous = if force_rebuild { None } else { previous.state() };
    let mesh = diff_table(&current.units.mesh, previous.map(|state| &state.units.mesh));
    let merge = diff_table(&current.units.merge, previous.map(|state| &state.units.merge));
    let texture = diff_table(&current.units.texture, previous.map(|state| &state.units.texture)).report;
    let (terrain_cell, terrain_chunk) = if current.terrain_enabled {
        (
            diff_table(
                &current.units.terrain_cell,
                previous
                    .filter(|state| state.terrain_enabled)
                    .map(|state| &state.units.terrain_cell),
            ),
            diff_table(
                &current.units.terrain_chunk,
                previous
                    .filter(|state| state.terrain_enabled)
                    .map(|state| &state.units.terrain_chunk),
            ),
        )
    } else {
        (TableDiff::disabled(), TableDiff::disabled())
    };
    let statics_global_changed = previous.is_none_or(|state| state.statics_global != current.statics_global);
    let settings_changed = previous.is_some_and(|state| state.settings_identity != current.settings_identity);
    let terrain_global_changed = current.terrain_enabled
        && previous.is_none_or(|state| !state.terrain_enabled || state.terrain_global != current.terrain_global);
    let statics_dirty = statics_global_changed || mesh.report.dirty != 0 || merge.report.dirty != 0;
    let terrain_dirty = current.terrain_enabled
        && (terrain_global_changed || terrain_cell.report.dirty != 0 || terrain_chunk.report.dirty != 0);
    let terrain_cells_added_or_removed = terrain_cell.report.added != 0 || terrain_cell.report.removed != 0;

    let report = UnitsReport {
        schema: 3,
        state: UnitStateReport { status },
        settings_changed,
        kinds: UnitKindReports {
            mesh: mesh.report,
            merge: merge.report,
            texture,
            terrain_cell: terrain_cell.report,
            terrain_chunk: terrain_chunk.report,
        },
        domains: UnitDomainReports {
            statics: UnitDomainReport {
                enabled: true,
                dirty: statics_dirty,
                global_changed: statics_global_changed,
            },
            terrain: UnitDomainReport {
                enabled: current.terrain_enabled,
                dirty: terrain_dirty,
                global_changed: terrain_global_changed,
            },
        },
    };
    Ok(GenerationDiff {
        merge_added: merge.added_all,
        merge_removed: merge.removed_all,
        merge_changed: merge.changed_all,
        mesh_added: mesh.added_all,
        mesh_removed: mesh.removed_all,
        mesh_changed: mesh.changed_all,
        report,
        dirty_terrain_chunks: terrain_chunk
            .added_all
            .into_iter()
            .merge(terrain_chunk.removed_all)
            .merge(terrain_chunk.changed_all)
            .collect(),
        terrain_cells_added_or_removed,
        terrain_global_changed,
        comparable: previous.is_some(),
    })
}

/// Formats a concise deterministic explanation of why generation must rebuild.
///
/// The structured unit report remains the source of truth. This string is observability only and
/// must never drive builder decisions.
pub fn format_rebuild_cause(units: &UnitsReport, force_rebuild: bool, migration: bool) -> Option<String> {
    if force_rebuild {
        return Some("forced rebuild".to_string());
    }
    if migration {
        return Some("cache absent or incompatible; full regeneration required".to_string());
    }

    let mut clauses = Vec::new();
    match units.state.status {
        GenerationStateStatus::Loaded => {}
        GenerationStateStatus::Absent => clauses.push("no previous generation state".to_string()),
        GenerationStateStatus::VersionMismatch => {
            clauses.push("previous generation state version mismatch".to_string());
        }
        GenerationStateStatus::Corrupt => clauses.push("previous generation state corrupt".to_string()),
    }
    if units.settings_changed {
        clauses.push("settings changed".to_string());
    }
    if units.domains.statics.global_changed {
        clauses.push("statics global inputs changed".to_string());
    }
    if units.domains.terrain.global_changed {
        clauses.push("terrain global inputs changed".to_string());
    }

    push_kind_cause(&mut clauses, "meshes", &units.kinds.mesh);
    push_kind_cause(&mut clauses, "merge groups", &units.kinds.merge);
    push_kind_cause(&mut clauses, "textures", &units.kinds.texture);
    push_kind_cause(&mut clauses, "terrain cells", &units.kinds.terrain_cell);
    push_kind_cause(&mut clauses, "terrain chunks", &units.kinds.terrain_chunk);

    (!clauses.is_empty()).then(|| clauses.join("; "))
}

fn push_kind_cause(clauses: &mut Vec<String>, label: &str, report: &UnitKindReport) {
    if report.dirty != 0 {
        clauses.push(format!("{label} +{}/-{}/~{}", report.added, report.removed, report.changed));
    }
}

/// One table comparison: the bounded report plus its complete, partitioned dirty-key lists.
struct TableDiff<K> {
    report: UnitKindReport,
    /// Keys present only in the current table (sorted).
    added_all: Vec<K>,
    /// Keys present only in the previous table (sorted).
    removed_all: Vec<K>,
    /// Keys present in both with a different fingerprint (sorted).
    changed_all: Vec<K>,
}

impl<K> TableDiff<K> {
    fn disabled() -> Self {
        Self {
            report: UnitKindReport::disabled(),
            added_all: Vec::new(),
            removed_all: Vec::new(),
            changed_all: Vec::new(),
        }
    }
}

/// Compares two fingerprint tables in one linear pass over their sorted entries.
///
/// Both inputs are strictly ascending and duplicate-free by construction: [`FingerprintTable::from_entries`]
/// sorts and dedups, [`encode`] refuses to serialize a state failing `validate`, and [`decode`]
/// reports such a state as corrupt rather than returning it. A merge walk therefore emits every
/// output list already in key order, so nothing here needs a map, a re-sort, or a dedup pass.
fn diff_table<K>(current: &FingerprintTable<K>, previous: Option<&FingerprintTable<K>>) -> TableDiff<K>
where
    K: Clone + Ord + std::fmt::Display,
{
    let empty = FingerprintTable::default();
    let previous = previous.unwrap_or(&empty);
    let mut added: Vec<K> = Vec::new();
    let mut removed: Vec<K> = Vec::new();
    let mut changed: Vec<K> = Vec::new();
    let (mut current_index, mut previous_index) = (0_usize, 0_usize);
    while current_index < current.entries.len() && previous_index < previous.entries.len() {
        let (current_key, current_hash) = &current.entries[current_index];
        let (previous_key, previous_hash) = &previous.entries[previous_index];
        match current_key.cmp(previous_key) {
            std::cmp::Ordering::Less => {
                added.push(current_key.clone());
                current_index += 1;
            }
            std::cmp::Ordering::Greater => {
                removed.push(previous_key.clone());
                previous_index += 1;
            }
            std::cmp::Ordering::Equal => {
                if current_hash != previous_hash {
                    changed.push(current_key.clone());
                }
                current_index += 1;
                previous_index += 1;
            }
        }
    }
    // Whichever side still has entries holds only keys the other side lacks entirely.
    for (key, _) in &current.entries[current_index..] {
        added.push(key.clone());
    }
    for (key, _) in &previous.entries[previous_index..] {
        removed.push(key.clone());
    }
    let dirty = added.len() + removed.len() + changed.len();
    let truncated = dirty > DIRTY_KEY_LIMIT;
    // The partitions are disjoint and individually sorted. Merge only the bounded report view
    // instead of retaining a second owned copy of every dirty key.
    let dirty_keys = added
        .iter()
        .merge(removed.iter())
        .merge(changed.iter())
        .take(DIRTY_KEY_LIMIT)
        .map(|key| key.to_string())
        .collect_vec();
    TableDiff {
        report: UnitKindReport {
            status: UnitKindStatus::Enabled,
            total: current.entries.len(),
            dirty,
            added: added.len(),
            removed: removed.len(),
            changed: changed.len(),
            dirty_keys,
            truncated,
        },
        added_all: added,
        removed_all: removed,
        changed_all: changed,
    }
}

#[cfg(test)]
mod tests;
