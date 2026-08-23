//! Exclusive writer session for the version-16 complete-or-absent store.
//!
//! One session owns the `.writer.lock` for the full decide/publish lifecycle. A valid base is a
//! decoded, Routine-validated `generation_state.bin`. Everything else is cache-absent and forces a
//! clean rebuild. Future versions are refused without mutating generated output.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use hashbrown::HashSet;
use tracing::{field, info, info_span, warn};

use crate::commit::{PublicationWrites, WrittenFile};
use crate::output::{GENERATION_REPORT_NAME, MGE_DL_VERSION, OutputPaths};

use super::capacity;
use super::lock::{ExclusiveLockGuard, LockFile};
use super::path::{self, is_superseded_output_path};
use super::state::{self, ArtifactValidation, CommittedState, RequiredArtifact};

const WRITER_LOCK_TIMEOUT: Duration = Duration::from_secs(2);
const PRESERVED_UNKNOWN_LOG_LIMIT: usize = 16;

/// Bounded observability from post-publication cleanup.
#[derive(Debug, Default, Eq, PartialEq)]
struct PruneReport {
    preserved_unknown_count: usize,
    preserved_unknown_paths: Vec<String>,
}

impl PruneReport {
    fn record_unknown(&mut self, relative_path: String) {
        self.preserved_unknown_count += 1;
        self.preserved_unknown_paths.push(relative_path);
    }

    /// Reduces the recorded paths to a deterministic bounded sample for logging. Call once
    /// after every scan has run; `preserved_unknown_count` keeps the untruncated total.
    fn finish(&mut self) {
        self.preserved_unknown_paths.sort_unstable();
        self.preserved_unknown_paths.truncate(PRESERVED_UNKNOWN_LOG_LIMIT);
    }
}

/// Classification of the store observed under the exclusive writer lock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheClass {
    /// A current-version tree plus a Routine-valid committed state.
    Valid,
    /// Missing, older, invalid, or incomplete. Force a full clean regeneration.
    Absent,
}

/// An exclusively locked output store ready for one generation decision.
pub struct WriterSession {
    paths: OutputPaths,
    _guard: ExclusiveLockGuard,
    base_state: Option<CommittedState>,
    classification: CacheClass,
}

