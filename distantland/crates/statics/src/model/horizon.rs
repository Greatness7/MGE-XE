use glam::Vec2;
use half::f16;

use super::Vertex;
use crate::mge_xe::distant_statics::{BoundingBox, HORIZON_FOOTPRINT_MAX_VERTS, HorizonFootprint};

const GEOMETRY_EPSILON: f32 = 1.0e-4;

pub(crate) fn horizon_footprint_from_vertices(vertices: &[Vertex], aabb: BoundingBox) -> HorizonFootprint {
    if vertices.len() < 3 || !aabb_xy_is_valid(aabb) {
        return HorizonFootprint::default();
    }

    let mut true_points = Vec::with_capacity(vertices.len());
    let mut max_z = f32::NEG_INFINITY;
    for vertex in vertices {
        let position = vertex.position;
        if !position.x.is_finite() || !position.y.is_finite() || !position.z.is_finite() {
            return HorizonFootprint::default();
        }

        let point = position.truncate();
        let packed_point = packed_xy(point);
        let packed_z = f16::from_f32(position.z).to_f32();
        if !packed_point.is_finite() || !packed_z.is_finite() {
            return HorizonFootprint::default();
        }

        true_points.push(point);
        max_z = max_z.max(position.z).max(packed_z);
    }

    let true_hull = convex_hull_xy(true_points);
    if true_hull.len() >= 3 && polygon_area(&true_hull) > area_epsilon(aabb) {
        let mut hull_points = Vec::with_capacity(true_hull.len() * 2);
        hull_points.extend_from_slice(&true_hull);
        hull_points.extend(true_hull.iter().copied().map(packed_xy));

        let hull = convex_hull_xy(hull_points);
        let footprint = horizon_footprint_from_hull(&hull, max_z, aabb);
        if footprint.vertex_count > 0 && footprint_contains_all_packed_vertices(footprint, vertices) {
            return footprint;
        }
    }

    let hull = expanded_hull_from_vertices(vertices);
    horizon_footprint_from_hull(&hull, max_z, aabb)
}

fn aabb_xy_is_valid(aabb: BoundingBox) -> bool {
    aabb.min.x.is_finite()
        && aabb.min.y.is_finite()
        && aabb.max.x.is_finite()
        && aabb.max.y.is_finite()
        && aabb.max.x - aabb.min.x > GEOMETRY_EPSILON
        && aabb.max.y - aabb.min.y > GEOMETRY_EPSILON
}

fn enclosing_aabb_footprint(points: &[Vec2], max_z: f32) -> Option<HorizonFootprint> {
    let (min, max) = point_bounds(points)?;
    if !max_z.is_finite() || max.x - min.x <= GEOMETRY_EPSILON || max.y - min.y <= GEOMETRY_EPSILON {
        return None;
    }

    Some(footprint_from_polygon(
        &[
            Vec2::new(min.x, min.y),
            Vec2::new(max.x, min.y),
            Vec2::new(max.x, max.y),
            Vec2::new(min.x, max.y),
        ],
        max_z,
    ))
}

fn point_bounds(points: &[Vec2]) -> Option<(Vec2, Vec2)> {
    let mut iter = points.iter().copied();
    let first = iter.next()?;
    if !first.is_finite() {
        return None;
    }

    let mut min = first;
    let mut max = first;
    for point in iter {
        if !point.is_finite() {
            return None;
        }
        min = min.min(point);
        max = max.max(point);
    }

    Some((min, max))
}

fn footprint_from_polygon(polygon: &[Vec2], max_z: f32) -> HorizonFootprint {
    let mut footprint = HorizonFootprint {
        max_z,
        vertex_count: polygon.len() as u8,
        ..HorizonFootprint::default()
    };

    for (slot, point) in footprint.footprint_xy.iter_mut().zip(polygon.iter()) {
        *slot = [point.x, point.y];
    }

    footprint
}

