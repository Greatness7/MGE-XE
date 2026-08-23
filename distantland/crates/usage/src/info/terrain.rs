use super::*;

/// Per-reference tally of where the `is_buried` burial test spends work and which exit decides
/// each reference, aggregated across the parallel buried-cull pass, returned for generation
/// reporting, and recorded as fields on the `usage.discard_low_visibility_references` span.
///
/// The five outcome counters partition `refs_considered`, and the two work counters expose the
/// triangle-transform and terrain-solve cost paid by the references that fall through to the full
/// per-triangle walk. The struct is a plain stack accumulator reduced with the parallel iterator,
/// so it adds no atomics or lock contention to the heuristic it measures.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct BurialStats {
    /// Non-grass exterior references with a resolved mesh that ran the heuristic.
    pub refs_considered: u64,
    /// Kept before the triangle loop: a subset clears terrain by the exposure threshold.
    pub keep_clearance_shortcut: u64,
    /// Kept inside the loop as soon as a triangle protrudes past the exposure threshold.
    pub keep_height_early: u64,
    /// Kept fail-closed: too few terrain-backed samples to trust a bury decision.
    pub keep_insufficient: u64,
    /// Kept by the final predicate: exposed height and/or area too large to bury.
    pub keep_exposed: u64,
    /// Buried by the final predicate.
    pub buried: u64,
    /// Triangles transformed inside the per-triangle loop (excludes fully-below subsets and the
    /// clearance-shortcut probe).
    pub tris_visited: u64,
    /// Per-triangle centroid terrain-height solves. This is the near-terrain straddling cost that the
    /// sphere shortcuts cannot avoid.
    pub centroid_height_samples: u64,
}

impl BurialStats {
    /// Folds another reference's tally into this one for the parallel reduction.
    pub(crate) fn merge(&mut self, other: Self) {
        self.refs_considered += other.refs_considered;
        self.keep_clearance_shortcut += other.keep_clearance_shortcut;
        self.keep_height_early += other.keep_height_early;
        self.keep_insufficient += other.keep_insufficient;
        self.keep_exposed += other.keep_exposed;
        self.buried += other.buried;
        self.tris_visited += other.tris_visited;
        self.centroid_height_samples += other.centroid_height_samples;
    }
}

impl DistantReference<'_> {
    /// The maximum Z value of the bounding box in world space
    #[rustfmt::skip]
    pub fn world_max_z(&self, bb: &BoundingBox) -> f32 {
        let transform = self.get_transform();
        let m = &transform.matrix3;
        let position = transform.transform_point3(Vec3::new(
            if m.x_axis.z >= 0.0 { bb.max.x } else { bb.min.x },
            if m.y_axis.z >= 0.0 { bb.max.y } else { bb.min.y },
            if m.z_axis.z >= 0.0 { bb.max.z } else { bb.min.z },
        ));
        position.z
    }

    /// Calculates the axis-aligned bounding box of the reference in world space.
    pub fn world_aabb(&self, bb: &BoundingBox) -> (Vec3, Vec3) {
        let transform = self.get_transform();
        let corners = [
            Vec3::new(bb.min.x, bb.min.y, bb.min.z),
            Vec3::new(bb.min.x, bb.min.y, bb.max.z),
            Vec3::new(bb.min.x, bb.max.y, bb.min.z),
            Vec3::new(bb.min.x, bb.max.y, bb.max.z),
            Vec3::new(bb.max.x, bb.min.y, bb.min.z),
            Vec3::new(bb.max.x, bb.min.y, bb.max.z),
            Vec3::new(bb.max.x, bb.max.y, bb.min.z),
            Vec3::new(bb.max.x, bb.max.y, bb.max.z),
        ];

        let mut min = Vec3::splat(f32::MAX);
        let mut max = Vec3::splat(f32::MIN);

        for corner in corners {
            let p = transform.transform_point3(corner);
            min = min.min(p);
            max = max.max(p);
        }

        (min, max)
    }

    /// Constructs the 3D affine transformation matrix for this reference.
    pub fn get_transform(&self) -> Affine3A {
        let [x, y, z] = self.rotation.to_array();
        Affine3A::from_scale_rotation_translation(
            Vec3::splat(self.scale),
            Quat::from_euler(EulerRot::XYZ, -x, -y, -z),
            self.translation,
        )
    }

    /// Return the integer cell coordinates for this reference.
    pub fn cell_coords(&self) -> (i32, i32) {
        let cell_x = (self.translation.x as i32) >> 13;
        let cell_y = (self.translation.y as i32) >> 13;
        (cell_x, cell_y)
    }
}

/// Queries the terrain height at a specific world coordinate.
///
/// Heights are sampled from the sole LAND store on [`UsageInfo::terrain_cells`].
pub fn terrain_height_at(terrain_cells: &TerrainCells<'_>, world_xy: Vec2) -> Option<f32> {
    let cell_x = (world_xy.x / LAND_CELL_SIZE).floor() as i32;
    let cell_y = (world_xy.y / LAND_CELL_SIZE).floor() as i32;
    let cell = terrain_cells.get(&(cell_x, cell_y))?;
    let local_xy = Vec2::new(
        world_xy.x - cell_x as f32 * LAND_CELL_SIZE,
        world_xy.y - cell_y as f32 * LAND_CELL_SIZE,
    );
    Some(get_height_at(local_xy, &cell.heights))
}

/// A point's location within its containing 128-unit quad: the quad's lower-left grid index, the
/// fractional position within the quad in `[0.0, 1.0)`, and which diagonal splits the quad.
struct QuadLocation {
    x: usize,
    y: usize,
    u: f32,
    v: f32,
    bw: usize,
}

