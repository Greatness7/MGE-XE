//! Complete-or-absent storage authority.
//!
//! `generation_state.bin` is the only publication authority. A missing, invalid, or incomplete
//! state means the cache is absent. Readers and writers share one inventory codec and the same
//! Routine/Full artifact checks.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result, bail};
use rkyv::rancor::Error as RkyvError;

use crate::framed_archive::{self, FrameError};
use crate::mge_xe::distant_statics::{STATIC_MESHES_MAGIC, STATIC_MESHES_VERSION};
use crate::mge_xe::distant_terrain::{TERRAIN_FILE_MAGIC, TERRAIN_FILE_VERSION};
use crate::mge_xe::terrain_occlusion::{TERRAIN_OCCLUSION_MAGIC, TERRAIN_OCCLUSION_VERSION};
use crate::output::MGE_DL_VERSION;
use crate::output::{STATIC_MESH_SHARD_COUNT, static_mesh_shard_relative_path};
use crate::state_db::{self, GenerationState, LoadedGenerationState};

use super::durable;
use super::fault;
use super::path;

/// Outer framed-archive magic for the complete-or-absent state file.
pub const COMMITTED_STATE_MAGIC: &[u8; 8] = b"TES3GCS1";
/// Storage-envelope schema version for [`CommittedState`].
pub const COMMITTED_STATE_VERSION: u32 = 1;

/// Kind of one required runtime artifact listed in the committed inventory.
#[derive(Clone, Copy, Debug, Eq, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug, Eq, PartialEq))]
#[repr(u8)]
pub enum ArtifactKind {
    /// `distantland\version`.
    Version = 1,
    /// `distantland\statics\usage.data`.
    Usage = 2,
    /// One fixed static-mesh shard produced by this build.
    StaticShard = 3,
    /// One generator-owned static atlas page under `statics\textures\`.
    AtlasDds = 4,
    /// `distantland\terrain.bin`.
    TerrainPayload = 5,
    /// One of the five terrain companion DDS outputs.
    TerrainDds = 6,
    /// `distantland\terrain_occlusion.bin`.
    TerrainOcclusion = 7,
}

/// One required artifact recorded in a published state.
#[derive(Clone, Debug, Eq, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug, Eq, PartialEq))]
pub struct RequiredArtifact {
    /// Artifact class used for header validation.
    pub kind: ArtifactKind,
    /// Path relative to `distantland`, using the backslash path grammar.
    pub relative_path: String,
    /// Exact on-disk byte length.
    pub byte_length: u64,
    /// BLAKE3 of the complete file contents.
    pub content_blake3: [u8; 32],
}

/// Complete published authority: opaque generation state plus the required-artifact inventory.
///
/// Equality and the persisted archive are decided solely by `state_bytes` and `artifacts`; the
/// `state_cache` field is a runtime-only memoization (see its docs and the manual [`PartialEq`]).
#[derive(Clone, Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug, Eq, PartialEq))]
pub struct CommittedState {
    /// Existing `state_db::encode` bytes; recipe/schema evolves independently.
    pub state_bytes: Vec<u8>,
    /// Required runtime artifacts, sorted by normalized relative-path bytes.
    pub artifacts: Vec<RequiredArtifact>,
    /// Runtime-only memoized decode of `state_bytes`, so the `state_db` decode runs at most once
    /// per committed state and is shared cheaply across clones through the `Arc`.
    ///
    /// Skipped from the rkyv archive with [`rkyv::with::Skip`] specifically to keep the
    /// committed-state wire layout byte-identical to the two persisted fields: it serializes to
    /// nothing, deserializes back to an empty cell, and is repopulated lazily on first access. It is
    /// excluded from equality so cache occupancy can never influence publication equality.
    #[rkyv(with = rkyv::with::Skip)]
    state_cache: OnceLock<Arc<LoadedGenerationState>>,
}

