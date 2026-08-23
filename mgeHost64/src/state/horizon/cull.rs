use std::f32::consts::TAU;

#[cfg(test)]
use crate::abi::BoundingSphere;
use crate::abi::{D3dxVector2, D3dxVector3};

use super::bounds::HorizonMeshBounds;
use super::table::{HorizonTable, MIN_DISTANCE, angle_span_covering, normalize_angle, sample_ring, visit_bins_covering};

/// Counters gathered while applying one horizon table to one quadtree traversal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HorizonCullStats {
    pub mesh_candidates: usize,
    pub meshes_culled: usize,
    pub obb_fallback_tests: usize,
    pub obb_fallback_culled: usize,
    /// Candidates proven visible by the cheap accept test, skipping the OBB fallback.
    pub early_accepts: usize,
    /// Subtrees pruned at the node level by the horizon check.
    pub nodes_pruned: usize,
}

/// Returns `true` when `sphere` is conservatively hidden by `table`.
#[cfg(test)]
pub fn horizon_culled(sphere: BoundingSphere, table: &HorizonTable) -> bool {
    horizon_culled_disc(sphere.center, sphere.radius, sphere.center.z + sphere.radius, table)
}

/// Like [`horizon_culled`], but uses the true geometry top instead of the sphere apex.
///
/// The circular footprint is conservative, so this only moves safe culls out of the OBB fallback.
#[cfg(test)]
pub fn horizon_culled_capped(sphere: BoundingSphere, max_z: f32, table: &HorizonTable) -> bool {
    horizon_culled_capped_xy((sphere.center.x, sphere.center.y), sphere.radius, max_z, table)
}

/// Like [`horizon_culled_capped`], but takes the horizontal disc explicitly in XY.
pub fn horizon_culled_capped_xy(center_xy: (f32, f32), radius: f32, max_z: f32, table: &HorizonTable) -> bool {
    if !max_z.is_finite() {
        return false;
    }
    if !center_xy.0.is_finite() || !center_xy.1.is_finite() || !radius.is_finite() {
        return false;
    }
    horizon_culled_disc(
        D3dxVector3 {
            x: center_xy.0,
            y: center_xy.1,
            z: 0.0,
        },
        radius,
        max_z,
        table,
    )
}

/// Returns `true` when the axis-aligned XY rectangle from `min_xy` to `max_xy`, top at `max_z`, is
/// conservatively hidden by `table`.
pub fn horizon_culled_rect(min_xy: D3dxVector2, max_xy: D3dxVector2, max_z: f32, table: &HorizonTable) -> bool {
    if !(min_xy.x.is_finite() && min_xy.y.is_finite() && max_xy.x.is_finite() && max_xy.y.is_finite() && max_z.is_finite()) {
        return false;
    }
    let epsilon = 1e-5;
    if !(min_xy.x + epsilon < max_xy.x && min_xy.y + epsilon < max_xy.y) {
        return false;
    }

    let footprint = [
        (min_xy.x, min_xy.y),
        (max_xy.x, min_xy.y),
        (max_xy.x, max_xy.y),
        (min_xy.x, max_xy.y),
    ];

    horizon_culled_footprint(&footprint, max_z, table)
}

/// Returns `true` when a disc is provably above the horizon, so the OBB fallback can be skipped.
///
/// The estimate is conservative and never skips a valid cull.
#[cfg(test)]
pub fn horizon_visible_capped(sphere: BoundingSphere, max_z: f32, table: &HorizonTable) -> bool {
    horizon_visible_capped_xy((sphere.center.x, sphere.center.y), sphere.radius, max_z, table)
}

/// Like [`horizon_visible_capped`], but takes the horizontal disc explicitly in XY.
pub fn horizon_visible_capped_xy(center_xy: (f32, f32), radius: f32, max_z: f32, table: &HorizonTable) -> bool {
    if !max_z.is_finite() {
        return false;
    }
    if !center_xy.0.is_finite() || !center_xy.1.is_finite() || !radius.is_finite() {
        return false;
    }
    if table.bin_count == 0 || table.ring_count == 0 || table.ring_step <= 0.0 {
        return false;
    }

    let dx = center_xy.0 - table.eye.x;
    let dy = center_xy.1 - table.eye.y;
    let horizontal_distance = (dx * dx + dy * dy).sqrt();
    // An eye inside the disc footprint spans every azimuth; cannot bound the span, so defer to OBB.
    if !horizontal_distance.is_finite() || radius >= horizontal_distance {
        return false;
    }

    let d_near = horizontal_distance - radius;
    let d_far = horizontal_distance + radius;
    let top_delta = max_z + table.bias_obj_z - table.eye.z;
    // Lowest (most pessimistic) elevation slope of the object's top: farthest distance shrinks a
    // positive slope, nearest distance deepens a negative one.
    let top_distance = if top_delta >= 0.0 { d_far } else { d_near };
    let min_top_slope = top_delta / top_distance.max(MIN_DISTANCE);

    // Highest horizon the object could face: cumulative-max at its far ring, maxed over its span.
    let far_ring = sample_ring(d_far, table.ring_step, table.ring_count);
    let theta = normalize_angle(dy.atan2(dx));
    let half_span = (radius / horizontal_distance).clamp(0.0, 1.0).asin();
    let mut max_horizon = f32::NEG_INFINITY;
    visit_bins_covering(table.bin_count, theta - half_span, theta + half_span, |bin| {
        max_horizon = max_horizon.max(table.slope_at(bin, far_ring));
    });

    min_top_slope > max_horizon
}