/// Resolves a local cell-space point to its containing quad on the 64×64 grid.
#[inline]
fn locate_quad(p: Vec2) -> QuadLocation {
    const QUAD_SIZE: f32 = 128.0;
    const Q_INVERSE: f32 = 1.0 / QUAD_SIZE;

    // Calculate grid float coordinates once
    let px = p.x * Q_INVERSE;
    let py = p.y * Q_INVERSE;

    let row = px.floor().clamp(0.0, 63.0);
    let col = py.floor().clamp(0.0, 63.0);

    let x = row as usize;
    let y = col as usize;

    // Fractional coordinates within the current quad [0.0, 1.0)
    let u = px - row;
    let v = py - col;

    // is the diagonal backward (1) or forward (0)
    let bw = (y * 65 + x) & 1;

    QuadLocation { x, y, u, v, bw }
}

/// Calculate the triangle that contains a point (x, y).
///
/// Note: The triangle winding pattern is not preserved.
///
pub fn get_triangle_at(p: Vec2) -> [(usize, usize); 3] {
    let QuadLocation { x, y, u, v, bw } = locate_quad(p);

    let lt = {
        // what side of the diagonal are we on
        // Branchless check using bitwise operators on bools:
        // backward -> diagonal is '/', left means v > u
        // backward -> diagonal is '\', left means u + v < 1.0
        (bw == 0) & (v > u) | (bw == 1) & (u + v < 1.0)
    } as usize;

    // the four indices that make our quad
    let (bl, br, tl, tr) = ((x, y), (x + 1, y), (x, y + 1), (x + 1, y + 1));

    match (bw, lt) {
        (0, 0) => [br, tr, bl],
        (0, 1) => [tr, tl, bl],
        (1, 0) => [tl, br, tr],
        (1, 1) => [bl, br, tl],
        _ => unreachable!(),
    }
}

/// Computes the exact interpolated height of a point on the terrain grid.
///
/// Every quad splits into two fixed right triangles along one of its diagonals, so this
/// interpolates directly from the three surrounding grid heights instead of fitting a general
/// 3-D plane through them: since the grid's x/y positions are always regular multiples of the
/// quad size and only height varies, the two are algebraically the same surface.
pub fn get_height_at(p: Vec2, heights: &[[f32; 65]; 65]) -> f32 {
    let QuadLocation { x, y, u, v, bw } = locate_quad(p);

    let h_bl = heights[y][x];
    let h_br = heights[y][x + 1];
    let h_tl = heights[y + 1][x];
    let h_tr = heights[y + 1][x + 1];

    if bw == 0 {
        if v > u {
            // upper-left triangle [tr, tl, bl], right angle at tl
            h_tl + u * (h_tr - h_tl) + (1.0 - v) * (h_bl - h_tl)
        } else {
            // lower-right triangle [br, tr, bl], right angle at br
            h_br + v * (h_tr - h_br) + (1.0 - u) * (h_bl - h_br)
        }
    } else if u + v < 1.0 {
        // lower-left triangle [bl, br, tl], right angle at bl
        h_bl + u * (h_br - h_bl) + v * (h_tl - h_bl)
    } else {
        // upper-right triangle [tl, br, tr], right angle at tr
        h_tr + (1.0 - u) * (h_tl - h_tr) + (1.0 - v) * (h_br - h_tr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Independent oracle matching the pre-specialization algorithm: fits a plane through the
    /// three grid vertices `get_triangle_at` selects and solves it for height at `p`. Used to
    /// check the piecewise-linear `get_height_at` against a differently-derived calculation
    /// rather than against itself.
    fn reference_height_at(p: Vec2, heights: &[[f32; 65]; 65]) -> f32 {
        let pts = get_triangle_at(p);
        let vertex = |(x, y): (usize, usize)| Vec3::new(x as f32 * 128.0, y as f32 * 128.0, heights[y][x]);
        let [v0, v1, v2] = pts.map(vertex);
        let n = (v1 - v0).cross(v2 - v0);
        let d = -n.dot(v0);
        -(n.x * p.x + n.y * p.y + d) / n.z
    }

    /// A deliberately non-planar height field so each quad's two triangles sit on distinct
    /// planes, making triangle-selection or wrong-corner mistakes detectable.
    fn sample_heights() -> Box<[[f32; 65]; 65]> {
        let mut heights = Box::new([[0.0f32; 65]; 65]);
        for (y, row) in heights.iter_mut().enumerate() {
            for (x, height) in row.iter_mut().enumerate() {
                *height = (x as f32 - 32.0).powi(2) - (y as f32 - 32.0).powi(2) + 0.37 * (x * y) as f32;
            }
        }
        heights
    }

    #[test]
    fn get_height_at_matches_reference_plane_solve_across_all_quad_configurations() {
        let heights = sample_heights();

        for y in 0..64 {
            for x in 0..64 {
                for &(fx, fy) in &[
                    (0.0, 0.0),
                    (1.0, 0.0),
                    (0.0, 1.0),
                    (1.0, 1.0), // vertices
                    (0.25, 0.75),
                    (0.75, 0.25),
                    (0.5, 0.5), // interior, both sides of either diagonal
                    (0.1, 0.9),
                    (0.9, 0.1),
                ] {
                    let p = Vec2::new((x as f32 + fx) * 128.0, (y as f32 + fy) * 128.0);
                    let expected = reference_height_at(p, &heights);
                    let actual = get_height_at(p, &heights);
                    assert!(
                        (actual - expected).abs() < 1e-3,
                        "quad ({x},{y}) frac ({fx},{fy}): expected {expected}, got {actual}"
                    );
                }
            }
        }
    }
}