/// Runtime equality ignores the `state_cache` memoization: only the persisted `state_bytes`
/// bytes and the artifact inventory decide whether two committed states are equal, so a populated
/// cache on one side and an empty cell on the other still compare equal.
impl PartialEq for CommittedState {
    fn eq(&self, other: &Self) -> bool {
        self.state_bytes == other.state_bytes && self.artifacts == other.artifacts
    }
}

impl Eq for CommittedState {}

/// Filesystem validation cost requested by a state or output reader.
///
/// Re-exported as [`crate::output_index::OutputValidation`], which is the name hosts use.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactValidation {
    /// Existence, exact length, and fixed current header rules.
    ///
    /// What both hosts pass on the every-launch path, where the cost of hashing the whole tree
    /// would be paid on each start.
    Routine,
    /// Also hashes every listed artifact and compares `content_blake3`.
    ///
    /// No product caller by design: this is the verification tier the interruption and pruning
    /// suites end on, where the property under test is that a tree published after a simulated
    /// crash matches its recorded digests byte for byte. `Routine` cannot express that. A torn
    /// artifact of the right length passes it. Keep the tier as long as those tests exist.
    Full,
}

/// Stable failure while decoding or validating committed state.
///
/// Every variant maps one-to-one onto the reason code returned by [`StateError::code`], which is
/// what the output reader and startup status publish. Classification therefore lives in the variant
/// set: construct the variant that matches the failure rather than encoding it in the message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StateError {
    /// Outer magic is zeroed or otherwise invalid.
    InvalidMagic,
    /// Outer schema version is not supported.
    VersionMismatch {
        /// Version encoded in the stored state.
        found: u32,
        /// Version required by this reader.
        expected: u32,
    },
    /// Frame/body is truncated or checksum-invalid.
    Corrupt(&'static str),
    /// Inventory invariants fail (ordering, kinds, terrain agreement, path grammar).
    InvalidInventory(String),
    /// An inventory-listed artifact is absent from the output tree.
    MissingArtifact(String),
    /// An artifact's on-disk length disagrees with the inventory.
    ArtifactLength(String),
    /// An artifact's contents disagree with the inventory's recorded `content_blake3`.
    ArtifactHash(String),
    /// An artifact's fixed header (payload magic, version byte, or DDS signature) is wrong.
    ArtifactHeader(String),
    /// An artifact failed a cross-check that is not a length, hash, or header rule.
    ArtifactInvalid(String),
    /// I/O on an inventory-listed payload failed.
    ArtifactIo(String),
    /// `generation_state.bin` is absent, which means the cache is absent.
    MissingState(String),
    /// I/O on `generation_state.bin` itself failed.
    StateIo(String),
}

impl StateError {
    /// Returns the stable machine-readable reason code for this failure.
    ///
    /// This is the crate's single classification authority for state failures: it is total over the
    /// variant set and never inspects the human-readable message, so rewording a message cannot
    /// silently retarget a code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidMagic => "invalid_state_magic",
            Self::VersionMismatch { .. } => "state_schema_mismatch",
            Self::Corrupt(_) => "corrupt_state",
            Self::InvalidInventory(_) => "invalid_inventory",
            Self::MissingArtifact(_) => "missing_artifact",
            Self::ArtifactLength(_) => "artifact_length_mismatch",
            Self::ArtifactHash(_) => "artifact_hash_mismatch",
            Self::ArtifactHeader(_) => "artifact_header_mismatch",
            Self::ArtifactInvalid(_) => "artifact_validation_failed",
            Self::ArtifactIo(_) => "artifact_io",
            Self::MissingState(_) => "missing_state",
            Self::StateIo(_) => "state_io",
        }
    }
}