fn packed_xy(point: Vec2) -> Vec2 {
    Vec2::new(f16::from_f32(point.x).to_f32(), f16::from_f32(point.y).to_f32())
}

fn expanded_hull_from_vertices(vertices: &[Vertex]) -> Vec<Vec2> {
    let mut points = Vec::with_capacity(vertices.len() * 2);
    for vertex in vertices {
        let point = vertex.position.truncate();
        points.push(point);
        points.push(packed_xy(point));
    }
    convex_hull_xy(points)
}

fn horizon_footprint_from_hull(hull: &[Vec2], max_z: f32, aabb: BoundingBox) -> HorizonFootprint {
    if hull.len() < 3 || polygon_area(hull) <= area_epsilon(aabb) {
        return HorizonFootprint::default();
    }

    let fallback_footprint = || enclosing_aabb_footprint(hull, max_z).unwrap_or_default();

    let candidate = if hull.len() > HORIZON_FOOTPRINT_MAX_VERTS {
        match cap_hull_to_max_vertices(hull, HORIZON_FOOTPRINT_MAX_VERTS) {
            Some(capped) => capped,
            None => return fallback_footprint(),
        }
    } else {
        hull.to_vec()
    };

    if candidate_is_useful_footprint(&candidate, hull) {
        footprint_from_polygon(&candidate, max_z)
    } else {
        fallback_footprint()
    }
}

fn footprint_contains_all_packed_vertices(footprint: HorizonFootprint, vertices: &[Vertex]) -> bool {
    let count = footprint.vertex_count as usize;
    if !(3..=HORIZON_FOOTPRINT_MAX_VERTS).contains(&count) {
        return false;
    }

    let mut polygon = [Vec2::ZERO; HORIZON_FOOTPRINT_MAX_VERTS];
    for (index, point) in footprint.footprint_xy[..count].iter().enumerate() {
        polygon[index] = Vec2::new(point[0], point[1]);
    }

    vertices
        .iter()
        .map(|vertex| packed_xy(vertex.position.truncate()))
        .all(|point| point_in_convex_polygon(point, &polygon[..count]))
}

fn convex_hull_xy(mut points: Vec<Vec2>) -> Vec<Vec2> {
    prefilter_hull_points(&mut points);
    monotone_chain_hull(points)
}

fn prefilter_hull_points(points: &mut Vec<Vec2>) {
    if points.len() <= 8 {
        return;
    }

    let prefilter_hull = monotone_chain_hull(extreme_hull_points(points));
    if prefilter_hull.len() < 3 {
        return;
    }

    points.retain(|&point| !point_strictly_inside_convex_polygon(point, &prefilter_hull));
}

fn extreme_hull_points(points: &[Vec2]) -> Vec<Vec2> {
    debug_assert!(!points.is_empty());

    let mut min_x = points[0];
    let mut max_x = points[0];
    let mut min_y = points[0];
    let mut max_y = points[0];
    let mut min_sum = points[0];
    let mut max_sum = points[0];
    let mut min_diff = points[0];
    let mut max_diff = points[0];
    let mut min_sum_value = points[0].x + points[0].y;
    let mut max_sum_value = min_sum_value;
    let mut min_diff_value = points[0].x - points[0].y;
    let mut max_diff_value = min_diff_value;

    for &point in &points[1..] {
        if point.x < min_x.x {
            min_x = point;
        }
        if point.x > max_x.x {
            max_x = point;
        }
        if point.y < min_y.y {
            min_y = point;
        }
        if point.y > max_y.y {
            max_y = point;
        }

        let sum = point.x + point.y;
        if sum < min_sum_value {
            min_sum = point;
            min_sum_value = sum;
        }
        if sum > max_sum_value {
            max_sum = point;
            max_sum_value = sum;
        }

        let diff = point.x - point.y;
        if diff < min_diff_value {
            min_diff = point;
            min_diff_value = diff;
        }
        if diff > max_diff_value {
            max_diff = point;
            max_diff_value = diff;
        }
    }

    let mut extremes = Vec::with_capacity(8);
    for point in [min_x, max_x, min_y, max_y, min_sum, max_sum, min_diff, max_diff] {
        if !extremes.contains(&point) {
            extremes.push(point);
        }
    }
    extremes
}

