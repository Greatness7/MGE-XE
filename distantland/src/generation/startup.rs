//! Startup-time output validation and single-writer generation helpers.
//!
//! The public entry points are [`check_output_status`] (read-only inspection) and
//! [`ensure_generated`] (lock → classify → one-call ensure/continue → under-session validation).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::Vfs;
use crate::vfs::VfsLoadOptions;

use super::{GenerationJob, GenerationReport, MGE_DL_VERSION, OutputContractValidation, OutputPaths, ProgressReporter};

mod status;
mod types;

pub use status::check_output_status;

pub use types::*;

/// Validates `job` and resolves its output layout.
///
/// This is the single job-to-paths resolution used by both startup entry points. `output_root`
/// falls back to the VFS data directory, so the VFS is loaded only when the job does not name
/// one -- an explicit `output_root` resolves without a Morrowind install present. Generation
/// itself still loads the full VFS (`generation.rs`), so this is not a weakened guard.
fn load_output_paths(job: &GenerationJob) -> Result<OutputPaths> {
    job.validate_for_generation()?;
    if let Some(output_root) = &job.output_root {
        return Ok(OutputPaths::new(output_root));
    }
    let options = VfsLoadOptions {
        morrowind_ini: job.morrowind_ini.clone(),
        data_dirs: job.data_dirs.clone(),
        plugins: job.plugins.clone(),
    };
    let vfs = Vfs::load_metadata_only(&options).context("Failed to initialize VFS for distant-land output paths")?;
    Ok(OutputPaths::new(job.resolved_output_root(&vfs)))
}

/// Performs a non-mutating best-effort probe for a live shared output session.
///
/// A `false` result is advisory only; callers must still use [`ensure_generated`], which
/// acquires the authoritative exclusive lock before publication.
///
/// # Errors
///
/// Returns an error if an existing lock file cannot be opened or queried.
pub fn output_has_live_session(output_root: &Path) -> Result<bool> {
    let paths = OutputPaths::new(output_root);
    if !paths.writer_lock_path.exists() {
        return Ok(false);
    }
    let lock = super::storage::lock::LockFile::new(&paths.writer_lock_path);
    Ok(lock.exclusive_for(Duration::ZERO)?.is_none())
}

#[cfg(test)]
use status::check_output_paths_status;

/// Ensures that valid distant land output exists for the given job.
///
/// Acquires one exclusive writer session, classifies the store, then runs a single locked
/// ensure/continue generation path: the front half reaches the unit-diff boundary once
/// and either returns already-current or continues with the same live locals into planning and
/// publication. Final on-disk Routine validation happens under that same exclusive session before
/// release. Independent callers still use [`check_output_status`] for read-only status.
///
/// A successful [`GenerationOutcome::Generated`] reports the pre-run classification in
/// `previous_status` (missing / invalid / stale / valid-before-force), not a post-publish re-scan.
///
/// Source files are not locked; the published tree represents the exact source snapshot consumed by
/// the single front half of this run. Concurrent mid-run source mutation is not a supported
/// guarantee of this path.
///
/// # Errors
///
/// Returns an error if status checking, generation, or validation fails, or if a future version
/// refuses mutation.
pub fn ensure_generated(job: &GenerationJob, reporter: &mut dyn ProgressReporter) -> Result<GenerationOutcome> {
    // Resolve only enough of the job to identify output_root, then take the lock before identity work.
    let output_paths = load_output_paths(job)?;
    let writer = super::storage::authority::WriterSession::acquire(output_paths)?;

    let previous_status = status::previous_status_under_lock(&writer)?;

    // Eligible for an early AlreadyValid return only when storage is Routine-valid and
    // force_rebuild is off. Migration/absent bases continue into ensure mode.
    let return_if_current = !job.settings.force_rebuild && !writer.is_migration() && !previous_status.should_generate();

    match super::ensure_with_output_root(job, writer, return_if_current, reporter)? {
        super::GenerationRunResult::AlreadyCurrent => Ok(GenerationOutcome::AlreadyValid { status: previous_status }),
        super::GenerationRunResult::Published(validated) => {
            if !validated.report.report.mge_xe_contract.complete {
                bail!(
                    "Published distant land output has an incomplete MGE-XE contract: {:#?}",
                    validated.report.report.mge_xe_contract.issues
                );
            }
            // When we were eligible for AlreadyValid but continued, the unit diff was dirty:
            // report Stale. force_rebuild / migration / advisory-stale keep the pre-run
            // classification unchanged.
            let previous_status = if return_if_current {
                status::refine_previous_status_after_dirty_ensure(previous_status, &validated.report)
            } else {
                previous_status
            };
            Ok(GenerationOutcome::Generated {
                previous_status,
                report: validated.report,
            })
        }
        super::GenerationRunResult::Inspected(_) => {
            unreachable!("ensure mode returned an independent inspection result")
        }
    }
}

#[cfg(test)]
mod tests;