impl std::fmt::Display for StateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidMagic => formatter.write_str("generation_state.bin magic is invalid"),
            Self::VersionMismatch { found, expected } => {
                write!(
                    formatter,
                    "generation_state.bin schema version {found} is unsupported; expected {expected}"
                )
            }
            Self::Corrupt(message) => write!(formatter, "generation_state.bin is corrupt: {message}"),
            Self::InvalidInventory(message) => write!(formatter, "generation_state inventory is invalid: {message}"),
            Self::MissingArtifact(path) => write!(formatter, "required artifact is missing: {path}"),
            Self::ArtifactLength(message) => write!(formatter, "required artifact has the wrong length: {message}"),
            Self::ArtifactHash(path) => {
                write!(
                    formatter,
                    "required artifact does not match its recorded content hash: {path}"
                )
            }
            Self::ArtifactHeader(message) => write!(formatter, "required artifact has an invalid header: {message}"),
            Self::ArtifactInvalid(message) => write!(formatter, "required artifact failed validation: {message}"),
            Self::ArtifactIo(message) => write!(formatter, "required artifact I/O failed: {message}"),
            Self::MissingState(path) => write!(formatter, "generation_state.bin is missing: {path}"),
            Self::StateIo(message) => write!(formatter, "generation_state I/O failed: {message}"),
        }
    }
}

impl std::error::Error for StateError {}

impl From<FrameError> for StateError {
    fn from(error: FrameError) -> Self {
        match error {
            FrameError::InvalidMagic => Self::InvalidMagic,
            FrameError::VersionMismatch { found, expected } => Self::VersionMismatch { found, expected },
            FrameError::Corrupt(message) => Self::Corrupt(message),
        }
    }
}

impl CommittedState {
    /// Builds a committed state from state bytes and a complete artifact inventory.
    ///
    /// Sorts artifacts by relative path and validates inventory invariants against the decoded
    /// generation state's terrain enablement.
    pub fn new(state_bytes: Vec<u8>, mut artifacts: Vec<RequiredArtifact>) -> Result<Self> {
        artifacts.sort_by(|left, right| left.relative_path.as_bytes().cmp(right.relative_path.as_bytes()));
        let committed = Self {
            state_bytes,
            artifacts,
            state_cache: OnceLock::new(),
        };
        committed.validate_invariants().map_err(|error| anyhow::anyhow!(error))?;
        Ok(committed)
    }

    /// Decodes the generation state blob with the independent `state_db` codec.
    ///
    /// The decode runs at most once: the result is memoized in `state_cache` and every later
    /// call, and every clone shares the backing `Arc` and reuses the same decoded value.
    pub fn loaded_state(&self) -> &LoadedGenerationState {
        self.state_cache
            .get_or_init(|| Arc::new(state_db::decode(&self.state_bytes)))
            .as_ref()
    }

    /// Returns the decoded generation state when it is fully loadable.
    pub fn state(&self) -> Option<&GenerationState> {
        self.loaded_state().state()
    }

    /// Returns whether terrain is enabled in the generation state.
    pub fn terrain_enabled(&self) -> Result<bool> {
        match self.loaded_state() {
            LoadedGenerationState::Loaded(state) => Ok(state.terrain_enabled),
            LoadedGenerationState::Absent => bail!("committed generation state is absent"),
            LoadedGenerationState::VersionMismatch => bail!("committed generation state version mismatch"),
            LoadedGenerationState::Corrupt => bail!("committed generation state is corrupt"),
        }
    }

    /// Looks up one inventory entry by relative path.
    pub fn artifact(&self, relative_path: &str) -> Option<&RequiredArtifact> {
        self.artifacts.iter().find(|entry| entry.relative_path == relative_path)
    }

    /// Looks up the first inventory entry of `kind`.
    pub fn artifact_by_kind(&self, kind: ArtifactKind) -> Option<&RequiredArtifact> {
        self.artifacts.iter().find(|entry| entry.kind == kind)
    }

    /// Returns every atlas page path relative to `distantland`.
    pub fn atlas_relative_paths(&self) -> impl Iterator<Item = &str> {
        self.artifacts
            .iter()
            .filter(|entry| entry.kind == ArtifactKind::AtlasDds)
            .map(|entry| entry.relative_path.as_str())
    }