fn point_strictly_inside_convex_polygon(point: Vec2, polygon: &[Vec2]) -> bool {
    if polygon.len() < 3 {
        return false;
    }

    let mut a = *polygon.last().unwrap();
    for &b in polygon {
        if cross(b - a, point - a) <= GEOMETRY_EPSILON as f64 {
            return false;
        }
        a = b;
    }
    true
}

fn monotone_chain_hull(mut points: Vec<Vec2>) -> Vec<Vec2> {
    points.sort_by(|a, b| a.x.total_cmp(&b.x).then_with(|| a.y.total_cmp(&b.y)));
    points.dedup_by(|a, b| a.x == b.x && a.y == b.y);
    if points.len() < 3 {
        return Vec::new();
    }

    let mut lower = Vec::new();
    for &point in &points {
        while lower.len() >= 2 {
            let count = lower.len();
            if cross(lower[count - 1] - lower[count - 2], point - lower[count - 1]) > GEOMETRY_EPSILON as f64 {
                break;
            }
            lower.pop();
        }
        lower.push(point);
    }

    let mut upper = Vec::new();
    for &point in points.iter().rev() {
        while upper.len() >= 2 {
            let count = upper.len();
            if cross(upper[count - 1] - upper[count - 2], point - upper[count - 1]) > GEOMETRY_EPSILON as f64 {
                break;
            }
            upper.pop();
        }
        upper.push(point);
    }

    lower.pop();
    upper.pop();
    lower.extend(upper);
    if signed_polygon_area(&lower) < 0.0 {
        lower.reverse();
    }
    lower
}

fn cap_hull_to_max_vertices(hull: &[Vec2], max_vertices: usize) -> Option<Vec<Vec2>> {
    let mut polygon = hull.to_vec();
    let mut candidate = Vec::with_capacity(polygon.len().saturating_sub(1));
    let mut best_candidate = Vec::with_capacity(polygon.len().saturating_sub(1));

    while polygon.len() > max_vertices {
        let current_area = polygon_area(&polygon);
        let mut best_added_area = f64::INFINITY;
        best_candidate.clear();

        for edge_start in 0..polygon.len() {
            if !drop_edge_conservatively(&polygon, edge_start, &mut candidate) {
                continue;
            }

            // Every other current vertex is also a candidate vertex. Containing the two dropped
            // vertices therefore contains the whole current convex polygon and, inductively, the
            // original hull without rescanning it for every candidate.
            let b = polygon[edge_start];
            let c = polygon[(edge_start + 1) % polygon.len()];
            if !polygon_is_convex_ccw(&candidate)
                || !point_in_convex_polygon(b, &candidate)
                || !point_in_convex_polygon(c, &candidate)
            {
                continue;
            }

            let added_area = polygon_area(&candidate) - current_area;
            if added_area >= -area_tolerance(current_area) && added_area < best_added_area {
                best_added_area = added_area;
                best_candidate.clear();
                best_candidate.extend_from_slice(&candidate);
            }
        }

        if best_candidate.is_empty() {
            return None;
        }
        std::mem::swap(&mut polygon, &mut best_candidate);
    }

    polygon_contains_all(&polygon, hull).then_some(polygon)
}

