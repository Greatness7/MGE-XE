//! Atlas planning metrics produced alongside the publication plan.

use serde::{Deserialize, Serialize};

use super::AtlasTextureSet;

/// Family-local atlas allocation path selected for the current run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AtlasFamilyPlanMode {
    /// Serialized family evidence and every committed page carried exactly.
    ExactCarry,
    /// Structurally valid prior slots were reconciled with the current logical groups.
    Reconciled,
    /// No comparable prior existed, so the family was packed independently.
    #[default]
    Fresh,
}

/// Aggregate binding delta for one atlas family.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct AtlasBindingDeltaMetrics {
    /// Whether structurally and inventory-valid prior evidence made the comparison exhaustive.
    pub available: bool,
    /// Logical keys introduced by the current binding relation.
    pub added: usize,
    /// Logical keys removed from the prior binding relation.
    pub removed: usize,
    /// Surviving logical keys whose page or UV-bound bits changed.
    pub changed: usize,
    /// Surviving logical keys whose complete binding tuple remained bitwise identical.
    pub unchanged: usize,
}

/// Allocation, page-lifecycle, binding, and clean-pack-candidate metrics for one atlas family.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct AtlasFamilyMetrics {
    /// Planning mode selected for the family.
    pub plan_mode: AtlasFamilyPlanMode,
    /// Prior physical slots retained by matching.
    pub retained_slots: usize,
    /// New monotonic slots allocated this run.
    pub allocated_slots: usize,
    /// Prior active slots released this run.
    pub freed_slots: usize,
    /// Current groups with surviving prior keys that required a new slot.
    pub relocated_slots: usize,
    /// Retained slots whose persisted provider identity changed.
    pub provider_promoted_slots: usize,
    /// Sum of visible destination areas in texels.
    pub active_area: u64,
    /// Sum of active reservation areas in texels.
    pub reserved_area: u64,
    /// Sum of full retained page areas in texels.
    pub page_area: u64,
    /// Sum of border-excluded retained page areas in texels.
    pub usable_page_area: u64,
    /// Unreserved share of usable page area in parts per million.
    pub fragmentation_ppm: u32,
    /// Pages carried without publication.
    pub carried_pages: usize,
    /// Pages recomposited and published.
    pub built_pages: usize,
    /// Pages appended after existing capacity was exhausted.
    pub appended_pages: usize,
    /// Empty non-trailing pages deliberately retained for future reuse.
    pub retained_empty_pages: usize,
    /// Empty trailing pages removed without renumbering survivors.
    pub truncated_pages: usize,
    /// Exhaustive binding comparison when available.
    pub binding_delta: AtlasBindingDeltaMetrics,
}

/// Report data produced while deciding atlas publication work.
#[derive(Clone, Debug)]
pub struct AtlasPlanMetrics {
    /// Final page counts for the opaque and alpha families.
    pub page_counts: AtlasTextureSet<u32>,
    /// Whether each family retained a layout-compatible digest.
    pub layout_hits: AtlasTextureSet<bool>,
    /// Number of source textures decoded during atlas planning.
    pub decoded_texture_count: usize,
    /// Page carry counts for the opaque and alpha families.
    pub carried_page_counts: AtlasTextureSet<usize>,
    /// Page build counts for the opaque and alpha families.
    pub built_page_counts: AtlasTextureSet<usize>,
    /// Allocator, page lifecycle, candidate, and binding metrics per family.
    pub family_metrics: AtlasTextureSet<AtlasFamilyMetrics>,
    /// Number of reconciled families that changed evidence without page writes.
    pub zero_page_write_reconciliation_count: usize,
    /// Number of atlas pages selected for fresh publication.
    pub dirty_page_count: usize,
    /// Conservative upper estimate for bytes written by atlas publication.
    pub publication_bytes_estimate: u64,
    /// Conservative peak-memory estimate for the no-write planning phase.
    pub planning_peak_bytes: u64,
    /// Conservative peak-memory estimate for streaming publication.
    pub publication_peak_bytes: u64,
}