    fn validate_invariants(&self) -> Result<(), StateError> {
        if self
            .artifacts
            .windows(2)
            .any(|pair| pair[0].relative_path == pair[1].relative_path)
        {
            return Err(StateError::InvalidInventory("duplicate relative paths".into()));
        }
        if self
            .artifacts
            .windows(2)
            .any(|pair| pair[0].relative_path.as_bytes() > pair[1].relative_path.as_bytes())
        {
            return Err(StateError::InvalidInventory(
                "artifacts are not sorted by relative path".into(),
            ));
        }

        let mut version = 0usize;
        let mut usage = 0usize;
        let mut static_shards = [false; STATIC_MESH_SHARD_COUNT];
        let mut terrain_payload = 0usize;
        let mut terrain_dds = 0usize;
        let mut terrain_occlusion = 0usize;
        for entry in &self.artifacts {
            validate_path_grammar(&entry.relative_path, entry.kind)?;
            match entry.kind {
                ArtifactKind::Version => {
                    version += 1;
                    if entry.relative_path != "version" {
                        return Err(StateError::InvalidInventory(
                            "version artifact path must be \"version\"".into(),
                        ));
                    }
                }
                ArtifactKind::Usage => {
                    usage += 1;
                    if entry.relative_path != "statics\\usage.data" {
                        return Err(StateError::InvalidInventory(
                            "usage artifact path must be \"statics\\\\usage.data\"".into(),
                        ));
                    }
                }
                ArtifactKind::StaticShard => {
                    let Some(shard_id) = path::parse_static_shard(&entry.relative_path) else {
                        return Err(StateError::InvalidInventory(format!(
                            "unrecognized static shard path: {}",
                            entry.relative_path
                        )));
                    };
                    static_shards[shard_id] = true;
                }
                ArtifactKind::AtlasDds => {
                    if path::parse_atlas_page(&entry.relative_path).is_none() {
                        return Err(StateError::InvalidInventory(format!(
                            "atlas path is not generator-owned: {}",
                            entry.relative_path
                        )));
                    }
                }
                ArtifactKind::TerrainPayload => {
                    terrain_payload += 1;
                    if entry.relative_path != "terrain.bin" {
                        return Err(StateError::InvalidInventory(
                            "terrain payload path must be \"terrain.bin\"".into(),
                        ));
                    }
                }
                ArtifactKind::TerrainDds => {
                    terrain_dds += 1;
                    if !path::TERRAIN_DDS_PATHS.contains(&entry.relative_path.as_str()) {
                        return Err(StateError::InvalidInventory(format!(
                            "unknown terrain DDS path: {}",
                            entry.relative_path
                        )));
                    }
                }
                ArtifactKind::TerrainOcclusion => {
                    terrain_occlusion += 1;
                    if entry.relative_path != "terrain_occlusion.bin" {
                        return Err(StateError::InvalidInventory(
                            "terrain occlusion path must be \"terrain_occlusion.bin\"".into(),
                        ));
                    }
                }
            }
        }

        if version != 1 || usage != 1 || !static_shards.into_iter().all(std::convert::identity) {
            return Err(StateError::InvalidInventory(
                "inventory must contain exactly one version, usage, and every static shard entry".into(),
            ));
        }

        let terrain_enabled = match self.loaded_state() {
            LoadedGenerationState::Loaded(state) => state.terrain_enabled,
            other => {
                return Err(StateError::InvalidInventory(format!(
                    "generation state is not loadable ({:?})",
                    other.status()
                )));
            }
        };

        if terrain_enabled {
            if terrain_payload != 1 || terrain_dds != 5 || terrain_occlusion != 1 {
                return Err(StateError::InvalidInventory(
                    "terrain-enabled inventory must list terrain.bin, five terrain DDS, and occlusion".into(),
                ));
            }
        } else if terrain_payload != 0 || terrain_dds != 0 || terrain_occlusion != 0 {
            return Err(StateError::InvalidInventory(
                "terrain-disabled inventory must not list terrain artifacts".into(),
            ));
        }

        Ok(())
    }
}

/// Encodes a committed state with the outer framed archive.
pub fn encode(state: &CommittedState) -> Result<Vec<u8>> {
    state.validate_invariants().map_err(|error| anyhow::anyhow!(error))?;
    // Serialize straight into the reserved frame header so the payload is never copied again.
    let mut bytes = rkyv::api::high::to_bytes_in::<_, RkyvError>(state, vec![0; framed_archive::HEADER_LEN])
        .context("failed to serialize committed state")?;
    framed_archive::seal(&mut bytes, COMMITTED_STATE_MAGIC, COMMITTED_STATE_VERSION);
    Ok(bytes)
}