impl WriterSession {
    /// Acquires exclusive ownership and classifies the store under the lock.
    ///
    /// # Errors
    ///
    /// Returns an error when the lock cannot be taken, directories cannot be created, or the tree is a
    /// future version that must not be modified.
    pub fn acquire(paths: OutputPaths) -> Result<Self> {
        fs::create_dir_all(&paths.distantland_dir)
            .with_context(|| format!("failed to create distant-land directory {}", paths.distantland_dir.display()))?;
        let lock = LockFile::new(&paths.writer_lock_path);
        let guard = lock
            .exclusive_for(WRITER_LOCK_TIMEOUT)
            .with_context(|| format!("failed to acquire writer lock {}", lock.path().display()))?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "distant-land output is in use; close Morrowind first and retry ({})",
                    lock.path().display()
                )
            })?;

        match read_version(&paths.version_path)? {
            Some(version) if version > MGE_DL_VERSION => {
                bail!("unsupported distant-land output version {version}");
            }
            Some(MGE_DL_VERSION) => match state::load_and_validate(
                &paths.distantland_dir,
                &paths.generation_state_path,
                ArtifactValidation::Routine,
            ) {
                Ok(base_state) => Ok(Self {
                    paths,
                    _guard: guard,
                    base_state: Some(base_state),
                    classification: CacheClass::Valid,
                }),
                Err(error) => {
                    tracing::info!(%error, "version-16 state is absent or invalid; forcing clean regeneration");
                    Ok(Self {
                        paths,
                        _guard: guard,
                        base_state: None,
                        classification: CacheClass::Absent,
                    })
                }
            },
            // Missing version or older tree: cache absent, never adopted.
            Some(_) | None => Ok(Self {
                paths,
                _guard: guard,
                base_state: None,
                classification: CacheClass::Absent,
            }),
        }
    }

    /// Returns the locked output layout.
    pub fn paths(&self) -> &OutputPaths {
        &self.paths
    }

    /// Returns whether this run has no Routine-valid base to carry from.
    pub fn is_migration(&self) -> bool {
        self.classification == CacheClass::Absent
    }

    /// Returns the store classification observed at acquire.
    pub fn classification(&self) -> CacheClass {
        self.classification
    }

    /// Returns the Routine-validated base state when one exists.
    pub fn base_state(&self) -> Option<&CommittedState> {
        self.base_state.as_ref()
    }

    /// Releases the session without writing state, artifacts, or durability flushes.
    pub fn finish_noop(self) {
        // Dropping the guard releases the exclusive lock. True no-ops must not touch authority.
    }

    /// Checks free space and invalidates an existing valid state before the first dirty write.
    pub fn begin_dirty(&mut self, required_bytes: u64) -> Result<()> {
        capacity::ensure_free_space(&self.paths.distantland_dir, required_bytes)?;
        let span = info_span!("storage.invalidate_state", report = true, invalidated = field::Empty);
        let _guard = span.enter();
        let invalidated = state::invalidate_in_place(&self.paths.generation_state_path)
            .with_context(|| format!("failed to invalidate {}", self.paths.generation_state_path.display()))?;
        span.record("invalidated", invalidated);
        // Once invalidation succeeds the base is no longer authoritative for this run.
        self.base_state = None;
        self.classification = CacheClass::Absent;
        Ok(())
    }

    /// Runs the sync barrier over every deferred payload write, checks the run's write ledger
    /// against the inventory being published, then durably publishes the complete state
    /// (publication commit point) and prunes superseded owned files.
    ///
    /// Taking `writes` as a parameter makes both invariants structural: the state write cannot be
    /// reached without the barrier syncing every buffered payload, nor without every authoritative
    /// write matching its inventory entry. Either failure aborts before the state write, leaving
    /// the state invalidated (cache absent).
    ///
    /// This retains the expected in-memory `CommittedState` but does **not** perform final Routine
    /// validation. Callers must invoke [`Self::validate_published`] after any remaining authorized
    /// work (for example the optional non-authoritative manifest) and before releasing the session.
    ///
    /// Post-publication pruning failures are cleanup warnings: the state flush already published.
    pub fn finish_publish(&mut self, committed: CommittedState, writes: &PublicationWrites) -> Result<()> {
        {
            let pending = writes.take_pending_durables();
            let span = info_span!(
                "storage.sync_barrier",
                report = true,
                file_count = pending.len() as u64,
                durable_bytes = field::Empty,
                sync_us = field::Empty,
            );
            let _guard = span.enter();
            let started = Instant::now();
            let mut durable_bytes = 0_u64;
            // One sequential pass: files the lazy writer already flushed complete in microseconds;
            // still-dirty files pay their cost here without cross-stream fsync interleaving.
            for entry in pending {
                let path = entry.path().to_path_buf();
                let bytes = entry
                    .sync()
                    .with_context(|| format!("sync barrier failed for {}", path.display()))?;
                durable_bytes = durable_bytes.saturating_add(bytes);
            }
            span.record("durable_bytes", durable_bytes);
            span.record("sync_us", u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
        }

        // Every byte written this run is durable now, so the ledger can be checked against the
        // inventory that is about to claim it. A mismatch fails while state is still invalid.
        verify_write_evidence(&self.paths.distantland_dir, &committed.artifacts, &writes.take_written())?;

        {
            let span = info_span!(
                "storage.publish_state",
                report = true,
                artifact_count = committed.artifacts.len() as u64,
            );
            let _guard = span.enter();
            state::write_durable(&self.paths.generation_state_path, &committed)
                .with_context(|| format!("failed to publish {}", self.paths.generation_state_path.display()))?;
        }

        match self.prune_superseded(&committed) {
            Ok(report) if report.preserved_unknown_count != 0 => {
                info!(
                    preserved_unknown_count = report.preserved_unknown_count,
                    preserved_unknown_paths = %report.preserved_unknown_paths.join(", "),
                    "Preserved non-owned files under distantland"
                );
            }
            Ok(_) => {}
            Err(error) => {
                warn!(%error, "post-publication pruning failed; leftover unlisted files are ignored by readers");
            }
        }

        // Retain the expected published state. Final on-disk Routine validation happens separately
        // so optional observability writes can still run under the exclusive session first.
        self.base_state = Some(committed);
        self.classification = CacheClass::Valid;
        Ok(())
    }

    /// Reloads and Routine-validates the on-disk state under the exclusive lock, comparing it with
    /// the expected state retained by [`Self::finish_publish`].
    ///
    /// # Errors
    ///
    /// Returns an error when no expected state is retained, when the on-disk state fails Routine
    /// validation, or when the reloaded state does not equal the expected committed value.
    pub fn validate_published(&mut self) -> Result<CommittedState> {
        let expected = self
            .base_state
            .clone()
            .context("validate_published requires finish_publish to have retained the expected state")?;
        let reloaded = self.validate_routine()?;
        if reloaded != expected {
            bail!("published generation_state.bin does not match the expected committed state");
        }
        self.base_state = Some(reloaded.clone());
        self.classification = CacheClass::Valid;
        Ok(reloaded)
    }

    /// Validates the currently published state under the exclusive lock (Routine mode).
    pub fn validate_routine(&self) -> Result<CommittedState> {
        state::load_and_validate(
            &self.paths.distantland_dir,
            &self.paths.generation_state_path,
            ArtifactValidation::Routine,
        )
        .map_err(|error| anyhow::anyhow!(error))
    }

    fn prune_superseded(&self, committed: &CommittedState) -> Result<PruneReport> {
        let span = info_span!("storage.prune_owned", report = true);
        let _guard = span.enter();

        let keep: HashSet<&str> = committed.artifacts.iter().map(|entry| entry.relative_path.as_str()).collect();
        let mut report = PruneReport::default();

        // Superseded evidence named by `is_superseded_output_path`, epoch-named terrain payloads,
        // and any unlisted generator-owned terrain companions at the distantland root.
        if let Ok(entries) = fs::read_dir(&self.paths.distantland_dir) {
            for entry in entries.flatten() {
                if !entry.file_type().is_ok_and(|file_type| file_type.is_file()) {
                    continue;
                }
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if is_superseded_output_path(&name) {
                    remove_file_if_exists(&entry.path())?;
                    continue;
                }
                if path::TERRAIN_DDS_PATHS.contains(&name.as_ref())
                    || name == "terrain.bin"
                    || name == "terrain_occlusion.bin"
                {
                    if !keep.contains(name.as_ref()) {
                        remove_file_if_exists(&entry.path())?;
                    }
                    continue;
                }
                if keep.contains(name.as_ref())
                    || matches!(name.as_ref(), ".writer.lock" | "generation_state.bin" | "version")
                    || name == GENERATION_REPORT_NAME
                {
                    continue;
                }
                report.record_unknown(name.into_owned());
            }
        }

        // Canonical static payload and epoch statics under statics\.
        if let Ok(entries) = fs::read_dir(&self.paths.statics_dir) {
            for entry in entries.flatten() {
                if !entry.file_type().is_ok_and(|file_type| file_type.is_file()) {
                    continue;
                }
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let relative = format!("statics\\{name}");
                if is_superseded_output_path(&name) {
                    remove_file_if_exists(&entry.path())?;
                    continue;
                }
                if matches!(name.as_ref(), "static_meshes" | "usage.data") {
                    if !keep.contains(relative.as_str()) {
                        remove_file_if_exists(&entry.path())?;
                    }
                } else if !keep.contains(relative.as_str()) {
                    report.record_unknown(relative);
                }
            }
        }

        // Generator-owned atlas pages not in the new inventory; preserve unknown files.
        if let Ok(entries) = fs::read_dir(&self.paths.atlas_texture_dir) {
            for entry in entries.flatten() {
                if !entry.file_type().is_ok_and(|file_type| file_type.is_file()) {
                    continue;
                }
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let relative = format!("statics\\textures\\{name}");
                if path::parse_atlas_page(&relative).is_some() {
                    if !keep.contains(relative.as_str()) {
                        remove_file_if_exists(&entry.path())?;
                    }
                } else if !keep.contains(relative.as_str()) {
                    report.record_unknown(relative);
                }
            }
        }

        report.finish();
        Ok(report)
    }
}

