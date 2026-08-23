//! Advisory generation-report types and serialization helpers.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::UnitsReport;

use crate::TraceSummary;

use super::contract::{MGE_DL_VERSION, OutputContractValidation, OutputPaths};
use crate::generation::cache::fingerprint_generation_request;
use crate::generation::job::{GenerationJob, GenerationSettings};
use crate::generation::metrics::{CacheMetadata, GenerationMetrics, OutputWriteDecision};

/// Generation-report schema version written by this crate.
pub const GENERATION_REPORT_VERSION: u32 = 3;

/// Non-fatal generation warning recorded in logs and the advisory report.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct GenerationWarning {
    pub code: String,
    pub message: String,
}

impl From<crate::usage::UsageWarning> for GenerationWarning {
    fn from(warning: crate::usage::UsageWarning) -> Self {
        Self {
            code: warning.code,
            message: warning.message,
        }
    }
}

/// Human-facing advisory report describing one generated distant-land output set.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GenerationReportData {
    /// Report schema version.
    pub report_version: u32,
    pub generator_version: String,
    /// MGE-XE distant-land format version written into `distantland\version`.
    pub mge_xe_distant_land_version: u8,
    /// Optional `Morrowind.ini` used to resolve the VFS.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub morrowind_ini: Option<PathBuf>,
    pub output_root: PathBuf,
    /// Fingerprint of the normalized generation request.
    pub request_fingerprint: String,
    /// Content identity of the resolved active load order.
    #[serde(default)]
    pub load_order_identity: String,
    pub settings: GenerationSettings,
    /// Summary of reportable tracing spans captured during generation.
    pub trace_summary: TraceSummary,
    /// Cache-hit summary for the run.
    pub cache: CacheMetadata,
    /// Aggregate runtime metrics for the run.
    pub metrics: GenerationMetrics,
    /// Non-authoritative unit dirty sets.
    #[serde(default)]
    pub units: UnitsReport,
    /// Concise human-readable explanation derived from `units` for a rebuilding run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebuild_cause: Option<String>,
    /// Non-fatal warnings emitted during generation.
    pub warnings: Vec<GenerationWarning>,
    /// Full validation details for the current output tree.
    pub mge_xe_contract: OutputContractValidation,
    /// Convenience mirror of `mge_xe_contract.complete`.
    pub mge_xe_contract_complete: bool,
}

#[derive(Clone, Debug)]
pub struct GenerationReport {
    pub output_root: PathBuf,
    pub report_path: PathBuf,
    /// Whether `report_path` was actually (re)written by this run.
    ///
    /// A true no-op run builds `report` in memory for callers but never touches disk, so
    /// `report_path` may still point at a stale file left by an earlier run in that case.
    pub report_written: OutputWriteDecision,
    pub report: GenerationReportData,
    pub warnings: Vec<GenerationWarning>,
}

/// Builds the advisory generation report from generation inputs, metrics, and traced stages.
///
/// # Errors
///
/// Returns an error if output enumeration or request fingerprinting fails.
pub(crate) fn build_generation_report_data(
    job: &GenerationJob,
    output_paths: &OutputPaths,
    trace_summary: TraceSummary,
    cache: &CacheMetadata,
    metrics: &GenerationMetrics,
    warnings: &[GenerationWarning],
    load_order_identity: &str,
    units: &UnitsReport,
    rebuild_cause: Option<&str>,
) -> Result<GenerationReportData> {
    let contract = output_paths.validate_mge_xe_contract();
    Ok(GenerationReportData {
        report_version: GENERATION_REPORT_VERSION,
        generator_version: env!("CARGO_PKG_VERSION").to_string(),
        mge_xe_distant_land_version: MGE_DL_VERSION,
        morrowind_ini: job.morrowind_ini.clone(),
        output_root: output_paths.output_root.clone(),
        request_fingerprint: fingerprint_generation_request(job).context("Failed to fingerprint generation request")?,
        load_order_identity: load_order_identity.to_string(),
        settings: job.settings.clone(),
        trace_summary,
        cache: cache.clone(),
        metrics: metrics.clone(),
        units: units.clone(),
        rebuild_cause: rebuild_cause.map(str::to_owned),
        warnings: warnings.to_vec(),
        mge_xe_contract_complete: contract.complete,
        mge_xe_contract: contract,
    })
}

/// Writes `distantland\generation_report.toml`.
///
/// # Errors
///
/// Returns an error if the report cannot be serialized or written.
pub(crate) fn write_generation_report_data(
    output_paths: &OutputPaths,
    report: &GenerationReportData,
) -> Result<OutputWriteDecision> {
    let bytes = toml::to_string_pretty(report)?.into_bytes();
    Ok(crate::generation::cache::write_plain_bytes_if_changed(
        &output_paths.generation_report_path,
        &bytes,
    )?)
}

/// Reads an existing `generation_report.toml` written by this crate version.
///
/// The report is advisory: an output tree stays valid whether or not this succeeds, so callers
/// treat failure as "no report available" rather than as a generation error. Only the current
/// schema is accepted; reports from an older `GENERATION_REPORT_VERSION` fail to decode and the
/// output should be regenerated.
///
/// # Errors
///
/// Returns an error if the file cannot be read or does not decode as the current schema.
pub fn load_generation_report_data(path: &Path) -> Result<GenerationReportData> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("Failed to read generation report {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("Failed to decode generation report {}", path.display()))
}
