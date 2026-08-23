use std::f32::consts::TAU;

use crate::abi::D3dxVector3;
use crate::config::Configuration;

use super::height_field::{EMPTY_HEIGHT, TerrainHeightField};

pub(super) const EMPTY_SLOPE: f32 = f32::NEG_INFINITY;
pub(super) const MIN_DISTANCE: f32 = 1.0;
pub const MAX_HORIZON_RINGS: usize = 32;

/// Tunable horizon-culling parameters derived from host configuration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HorizonParams {
    pub bin_count: usize,
    pub ring_count: usize,
    pub ring_step: f32,
    pub r_near: f32,
    pub bias_z: f32,
    pub bias_obj_z: f32,
    pub march_step: f32,
    /// Selects the max-height-pyramid builder; set once at startup and unchanged mid-run.
    pub hierarchical_march: bool,
}

impl HorizonParams {
    pub fn from_configuration(configuration: Configuration) -> Self {
        let bin_count = configuration.horizon_bins.clamp(64, 4096) as usize;
        let max_range = configuration.horizon_max_range.max(MIN_DISTANCE);
        let mut ring_step = configuration.horizon_ring_step.max(MIN_DISTANCE);
        let mut ring_count = (max_range / ring_step).ceil().max(1.0) as usize;
        if ring_count > MAX_HORIZON_RINGS {
            ring_count = MAX_HORIZON_RINGS;
            ring_step = (max_range / MAX_HORIZON_RINGS as f32).max(MIN_DISTANCE);
        }

        Self {
            bin_count,
            ring_count,
            ring_step,
            r_near: configuration.horizon_near_units.max(0.0),
            bias_z: configuration.horizon_bias_z.max(0.0),
            bias_obj_z: configuration.horizon_object_bias_z.max(0.0),
            march_step: configuration.horizon_sample_spacing.max(MIN_DISTANCE),
            hierarchical_march: configuration.horizon_hierarchical_march,
        }
    }

    /// Returns the number of samples marched from `r_near` through `r_max` for one azimuth bin.
    pub fn samples_per_bin(&self) -> u32 {
        let r_max = self.ring_step * self.ring_count as f32;
        let sample_start = self.r_near.max(MIN_DISTANCE);
        if sample_start > r_max {
            0
        } else {
            ((r_max - sample_start) / self.march_step).floor() as u32 + 1
        }
    }
}

/// Camera-relative, distance-layered horizon table.
#[derive(Clone, Debug)]
pub struct HorizonTable {
    pub eye: D3dxVector3,
    pub bin_count: usize,
    pub ring_count: usize,
    pub ring_step: f32,
    pub r_near: f32,
    pub bias_obj_z: f32,
    /// Terrain height bias baked into `max_slope`; tracked so the per-frame cache invalidates
    /// when only the bias is tuned (the slopes are otherwise unchanged by eye movement).
    pub bias_z: f32,
    pub(crate) max_slope: Vec<f32>,
}

/// Work counters used to compare hierarchical and linear builds in tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct MarchStats {
    pub leaf_samples: u64,
    pub bound_probes: u64,
    pub segments_skipped: u64,
}

/// Small segments are marched directly to avoid bound-probe overhead.
const LEAF_SAMPLES: u32 = 8;

impl HorizonTable {
    /// Builds a layered prefix-max horizon.
    ///
    /// Intentionally infallible: the async worker runs this under `panic = "abort"`, and
    /// `bin_count`/`ring_count` are nonzero by construction (clamped in
    /// [`HorizonParams::from_configuration`]), so indexing and allocation cannot panic.
    pub fn build(field: &TerrainHeightField, eye: D3dxVector3, params: HorizonParams) -> Self {
        Self::build_with_stats(field, eye, params).0
    }

    /// Builds a horizon and returns work counters; the linear path is the equivalence oracle.
    pub(super) fn build_with_stats(
        field: &TerrainHeightField,
        eye: D3dxVector3,
        params: HorizonParams,
    ) -> (Self, MarchStats) {
        if params.hierarchical_march {
            Self::build_hierarchical(field, eye, params)
        } else {
            let table = Self::build_linear(field, eye, params);
            let leaf_samples = params.samples_per_bin() as u64 * params.bin_count as u64;
            let stats = MarchStats {
                leaf_samples,
                bound_probes: 0,
                segments_skipped: 0,
            };
            (table, stats)
        }
    }