/// Checks that every authoritative artifact this run wrote is described exactly by the state about
/// to be published.
///
/// This proves one direction only: writes are represented accurately by the new inventory. It says
/// nothing about carried entries this run did not write. `ArtifactValidation::Full` is the only
/// mechanism that proves those bodies.
fn verify_write_evidence(distantland_dir: &Path, inventory: &[RequiredArtifact], written: &[WrittenFile]) -> Result<()> {
    for file in written {
        let relative = path::inventory_relative(distantland_dir, &file.path).with_context(|| {
            format!(
                "authoritative write landed outside {}: {}",
                distantland_dir.display(),
                file.path.display()
            )
        })?;
        let entry = inventory
            .iter()
            .find(|entry| entry.relative_path == relative)
            .with_context(|| format!("published state does not list the artifact written at {relative}"))?;
        if entry.byte_length != file.byte_length || entry.content_blake3 != file.content_blake3 {
            bail!(
                "published state describes {relative} as {} bytes with a different digest than the {} bytes written",
                entry.byte_length,
                file.byte_length
            );
        }
    }
    Ok(())
}

fn read_version(path: &Path) -> Result<Option<u8>> {
    match fs::read(path) {
        Ok(bytes) if bytes.len() == 1 => Ok(Some(bytes[0])),
        Ok(bytes) if bytes.is_empty() => Ok(None),
        Ok(bytes) => bail!(
            "{} must contain exactly one version byte, found {}",
            path.display(),
            bytes.len()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::storage::state::{ArtifactKind, artifact_from_written};

    const DISTANTLAND_DIR: &str = r"out\distantland";
    const SHARD_0: &str = r"statics\static_meshes_00";
    const SHARD_1: &str = r"statics\static_meshes_01";

    fn written(relative: &str, bytes: &[u8]) -> WrittenFile {
        WrittenFile {
            path: path::resolve_relative(Path::new(DISTANTLAND_DIR), relative),
            byte_length: bytes.len() as u64,
            content_blake3: *blake3::hash(bytes).as_bytes(),
        }
    }

    fn inventory(entries: &[(&str, &[u8])]) -> Vec<RequiredArtifact> {
        entries
            .iter()
            .map(|(relative, bytes)| {
                artifact_from_written(
                    ArtifactKind::StaticShard,
                    *relative,
                    bytes.len() as u64,
                    *blake3::hash(bytes).as_bytes(),
                )
            })
            .collect()
    }

    #[test]
    fn matching_writes_pass_the_inventory_check() {
        let inventory = inventory(&[(SHARD_0, b"shard bytes"), (SHARD_1, b"other bytes")]);
        let evidence = [written(SHARD_0, b"shard bytes")];
        verify_write_evidence(Path::new(DISTANTLAND_DIR), &inventory, &evidence).unwrap();
    }

    #[test]
    fn a_write_absent_from_the_inventory_is_rejected() {
        let inventory = inventory(&[(SHARD_0, b"shard bytes")]);
        let evidence = [written(SHARD_1, b"other bytes")];
        let error = verify_write_evidence(Path::new(DISTANTLAND_DIR), &inventory, &evidence).unwrap_err();
        assert!(error.to_string().contains(SHARD_1), "{error}");
    }

    #[test]
    fn a_write_whose_inventory_entry_describes_other_bytes_is_rejected() {
        // The carried-artifact overwrite case: the inventory still carries the prior metadata.
        let inventory = inventory(&[(SHARD_0, b"carried bytes")]);
        let evidence = [written(SHARD_0, b"overwritten!!")];
        let error = verify_write_evidence(Path::new(DISTANTLAND_DIR), &inventory, &evidence).unwrap_err();
        assert!(error.to_string().contains("different digest"), "{error}");
    }

    #[test]
    fn a_write_outside_the_output_tree_is_rejected() {
        let inventory = inventory(&[(SHARD_0, b"shard bytes")]);
        let evidence = [WrittenFile {
            path: PathBuf::from("elsewhere").join("static_meshes_00"),
            byte_length: 11,
            content_blake3: *blake3::hash(b"shard bytes").as_bytes(),
        }];
        let error = verify_write_evidence(Path::new(DISTANTLAND_DIR), &inventory, &evidence).unwrap_err();
        assert!(error.to_string().contains("outside"), "{error}");
    }

    #[test]
    fn prune_report_keeps_a_sorted_bounded_path_sample() {
        let mut report = PruneReport::default();
        for index in (0..20).rev() {
            report.record_unknown(format!("unknown_{index:02}.bin"));
        }
        report.finish();

        assert_eq!(report.preserved_unknown_count, 20);
        assert_eq!(report.preserved_unknown_paths.len(), PRESERVED_UNKNOWN_LOG_LIMIT);
        assert_eq!(report.preserved_unknown_paths.first().unwrap(), "unknown_00.bin");
        assert_eq!(report.preserved_unknown_paths.last().unwrap(), "unknown_15.bin");
    }
}
