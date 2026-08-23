//! Diagnostics produced while loading dedicated grass plugins.

/// Non-fatal warning produced by usage loading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageWarning {
    /// Stable machine-readable warning code.
    pub code: String,
    /// Human-readable warning message.
    pub message: String,
}
