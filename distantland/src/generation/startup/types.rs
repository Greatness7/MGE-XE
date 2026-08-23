use super::*;

/// High-level classification for the current distant-land output tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStatusKind {
    /// Existing output appears complete and matches the requested job.
    Valid,
    /// Required output files are missing.
    Missing,
    /// Output exists but does not match the requested job or load order.
    Stale,
    /// Output exists but fails structural validation.
    Invalid,
}

/// One issue contributing to an `OutputStatus`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OutputStatusIssue {
    /// Stable issue code.
    pub code: String,
    /// Human-readable explanation of the issue.
    pub message: String,
}

/// Detailed information attached to an `OutputStatus`.
#[derive(Clone, Debug, Serialize)]
pub struct OutputStatusDetails {
    pub output_root: PathBuf,
    pub generation_report_path: PathBuf,
    pub validation: OutputContractValidation,
    pub issues: Vec<OutputStatusIssue>,
}

/// Current status of the output tree for a generation job.
///
/// Every state carries the same details, so the state is a field rather than a variant; the
/// per-state difference lives in `details.issues`.
#[derive(Clone, Debug, Serialize)]
pub struct OutputStatus {
    #[serde(rename = "status")]
    pub kind: OutputStatusKind,
    /// Findings behind `kind`, including the issues that produced it.
    pub details: OutputStatusDetails,
}

/// Result of `ensure_generated`.
#[derive(Clone, Debug)]
pub enum GenerationOutcome {
    /// Existing output was already valid, so no generation work was needed.
    AlreadyValid { status: OutputStatus },
    /// New output was generated and published.
    Generated {
        /// Status observed before regeneration began (missing / invalid / stale / valid-before-force),
        /// not a post-publish re-scan of the newly written tree.
        previous_status: OutputStatus,
        report: GenerationReport,
    },
}

impl OutputStatus {
    pub const fn new(kind: OutputStatusKind, details: OutputStatusDetails) -> Self {
        Self { kind, details }
    }

    pub const fn kind(&self) -> OutputStatusKind {
        self.kind
    }

    /// Returns whether generation should run to satisfy this status.
    pub const fn should_generate(&self) -> bool {
        !matches!(self.kind, OutputStatusKind::Valid)
    }

    pub const fn details(&self) -> &OutputStatusDetails {
        &self.details
    }

    pub fn format_issues(&self) -> String {
        format_output_status_issues(self)
    }
}

/// Formats an `OutputStatus` into a compact issue summary suitable for log output.
///
/// Each issue is rendered as `code: message`, semicolon-separated, prefixed with the
/// status kind. Returns `"<kind>: no issues"` when the status has no issues.
pub fn format_output_status_issues(status: &OutputStatus) -> String {
    let details = &status.details;
    if details.issues.is_empty() {
        return format!("{}: no issues", output_status_kind_label(status.kind));
    }

    let mut formatted = format!("{}:", output_status_kind_label(status.kind));
    for (index, issue) in details.issues.iter().enumerate() {
        if index > 0 {
            formatted.push_str("; ");
        } else {
            formatted.push(' ');
        }
        formatted.push_str(&issue.code);
        formatted.push_str(": ");
        formatted.push_str(&issue.message);
    }
    formatted
}

const fn output_status_kind_label(kind: OutputStatusKind) -> &'static str {
    match kind {
        OutputStatusKind::Valid => "valid",
        OutputStatusKind::Missing => "missing",
        OutputStatusKind::Stale => "stale",
        OutputStatusKind::Invalid => "invalid",
    }
}
