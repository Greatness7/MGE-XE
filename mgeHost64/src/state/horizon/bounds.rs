use crate::abi::{BoundingBox, D3dxVector3, HorizonFootprint};

/// The projected OBB hull has at most six vertices.
#[derive(Clone, Copy, Debug)]
pub struct HorizonMeshBounds {
    pub max_z: f32,
    /// Zero is degenerate and must fail open.
    pub vertex_count: u8,
    /// Only the first `vertex_count` entries are meaningful.
    pub footprint_xy: [(f32, f32); 6],
    pub footprint_center: (f32, f32),
    /// Zero means the cheap horizon path must fall back to the bounding sphere.
    pub footprint_radius: f32,
}

impl HorizonMeshBounds {
    pub fn from_box(box_value: BoundingBox) -> Self {
        let corners = [
            box_value.center + box_value.vx + box_value.vy + box_value.vz,
            box_value.center + box_value.vx + box_value.vy - box_value.vz,
            box_value.center + box_value.vx - box_value.vy + box_value.vz,
            box_value.center + box_value.vx - box_value.vy - box_value.vz,
            box_value.center - box_value.vx + box_value.vy + box_value.vz,
            box_value.center - box_value.vx + box_value.vy - box_value.vz,
            box_value.center - box_value.vx - box_value.vy + box_value.vz,
            box_value.center - box_value.vx - box_value.vy - box_value.vz,
        ];

        let mut max_z = f32::NEG_INFINITY;
        let mut projected = [(0.0_f32, 0.0_f32); 8];
        for (slot, corner) in projected.iter_mut().zip(corners) {
            *slot = (corner.x, corner.y);
            max_z = max_z.max(corner.z);
        }

        let (hull, hull_len) = projected_box_hull(&projected);
        let mut footprint = [(0.0_f32, 0.0_f32); 6];
        let vertex_count = if (3..=6).contains(&hull_len) {
            footprint[..hull_len].copy_from_slice(&hull[..hull_len]);
            hull_len as u8
        } else {
            // Degenerate footprints must fail open.
            0
        };

        let (footprint_center, footprint_radius) =
            minimum_enclosing_circle(&footprint[..vertex_count as usize]).unwrap_or(((0.0, 0.0), 0.0));

        Self {
            max_z,
            vertex_count,
            footprint_xy: footprint,
            footprint_center,
            footprint_radius,
        }
    }

    /// Generated footprints require translation-only placement; rotation and scale are
    /// validated at the loading call site before this constructor runs.
    pub fn from_generated_footprint(footprint: &HorizonFootprint, translation: D3dxVector3) -> Option<Self> {
        let vertex_count = usize::from(footprint.vertex_count);
        if !(3..=6).contains(&vertex_count)
            || !footprint.max_z.is_finite()
            || !translation.x.is_finite()
            || !translation.y.is_finite()
            || !translation.z.is_finite()
        {
            return None;
        }

        // Measure area locally to avoid cancellation at world-scale coordinates.
        let mut local_xy = [(0.0_f32, 0.0_f32); 6];
        for (slot, point) in local_xy.iter_mut().zip(footprint.footprint_xy[..vertex_count].iter()) {
            if !point[0].is_finite() || !point[1].is_finite() {
                return None;
            }
            *slot = (point[0], point[1]);
        }
        if generated_footprint_signed_area(&local_xy[..vertex_count]) <= 1.0e-4 {
            return None;
        }

        let (local_center, footprint_radius) = minimum_enclosing_circle(&local_xy[..vertex_count])?;

        let mut footprint_xy = [(0.0_f32, 0.0_f32); 6];
        for (slot, &(lx, ly)) in footprint_xy.iter_mut().zip(local_xy[..vertex_count].iter()) {
            let x = lx + translation.x;
            let y = ly + translation.y;
            if !x.is_finite() || !y.is_finite() {
                return None;
            }
            *slot = (x, y);
        }

        let footprint_center = (local_center.0 + translation.x, local_center.1 + translation.y);
        if !footprint_center.0.is_finite() || !footprint_center.1.is_finite() {
            return None;
        }

        let max_z = footprint.max_z + translation.z;
        max_z.is_finite().then_some(Self {
            max_z,
            vertex_count: footprint.vertex_count,
            footprint_xy,
            footprint_center,
            footprint_radius,
        })
    }

    /// Returns the cached footprint circle when it is valid for the cheap horizon path.
    pub fn footprint_circle(&self) -> Option<((f32, f32), f32)> {
        (self.vertex_count >= 3
            && self.footprint_radius > 0.0
            && self.footprint_radius.is_finite()
            && self.footprint_center.0.is_finite()
            && self.footprint_center.1.is_finite())
        .then_some((self.footprint_center, self.footprint_radius))
    }
}