    fn new_empty(eye: D3dxVector3, params: HorizonParams) -> Self {
        debug_assert!(
            params.bin_count > 0 && params.ring_count > 0,
            "HorizonParams must have nonzero bin_count/ring_count"
        );
        Self {
            eye,
            bin_count: params.bin_count,
            ring_count: params.ring_count,
            ring_step: params.ring_step,
            r_near: params.r_near,
            bias_obj_z: params.bias_obj_z,
            bias_z: params.bias_z,
            max_slope: vec![EMPTY_SLOPE; params.bin_count * params.ring_count],
        }
    }

    /// Linear march retained as the bit-identical test oracle.
    fn build_linear(field: &TerrainHeightField, eye: D3dxVector3, params: HorizonParams) -> Self {
        let mut table = Self::new_empty(eye, params);
        let r_max = params.ring_step * params.ring_count as f32;
        let sample_start = params.r_near.max(MIN_DISTANCE);
        if sample_start > r_max {
            return table;
        }

        // Keep sample positions identical between linear and hierarchical builds.
        let n = params.samples_per_bin();

        for bin in 0..params.bin_count {
            let theta = (bin as f32 + 0.5) / params.bin_count as f32 * TAU;
            let dir_x = theta.cos();
            let dir_y = theta.sin();
            let mut running = EMPTY_SLOPE;
            for i in 0..n {
                let r = sample_start + i as f32 * params.march_step;
                let x = eye.x + dir_x * r;
                let y = eye.y + dir_y * r;
                if let Some(height) = field.sample_max_z(x, y) {
                    running = running.max((height - params.bias_z - eye.z) / r.max(MIN_DISTANCE));
                }
                let ring = sample_ring(r, params.ring_step, params.ring_count);
                let slot = table.index(bin, ring);
                table.max_slope[slot] = table.max_slope[slot].max(running);
            }
            for ring in 1..params.ring_count {
                let slot = table.index(bin, ring);
                let previous = table.max_slope[table.index(bin, ring - 1)];
                table.max_slope[slot] = table.max_slope[slot].max(previous);
            }
        }
        table
    }

    /// Hierarchical builder that skips segments proven unable to raise the horizon.
    fn build_hierarchical(field: &TerrainHeightField, eye: D3dxVector3, params: HorizonParams) -> (Self, MarchStats) {
        let mut table = Self::new_empty(eye, params);
        let mut stats = MarchStats::default();
        let r_max = params.ring_step * params.ring_count as f32;
        let sample_start = params.r_near.max(MIN_DISTANCE);
        if sample_start > r_max {
            return (table, stats);
        }
        let n = params.samples_per_bin();

        for bin in 0..params.bin_count {
            let theta = (bin as f32 + 0.5) / params.bin_count as f32 * TAU;
            let dir_x = theta.cos();
            let dir_y = theta.sin();
            let mut running = EMPTY_SLOPE;
            Self::march_segment(
                field,
                eye,
                &params,
                dir_x,
                dir_y,
                sample_start,
                0,
                n,
                &mut running,
                &mut table,
                bin,
                &mut stats,
            );
            for ring in 1..params.ring_count {
                let slot = table.index(bin, ring);
                let previous = table.max_slope[table.index(bin, ring - 1)];
                table.max_slope[slot] = table.max_slope[slot].max(previous);
            }
        }
        (table, stats)
    }

    /// Marches or skips one bin's sample range while preserving linear-build order.
    #[allow(clippy::too_many_arguments)]
    fn march_segment(
        field: &TerrainHeightField,
        eye: D3dxVector3,
        params: &HorizonParams,
        dir_x: f32,
        dir_y: f32,
        sample_start: f32,
        lo: u32,
        hi: u32,
        running: &mut f32,
        table: &mut HorizonTable,
        bin: usize,
        stats: &mut MarchStats,
    ) {
        if hi <= lo {
            return;
        }
        if hi - lo > LEAF_SAMPLES {
            stats.bound_probes += 1;
            let bound = segment_upper_bound_slope(field, eye, params, dir_x, dir_y, sample_start, lo, hi);
            if bound <= *running {
                stats.segments_skipped += 1;
                return;
            }
            let mid = lo + (hi - lo) / 2;
            Self::march_segment(
                field,
                eye,
                params,
                dir_x,
                dir_y,
                sample_start,
                lo,
                mid,
                running,
                table,
                bin,
                stats,
            );
            Self::march_segment(
                field,
                eye,
                params,
                dir_x,
                dir_y,
                sample_start,
                mid,
                hi,
                running,
                table,
                bin,
                stats,
            );
            return;
        }
        for i in lo..hi {
            stats.leaf_samples += 1;
            let r = sample_start + i as f32 * params.march_step;
            let x = eye.x + dir_x * r;
            let y = eye.y + dir_y * r;
            if let Some(height) = field.sample_max_z(x, y) {
                *running = running.max((height - params.bias_z - eye.z) / r.max(MIN_DISTANCE));
            }
            let ring = sample_ring(r, params.ring_step, params.ring_count);
            let slot = table.index(bin, ring);
            table.max_slope[slot] = table.max_slope[slot].max(*running);
        }
    }

