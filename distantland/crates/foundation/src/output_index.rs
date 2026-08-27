//! Read-only runtime authority for the complete-or-absent output tree.
//!
//! Opening a snapshot acquires a shared storage lock, revalidates `generation_state.bin` under that
//! lock, and keeps the guard alive for the lifetime of the returned snapshot. Fixed payload
//! paths are used exclusively; there is no descriptor, generation number, or epoch path.

use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::output::{MGE_DL_VERSION, OutputPaths};
use crate::storage::lock::{LockFile, SharedLockGuard};
use crate::storage::path;
use crate::storage::state::{self, CommittedState, StateError};

/// Filesystem validation cost requested by an output reader.
///
/// This is the storage authority's own validation tier, re-exported under the reader's name so
/// hosts depend on this module rather than on storage internals while the crate keeps exactly one
/// validation-tier type.
pub use crate::storage::state::ArtifactValidation as OutputValidation;

/// Stable failure returned by the versioned output reader.
#[derive(Debug)]
pub struct OutputIndexError {
    code: &'static str,
    message: String,
}

impl OutputIndexError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Returns the stable machine-readable reason code.
    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for OutputIndexError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for OutputIndexError {}

/// Lock-pinned, version-keyed output selection backed by `generation_state.bin`.
pub struct OutputSnapshot {
    state: CommittedState,
    paths: OutputPaths,
    terrain_available: bool,
    atlas_pages: Vec<PathBuf>,
    _guard: SharedLockGuard,
}

impl fmt::Debug for OutputSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutputSnapshot")
            .field("version", &MGE_DL_VERSION)
            .field("paths", &self.paths)
            .field("terrain_available", &self.terrain_available)
            .field("atlas_pages", &self.atlas_pages)
            .field("artifact_count", &self.state.artifacts.len())
            .finish_non_exhaustive()
    }
}

impl OutputSnapshot {
    /// Returns the selected output version.
    ///
    /// Always [`MGE_DL_VERSION`]: the pre-lock gate and the post-lock recheck both reject every
    /// other version, so an opened snapshot cannot hold anything else.
    pub fn version(&self) -> u8 {
        MGE_DL_VERSION
    }

    /// Returns the decoded committed state while this snapshot keeps its shared pin alive.
    pub fn committed_state(&self) -> &CommittedState {
        &self.state
    }

    /// Returns the selected fixed filesystem paths.
    pub fn paths(&self) -> &OutputPaths {
        &self.paths
    }

    /// Returns whether a terrain payload belongs to the selected output.
    pub fn terrain_available(&self) -> bool {
        self.terrain_available
    }

    /// Returns the atlas pages recorded in the committed inventory.
    pub fn atlas_pages(&self) -> &[PathBuf] {
        &self.atlas_pages
    }
}

/// Opens an output snapshot and keeps its shared lock pinned.
///
/// Waits up to `lock_timeout` for the shared lock, then re-reads and validates the state while
/// holding it. Every other version is rejected. An older tree must be replaced by a clean rebuild
/// before it can be read.
///
/// # Errors
///
/// Returns an error for an unsupported version, lock timeout, corrupt/missing state, invalid
/// inventory, missing or malformed required artifacts, or a requested full-hash mismatch.
pub fn open_output_snapshot(
    output_root: &Path,
    lock_timeout: Duration,
    validation: OutputValidation,
) -> Result<OutputSnapshot, OutputIndexError> {
    let paths = OutputPaths::new(output_root);
    match read_version(&paths.version_path)? {
        MGE_DL_VERSION => open_state_snapshot(paths, lock_timeout, validation),
        version if version > MGE_DL_VERSION => Err(OutputIndexError::new(
            "future_output_version",
            format!("unsupported distant-land output version {version}"),
        )),
        version => Err(OutputIndexError::new(
            "incompatible_older_version",
            format!("distant-land output version {version} must be rebuilt for version {MGE_DL_VERSION}"),
        )),
    }
}

fn open_state_snapshot(
    paths: OutputPaths,
    lock_timeout: Duration,
    validation: OutputValidation,
) -> Result<OutputSnapshot, OutputIndexError> {
    let lock = LockFile::new(&paths.writer_lock_path);
    let guard = lock
        .shared_for(lock_timeout)
        .map_err(|error| io_error("lock_io", &paths.writer_lock_path, error))?
        .ok_or_else(|| OutputIndexError::new("lock_timeout", "timed out waiting for the shared output lock"))?;

    let version = read_version(&paths.version_path)?;
    if version != MGE_DL_VERSION {
        return Err(OutputIndexError::new(
            "version_changed_while_pinning",
            format!("expected version {MGE_DL_VERSION} under the shared lock, found {version}"),
        ));
    }

    let state =
        state::load_and_validate(&paths.distantland_dir, &paths.generation_state_path, validation).map_err(state_error)?;
    let terrain_available = state
        .terrain_enabled()
        .map_err(|error| OutputIndexError::new("corrupt_state", error.to_string()))?;
    let atlas_pages = state
        .atlas_relative_paths()
        .map(|relative| path::resolve_relative(&paths.distantland_dir, relative))
        .collect();

    Ok(OutputSnapshot {
        state,
        paths,
        terrain_available,
        atlas_pages,
        _guard: guard,
    })
}

fn read_version(path: &Path) -> Result<u8, OutputIndexError> {
    let file = fs::File::open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            OutputIndexError::new("missing_version", format!("{} is missing", path.display()))
        } else {
            io_error("version_io", path, error)
        }
    })?;
    // The marker is one byte by contract. Cap the read at two so a truncated or corrupt file is
    // still proven wrong-length without buffering whatever size it happens to be.
    let mut bytes = Vec::with_capacity(2);
    file.take(2)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("version_io", path, error))?;
    if bytes.len() != 1 {
        return Err(OutputIndexError::new(
            "malformed_version",
            format!("{} must contain exactly one byte", path.display()),
        ));
    }
    Ok(bytes[0])
}

/// Lifts a state failure into the reader's error, keeping the storage authority's reason code.
fn state_error(error: StateError) -> OutputIndexError {
    OutputIndexError::new(error.code(), error.to_string())
}

fn io_error(code: &'static str, path: &Path, error: std::io::Error) -> OutputIndexError {
    OutputIndexError::new(code, format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests;