/// Decodes and checksums a committed state, then validates inventory invariants.
///
/// A zeroed magic is how [`invalidate_in_place`] marks the cache absent; it surfaces here as
/// [`StateError::InvalidMagic`] like any other magic mismatch.
pub fn decode(bytes: &[u8]) -> Result<CommittedState, StateError> {
    let payload = framed_archive::decode(bytes, COMMITTED_STATE_MAGIC, COMMITTED_STATE_VERSION)?;
    let state = rkyv::from_bytes::<CommittedState, RkyvError>(payload)
        .map_err(|_| StateError::Corrupt("committed state body failed rkyv decode"))?;
    state.validate_invariants()?;
    Ok(state)
}

/// Loads, decodes, and optionally validates artifacts for a state file on disk.
pub fn load_and_validate(
    distantland_dir: &Path,
    state_path: &Path,
    validation: ArtifactValidation,
) -> Result<CommittedState, StateError> {
    let bytes = fs::read(state_path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            StateError::MissingState(state_path.display().to_string())
        } else {
            StateError::StateIo(format!("{}: {error}", state_path.display()))
        }
    })?;
    let state = decode(&bytes)?;
    validate_artifacts(distantland_dir, &state, validation)?;
    Ok(state)
}

/// Validates every required artifact listed by `state` under `distantland_dir`.
pub fn validate_artifacts(
    distantland_dir: &Path,
    state: &CommittedState,
    validation: ArtifactValidation,
) -> Result<(), StateError> {
    for entry in &state.artifacts {
        validate_artifact(distantland_dir, entry, validation)?;
    }
    validate_static_shard_metadata(distantland_dir, state)?;
    validate_terrain_record_metadata(distantland_dir, state)?;
    Ok(())
}

/// Binds every fixed shard header count to generation state and their checked sum to `usage.data`.
fn validate_static_shard_metadata(distantland_dir: &Path, state: &CommittedState) -> Result<(), StateError> {
    let decoded = state
        .state()
        .ok_or_else(|| StateError::InvalidInventory("generation state is not loadable".into()))?;
    let mut total = 0_u32;

    for shard_id in 0..STATIC_MESH_SHARD_COUNT {
        let relative_path = static_mesh_shard_relative_path(shard_id);
        let artifact = state
            .artifact(&relative_path)
            .ok_or_else(|| StateError::InvalidInventory(format!("missing static shard {relative_path}")))?;
        let path = path::resolve_relative(distantland_dir, &artifact.relative_path);
        let mut file = File::open(&path).map_err(|error| StateError::ArtifactIo(format!("{}: {error}", path.display())))?;
        file.seek(SeekFrom::Start(32))
            .and_then(|_| {
                let mut bytes = [0_u8; 4];
                file.read_exact(&mut bytes)?;
                Ok(bytes)
            })
            .map(u32::from_le_bytes)
            .and_then(|record_count| {
                let expected = decoded.static_mesh_shards[shard_id].record_count;
                if record_count != expected {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("declares {record_count} records but generation state records {expected}"),
                    ));
                }
                total = total.checked_add(record_count).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "static shard record-count sum overflowed u32")
                })?;
                Ok(())
            })
            .map_err(|error| StateError::ArtifactInvalid(format!("{}: {error}", path.display())))?;
    }

    let usage = state
        .artifacts
        .iter()
        .find(|entry| entry.kind == ArtifactKind::Usage)
        .expect("validated inventory has usage.data");
    let usage_path = path::resolve_relative(distantland_dir, &usage.relative_path);
    let mut bytes = [0_u8; 4];
    File::open(&usage_path)
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|error| StateError::ArtifactIo(format!("{}: {error}", usage_path.display())))?;
    let usage_count = u32::from_le_bytes(bytes);
    if total != usage_count {
        return Err(StateError::ArtifactInvalid(format!(
            "{} declares {usage_count} statics but shard headers sum to {total}",
            usage_path.display()
        )));
    }
    Ok(())
}