/// Conservatively tests a vertical disc against the horizon.
fn horizon_culled_disc(center: D3dxVector3, radius: f32, top_z: f32, table: &HorizonTable) -> bool {
    if table.bin_count == 0 || table.ring_count == 0 || table.ring_step <= 0.0 {
        return false;
    }

    let dx = center.x - table.eye.x;
    let dy = center.y - table.eye.y;
    let horizontal_distance = (dx * dx + dy * dy).sqrt();
    if !horizontal_distance.is_finite() || radius >= horizontal_distance {
        return false;
    }

    let d_near = horizontal_distance - radius;
    let complete_rings = (d_near / table.ring_step).floor() as isize;
    if complete_rings <= 0 {
        return false;
    }
    let ring = (complete_rings as usize - 1).min(table.ring_count - 1);

    // A below-eye top has its highest slope at the farthest point.
    let d_far = horizontal_distance + radius;
    let top_delta = top_z + table.bias_obj_z - table.eye.z;
    let top_distance = if top_delta >= 0.0 { d_near } else { d_far };
    let top_slope = top_delta / top_distance.max(MIN_DISTANCE);

    let theta = normalize_angle(dy.atan2(dx));
    let half_span = (radius / horizontal_distance).clamp(0.0, 1.0).asin();
    let mut min_horizon = f32::INFINITY;
    visit_bins_covering(table.bin_count, theta - half_span, theta + half_span, |bin| {
        min_horizon = min_horizon.min(table.slope_at(bin, ring));
    });

    top_slope < min_horizon
}

/// Returns `true` when `bounds` is conservatively hidden by `table`.
pub fn horizon_culled_bounds(bounds: &HorizonMeshBounds, table: &HorizonTable) -> bool {
    let footprint = &bounds.footprint_xy[..bounds.vertex_count as usize];
    horizon_culled_footprint(footprint, bounds.max_z, table)
}

fn horizon_culled_footprint(footprint: &[(f32, f32)], max_z: f32, table: &HorizonTable) -> bool {
    if table.bin_count == 0 || table.ring_count == 0 || table.ring_step <= 0.0 {
        return false;
    }

    if footprint.len() < 3 {
        // Degenerate footprint: nothing to occlude against, so fail open.
        return false;
    }

    let mut projected = [(0.0_f32, 0.0_f32); 6];
    let mut angles = [0.0_f32; 6];
    let mut max_distance_sq = 0.0_f32;
    for ((slot, angle), &(x, y)) in projected.iter_mut().zip(angles.iter_mut()).zip(footprint) {
        let dx = x - table.eye.x;
        let dy = y - table.eye.y;
        let distance_sq = dx * dx + dy * dy;
        if !distance_sq.is_finite() {
            return false;
        }

        *slot = (dx, dy);
        max_distance_sq = max_distance_sq.max(distance_sq);
        *angle = normalize_angle(dy.atan2(dx));
    }

    let n = footprint.len();
    let min_distance_sq = min_distance_sq_to_polygon_edges(&projected[..n]);
    if !min_distance_sq.is_finite() || min_distance_sq <= MIN_DISTANCE * MIN_DISTANCE {
        return false;
    }
    let min_distance = min_distance_sq.sqrt();
    let max_distance = max_distance_sq.sqrt();

    let complete_rings = (min_distance / table.ring_step).floor() as isize;
    if complete_rings <= 0 {
        return false;
    }
    let ring = (complete_rings as usize - 1).min(table.ring_count - 1);

    // Every footprint vertex shares the box's top, so the highest object point is `max_z`.
    let max_top_delta = max_z + table.bias_obj_z - table.eye.z;
    let top_distance = if max_top_delta >= 0.0 { min_distance } else { max_distance };
    let top_slope = max_top_delta / top_distance.max(MIN_DISTANCE);

    let (start, end, span) = angle_span_covering(&mut angles[..n]);
    if span >= TAU * 0.5 {
        return false;
    }

    let mut min_horizon = f32::INFINITY;
    visit_bins_covering(table.bin_count, start, end, |bin| {
        min_horizon = min_horizon.min(table.slope_at(bin, ring));
    });

    top_slope < min_horizon
}

/// Minimum squared distance from the origin to a convex polygon edge.
pub(super) fn min_distance_sq_to_polygon_edges(points: &[(f32, f32)]) -> f32 {
    let mut min_distance_sq = f32::INFINITY;
    for index in 0..points.len() {
        let a = points[index];
        let b = points[(index + 1) % points.len()];
        min_distance_sq = min_distance_sq.min(distance_sq_to_projected_segment(a, b));
    }
    min_distance_sq
}

fn distance_sq_to_projected_segment(a: (f32, f32), b: (f32, f32)) -> f32 {
    let vx = b.0 - a.0;
    let vy = b.1 - a.1;
    let length_sq = vx * vx + vy * vy;
    if length_sq <= f32::EPSILON {
        return a.0 * a.0 + a.1 * a.1;
    }

    let t = (-(a.0 * vx + a.1 * vy) / length_sq).clamp(0.0, 1.0);
    let closest_x = a.0 + vx * t;
    let closest_y = a.1 + vy * t;
    closest_x * closest_x + closest_y * closest_y
}