    pub fn slope_at(&self, bin: usize, ring: usize) -> f32 {
        self.max_slope[self.index(bin, ring)]
    }

    fn index(&self, bin: usize, ring: usize) -> usize {
        bin * self.ring_count + ring
    }
}

/// Conservative upper bound for the slope contributed by samples in `[lo, hi)`.
#[allow(clippy::too_many_arguments)]
fn segment_upper_bound_slope(
    field: &TerrainHeightField,
    eye: D3dxVector3,
    params: &HorizonParams,
    dir_x: f32,
    dir_y: f32,
    sample_start: f32,
    lo: u32,
    hi: u32,
) -> f32 {
    let r_lo = sample_start + lo as f32 * params.march_step;
    let r_hi_incl = sample_start + (hi - 1) as f32 * params.march_step;
    let p_lo_x = eye.x + dir_x * r_lo;
    let p_lo_y = eye.y + dir_y * r_lo;
    let p_hi_x = eye.x + dir_x * r_hi_incl;
    let p_hi_y = eye.y + dir_y * r_hi_incl;

    // Pad the segment by two base cells to cover each sampled 2x2 neighborhood.
    let pad = 2.0 * field.spacing;
    let min_x = p_lo_x.min(p_hi_x) - pad;
    let max_x = p_lo_x.max(p_hi_x) + pad;
    let min_y = p_lo_y.min(p_hi_y) - pad;
    let max_y = p_lo_y.max(p_hi_y) + pad;

    let extent = (max_x - min_x).max(max_y - min_y);
    let level = field.level_for_extent(extent);
    let max_z = field.max_over_aabb(level, min_x, min_y, max_x, max_y);
    if max_z == EMPTY_HEIGHT {
        // No sample in the segment could return a height at all; nothing to raise the horizon.
        return EMPTY_SLOPE;
    }

    let dz = max_z - params.bias_z - eye.z;
    if dz >= 0.0 {
        // Positive slope is largest at the nearest radius.
        dz / r_lo.max(MIN_DISTANCE)
    } else {
        // Negative slope is largest (least negative) at the farthest radius.
        dz / r_hi_incl.max(MIN_DISTANCE)
    }
}

pub(super) fn sample_ring(distance: f32, ring_step: f32, ring_count: usize) -> usize {
    ((distance / ring_step).ceil() as usize).saturating_sub(1).min(ring_count - 1)
}

pub(super) fn normalize_angle(angle: f32) -> f32 {
    angle.rem_euclid(TAU)
}

pub(super) fn angle_span_covering(angles: &mut [f32]) -> (f32, f32, f32) {
    let n = angles.len();
    if n < 2 {
        // Too few angles to bound a span; report a full circle so callers fail open.
        return (0.0, 0.0, TAU);
    }
    angles.sort_by(|left, right| left.total_cmp(right));

    let mut largest_gap = angles[0] + TAU - angles[n - 1];
    let mut gap_after = n - 1;
    for index in 0..n - 1 {
        let gap = angles[index + 1] - angles[index];
        if gap > largest_gap {
            largest_gap = gap;
            gap_after = index;
        }
    }

    (angles[(gap_after + 1) % n], angles[gap_after], TAU - largest_gap)
}

pub(super) fn visit_bins_covering(mut bin_count: usize, start: f32, end: f32, mut visitor: impl FnMut(usize)) {
    if bin_count == 0 {
        return;
    }
    bin_count = bin_count.max(1);
    let bin_width = TAU / bin_count as f32;
    let start = normalize_angle(start);
    let end = normalize_angle(end);
    if start <= end {
        visit_bin_segment(bin_count, bin_width, start, end, &mut visitor);
    } else {
        visit_bin_segment(bin_count, bin_width, start, TAU, &mut visitor);
        visit_bin_segment(bin_count, bin_width, 0.0, end, &mut visitor);
    }
}

fn visit_bin_segment(bin_count: usize, bin_width: f32, start: f32, end: f32, visitor: &mut impl FnMut(usize)) {
    let first = (start / bin_width).floor().clamp(0.0, (bin_count - 1) as f32) as usize;
    let last = (end / bin_width).floor().clamp(0.0, (bin_count - 1) as f32) as usize;
    for bin in first..=last {
        visitor(bin);
    }
}