/// Binds the persisted terrain record-key count to the committed `terrain.bin` header.
///
/// Runs at every validation level: it reads only the bounded header, so the check costs nothing
/// extra on the Routine path. Full validation additionally hashes the whole payload via
/// [`validate_artifact`].
///
/// A missing terrain payload needs no check here. [`CommittedState::validate_invariants`] already
/// requires a terrain-enabled inventory to list one, and [`GenerationState`] validation already
/// requires disabled terrain to carry no record metadata.
fn validate_terrain_record_metadata(distantland_dir: &Path, state: &CommittedState) -> Result<(), StateError> {
    let (Some(decoded), Some(payload)) = (
        state.state(),
        state
            .artifacts
            .iter()
            .find(|entry| entry.kind == ArtifactKind::TerrainPayload),
    ) else {
        return Ok(());
    };
    let path = path::resolve_relative(distantland_dir, &payload.relative_path);
    let header = crate::mge_xe::distant_terrain::load_terrain_header(&path)
        .map_err(|error| StateError::ArtifactInvalid(format!("{}: {error}", path.display())))?;
    if header.mesh_count as usize != decoded.terrain_records.len() {
        return Err(StateError::ArtifactInvalid(format!(
            "{} declares {} meshes but generation state records {} keys",
            path.display(),
            header.mesh_count,
            decoded.terrain_records.len()
        )));
    }
    Ok(())
}

/// Validates one required artifact.
pub fn validate_artifact(
    distantland_dir: &Path,
    entry: &RequiredArtifact,
    validation: ArtifactValidation,
) -> Result<(), StateError> {
    let path = path::resolve_relative(distantland_dir, &entry.relative_path);
    let mut file = File::open(&path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            StateError::MissingArtifact(path.display().to_string())
        } else {
            StateError::ArtifactIo(format!("{}: {error}", path.display()))
        }
    })?;
    let actual_len = file
        .metadata()
        .map_err(|error| StateError::ArtifactIo(format!("{}: {error}", path.display())))?
        .len();
    if actual_len != entry.byte_length {
        return Err(StateError::ArtifactLength(format!(
            "{} has length {actual_len}, expected {}",
            path.display(),
            entry.byte_length
        )));
    }

    validate_header(&mut file, &path, entry.kind)?;

    if validation == ArtifactValidation::Full {
        file.seek(SeekFrom::Start(0))
            .map_err(|error| StateError::ArtifactIo(format!("{}: {error}", path.display())))?;
        let actual = hash_reader(&mut file, &path)?;
        if actual != entry.content_blake3 {
            return Err(StateError::ArtifactHash(path.display().to_string()));
        }
    }
    Ok(())
}

/// Durably invalidates an existing state file by zeroing its first eight magic bytes.
///
/// A missing file is already cache-absent and returns `Ok(false)`.
pub fn invalidate_in_place(path: &Path) -> io::Result<bool> {
    let mut file = match OpenOptions::new().write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&[0; 8])?;
    file.sync_all()?;
    fault::exit_if_requested("after_state_invalidation");
    Ok(true)
}

/// Encodes, writes, and syncs a complete committed state as the publication boundary.
pub fn write_durable(path: &Path, state: &CommittedState) -> Result<[u8; 32]> {
    let bytes = encode(state)?;
    let written = durable::write_durable(path, &bytes, durable::SyncClass::State)?;
    Ok(*written.hash.as_bytes())
}

/// Builds a [`RequiredArtifact`] from a durable write result.
pub fn artifact_from_written(
    kind: ArtifactKind,
    relative_path: impl Into<String>,
    byte_length: u64,
    content_blake3: [u8; 32],
) -> RequiredArtifact {
    RequiredArtifact {
        kind,
        relative_path: relative_path.into(),
        byte_length,
        content_blake3,
    }
}