pub(super) fn generated_footprint_signed_area(points: &[(f32, f32)]) -> f32 {
    let mut twice_area = 0.0;
    for index in 0..points.len() {
        let a = points[index];
        let b = points[(index + 1) % points.len()];
        twice_area += a.0 * b.1 - a.1 * b.0;
    }
    twice_area * 0.5
}

fn cross(o: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
}

fn projected_box_hull(corners: &[(f32, f32); 8]) -> ([(f32, f32); 8], usize) {
    let mut sorted = *corners;
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));

    let mut chain = [(0.0_f32, 0.0_f32); 16];
    let mut k = 0_usize;
    for &p in &sorted {
        while k >= 2 && cross(chain[k - 2], chain[k - 1], p) <= 0.0 {
            k -= 1;
        }
        chain[k] = p;
        k += 1;
    }
    let lower = k + 1;
    for &p in sorted.iter().rev().skip(1) {
        while k >= lower && cross(chain[k - 2], chain[k - 1], p) <= 0.0 {
            k -= 1;
        }
        chain[k] = p;
        k += 1;
    }

    let count = k.saturating_sub(1);
    let mut hull = [(0.0_f32, 0.0_f32); 8];
    let valid = count.min(hull.len());
    hull[..valid].copy_from_slice(&chain[..valid]);
    (hull, valid)
}

pub(super) fn minimum_enclosing_circle(points: &[(f32, f32)]) -> Option<((f32, f32), f32)> {
    if points.len() < 3 || points.len() > 6 {
        return None;
    }
    let origin = points[0];
    if !origin.0.is_finite() || !origin.1.is_finite() {
        return None;
    }

    let mut local = [(0.0_f32, 0.0_f32); 6];
    for (slot, &(x, y)) in local.iter_mut().zip(points.iter()) {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        *slot = (x - origin.0, y - origin.1);
    }
    let local = &local[..points.len()];

    let mut best: Option<((f32, f32), f32)> = None;
    for i in 0..local.len() {
        for j in i + 1..local.len() {
            let center = ((local[i].0 + local[j].0) * 0.5, (local[i].1 + local[j].1) * 0.5);
            let radius_sq = distance_sq(center, local[i]);
            consider_enclosing_circle(center, radius_sq, local, &mut best);
        }
    }

    for i in 0..local.len() {
        for j in i + 1..local.len() {
            for k in j + 1..local.len() {
                if let Some((center, radius_sq)) = circumcircle(local[i], local[j], local[k]) {
                    consider_enclosing_circle(center, radius_sq, local, &mut best);
                }
            }
        }
    }

    best.map(|(center, radius_sq)| {
        let radius = radius_sq.sqrt();
        let radius = radius + radius.max(1.0) * 1.0e-6;
        ((center.0 + origin.0, center.1 + origin.1), radius)
    })
}

fn consider_enclosing_circle(
    center: (f32, f32),
    radius_sq: f32,
    points: &[(f32, f32)],
    best: &mut Option<((f32, f32), f32)>,
) {
    if !center.0.is_finite() || !center.1.is_finite() || !radius_sq.is_finite() || radius_sq <= 0.0 {
        return;
    }

    let tolerance = radius_sq.max(1.0) * 1.0e-5;
    let mut needed_radius_sq = 0.0_f32;
    for &point in points {
        let point_radius_sq = distance_sq(center, point);
        if !point_radius_sq.is_finite() || point_radius_sq > radius_sq + tolerance {
            return;
        }
        needed_radius_sq = needed_radius_sq.max(point_radius_sq);
    }

    if best.is_none_or(|(_, best_radius_sq)| needed_radius_sq < best_radius_sq) {
        *best = Some((center, needed_radius_sq));
    }
}

fn circumcircle(a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> Option<((f32, f32), f32)> {
    let d = 2.0 * (a.0 * (b.1 - c.1) + b.0 * (c.1 - a.1) + c.0 * (a.1 - b.1));
    if !d.is_finite() || d.abs() <= 1.0e-6 {
        return None;
    }

    let a_len = a.0 * a.0 + a.1 * a.1;
    let b_len = b.0 * b.0 + b.1 * b.1;
    let c_len = c.0 * c.0 + c.1 * c.1;
    let center = (
        (a_len * (b.1 - c.1) + b_len * (c.1 - a.1) + c_len * (a.1 - b.1)) / d,
        (a_len * (c.0 - b.0) + b_len * (a.0 - c.0) + c_len * (b.0 - a.0)) / d,
    );
    let radius_sq = distance_sq(center, a);
    (center.0.is_finite() && center.1.is_finite() && radius_sq.is_finite()).then_some((center, radius_sq))
}

fn distance_sq(a: (f32, f32), b: (f32, f32)) -> f32 {
    let dx = a.0 - b.0;
    let dy = a.1 - b.1;
    dx * dx + dy * dy
}