fn drop_edge_conservatively(polygon: &[Vec2], edge_start: usize, candidate: &mut Vec<Vec2>) -> bool {
    debug_assert!(polygon.len() > 3);
    let len = polygon.len();
    let b_index = edge_start;
    let c_index = (edge_start + 1) % len;
    let a = polygon[(edge_start + len - 1) % len];
    let b = polygon[b_index];
    let c = polygon[c_index];
    let d = polygon[(edge_start + 2) % len];
    let Some(intersection) = line_intersection(a, b, c, d) else {
        return false;
    };

    candidate.clear();
    candidate.reserve(len - 1);
    for (index, point) in polygon.iter().copied().enumerate() {
        if index == b_index {
            candidate.push(intersection);
        } else if index != c_index {
            candidate.push(point);
        }
    }
    true
}

fn line_intersection(a: Vec2, b: Vec2, c: Vec2, d: Vec2) -> Option<Vec2> {
    let r = b - a;
    let s = d - c;
    let denominator = cross(r, s);
    if denominator.abs() <= GEOMETRY_EPSILON as f64 {
        return None;
    }

    let t = cross(c - a, s) / denominator;
    let point = Vec2::new(a.x + (t as f32) * r.x, a.y + (t as f32) * r.y);
    point.is_finite().then_some(point)
}

fn candidate_is_useful_footprint(polygon: &[Vec2], hull: &[Vec2]) -> bool {
    if !polygon_is_convex_ccw(polygon) || !polygon_contains_all(polygon, hull) {
        return false;
    }

    let Some((min, max)) = point_bounds(hull) else {
        return false;
    };

    polygon_area(polygon) <= aabb_area(min, max) + area_epsilon_for_bounds(min, max)
}

fn polygon_contains_all(polygon: &[Vec2], points: &[Vec2]) -> bool {
    points.iter().copied().all(|point| point_in_convex_polygon(point, polygon))
}

fn point_in_convex_polygon(point: Vec2, polygon: &[Vec2]) -> bool {
    if polygon.len() < 3 {
        return false;
    }

    let mut a = *polygon.last().unwrap();
    for &b in polygon {
        if cross(b - a, point - a) < -(GEOMETRY_EPSILON as f64) {
            return false;
        }
        a = b;
    }
    true
}

fn polygon_is_convex_ccw(polygon: &[Vec2]) -> bool {
    if polygon.len() < 3 || polygon_area(polygon) <= 0.0 {
        return false;
    }

    for index in 0..polygon.len() {
        let a = polygon[index];
        let b = polygon[(index + 1) % polygon.len()];
        let c = polygon[(index + 2) % polygon.len()];
        if cross(b - a, c - b) < -(GEOMETRY_EPSILON as f64) {
            return false;
        }
    }
    true
}

fn polygon_area(polygon: &[Vec2]) -> f64 {
    signed_polygon_area(polygon).abs()
}

fn signed_polygon_area(polygon: &[Vec2]) -> f64 {
    if polygon.len() < 3 {
        return 0.0;
    }

    let mut twice_area = 0.0;
    for index in 0..polygon.len() {
        let a = polygon[index];
        let b = polygon[(index + 1) % polygon.len()];
        twice_area += f64::from(a.x) * f64::from(b.y) - f64::from(a.y) * f64::from(b.x);
    }
    twice_area * 0.5
}

fn aabb_area(min: Vec2, max: Vec2) -> f64 {
    f64::from(max.x - min.x) * f64::from(max.y - min.y)
}

fn area_epsilon(aabb: BoundingBox) -> f64 {
    area_epsilon_for_bounds(aabb.min.truncate(), aabb.max.truncate())
}

fn area_epsilon_for_bounds(min: Vec2, max: Vec2) -> f64 {
    let width = (max.x - min.x).abs();
    let height = (max.y - min.y).abs();
    f64::from((width + height).max(1.0) * GEOMETRY_EPSILON)
}

fn area_tolerance(area: f64) -> f64 {
    (area.abs().max(1.0)) * f64::from(GEOMETRY_EPSILON)
}

fn cross(a: Vec2, b: Vec2) -> f64 {
    f64::from(a.x) * f64::from(b.y) - f64::from(a.y) * f64::from(b.x)
}

#[cfg(test)]
mod tests;