fn validate_path_grammar(path: &str, kind: ArtifactKind) -> Result<(), StateError> {
    if path.is_empty() || path.starts_with('\\') || path.ends_with('\\') {
        return Err(StateError::InvalidInventory(format!("path grammar rejected {path:?}")));
    }
    if path.contains('/') || path.contains("\\\\") {
        return Err(StateError::InvalidInventory(format!("path grammar rejected {path:?}")));
    }
    if Path::new(path).is_absolute() || path.split('\\').any(|part| part.is_empty() || part == "." || part == "..") {
        return Err(StateError::InvalidInventory(format!("path traversal rejected {path:?}")));
    }
    if !path::common_path_rules(path) {
        return Err(StateError::InvalidInventory(format!("path rules rejected {path:?}")));
    }
    // Kind/path pairing is re-checked in validate_invariants; this only gates absolute/parent paths.
    let _ = kind;
    Ok(())
}

fn validate_header(file: &mut File, path: &Path, kind: ArtifactKind) -> Result<(), StateError> {
    match kind {
        ArtifactKind::Version => {
            let mut bytes = [0_u8; 1];
            file.read_exact(&mut bytes)
                .map_err(|error| StateError::ArtifactIo(format!("{}: {error}", path.display())))?;
            if bytes != [MGE_DL_VERSION] {
                return Err(StateError::ArtifactHeader(format!(
                    "{} does not contain the version-{MGE_DL_VERSION} byte",
                    path.display()
                )));
            }
        }
        ArtifactKind::Usage => {
            // usage.data has no payload magic; its leading count is bound to the shard-header sum
            // after individual artifact validation.
        }
        ArtifactKind::StaticShard => {
            validate_magic_version(file, path, STATIC_MESHES_MAGIC, STATIC_MESHES_VERSION)?;
        }
        ArtifactKind::TerrainPayload => {
            validate_magic_version(file, path, &TERRAIN_FILE_MAGIC, TERRAIN_FILE_VERSION)?;
        }
        ArtifactKind::TerrainOcclusion => {
            validate_magic_version(file, path, &TERRAIN_OCCLUSION_MAGIC, TERRAIN_OCCLUSION_VERSION)?;
        }
        ArtifactKind::AtlasDds | ArtifactKind::TerrainDds => {
            let mut magic = [0_u8; 4];
            file.read_exact(&mut magic)
                .map_err(|error| StateError::ArtifactIo(format!("{}: {error}", path.display())))?;
            if &magic != b"DDS " {
                return Err(StateError::ArtifactHeader(format!(
                    "{} does not start with a DDS signature",
                    path.display()
                )));
            }
            // Require a full 128-byte legacy DDS header (4 magic + 124 DDS_HEADER).
            let len = file
                .metadata()
                .map_err(|error| StateError::ArtifactIo(format!("{}: {error}", path.display())))?
                .len();
            if len < 128 {
                return Err(StateError::ArtifactHeader(format!(
                    "{} is shorter than a legacy DDS header",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

fn validate_magic_version(file: &mut File, path: &Path, magic: &[u8; 8], version: u32) -> Result<(), StateError> {
    let mut header = [0_u8; 12];
    file.read_exact(&mut header)
        .map_err(|error| StateError::ArtifactIo(format!("{}: {error}", path.display())))?;
    if header[..8] != *magic {
        return Err(StateError::ArtifactHeader(format!(
            "{} has unexpected payload magic",
            path.display()
        )));
    }
    let found = u32::from_le_bytes(header[8..12].try_into().expect("four version bytes"));
    if found != version {
        return Err(StateError::ArtifactHeader(format!(
            "{} has payload version {found}, expected {version}",
            path.display()
        )));
    }
    Ok(())
}

fn hash_reader(reader: &mut impl Read, path: &Path) -> Result<[u8; 32], StateError> {
    let mut hasher = blake3::Hasher::new();
    hasher
        .update_reader(reader)
        .map_err(|error| StateError::ArtifactIo(format!("{}: {error}", path.display())))?;
    Ok(*hasher.finalize().as_bytes())
}

#[cfg(test)]
mod tests;
