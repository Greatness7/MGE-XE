//! Per-triangle texel-density measurement from transformed geometry and raw UVs.
//!
//! UV gradients match the runtime shader: original unbounded UVs are differenced without wrapping
//! or normalization, and `frac()` is applied only to the final atlas coordinate.

use glam::{Mat2, Vec2, Vec3};

/// World-space cross/determinant magnitude at or below which a mapping is treated as degenerate.
///
/// Calibration-tunable. World units are large (Morrowind game units), so a real triangle's scaled
/// edge cross product is far above this; values this small mean a zero-area or rank-deficient world
/// triangle.
pub(crate) const WORLD_EPS: f32 = 1e-6;

/// Directional (singular-value) density at or below which an axis is treated as carrying no detail.
///
/// Calibration-tunable. A singular value above this is an active mapping direction; at or below it
/// the axis is degenerate (rank-deficient).
pub(crate) const DENSITY_EPS: f32 = 1e-6;

/// Reasons a triangle produced no usable density measurement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TriangleFailure {
    /// Zero-area (or sub-epsilon) world triangle: it covers no pixels, so it is counted and ignored
    /// without affecting the sizing decision.
    IgnoredDegenerateWorld,
    /// The world basis was numerically singular on otherwise non-degenerate geometry: uncertain.
    UncertainWorldMapping,
    /// Non-finite input, non-positive scale, or numerical failure: uncertain.
    NonFinite,
    /// Rank-0 (constant) UV mapping. Reducing the source could change the sampled color through
    /// filtering, so the texture keeps its baseline dimensions.
    ConstantUv,
}

/// Measures the texel density of one triangle using transformed positions `p0..p2`, raw UVs
/// `uv0..uv2`, baseline (globally-capped) dims `(w, h)`, and the mesh's max reference scale.
///
/// Returns `sqrt(texel_area / world_area)`: texels per game unit, averaged over the triangle.
///
/// Using the *largest* placed scale is conservative: bigger instances have lower density.
pub(crate) fn analyze_triangle_density(
    p0: Vec3,
    p1: Vec3,
    p2: Vec3,
    uv0: Vec2,
    uv1: Vec2,
    uv2: Vec2,
    w: u32,
    h: u32,
    scale: f32,
) -> Result<f32, TriangleFailure> {
    if !scale.is_finite() || scale <= 0.0 {
        return Err(TriangleFailure::NonFinite);
    }
    if !(p0.is_finite() && p1.is_finite() && p2.is_finite() && uv0.is_finite() && uv1.is_finite() && uv2.is_finite()) {
        return Err(TriangleFailure::NonFinite);
    }

    let ea = (p1 - p0) * scale;
    let eb = (p2 - p0) * scale;
    let cross = ea.cross(eb);
    let cross_len = cross.length();
    if cross_len <= WORLD_EPS {
        return Err(TriangleFailure::IgnoredDegenerateWorld);
    }
    let world_area = 0.5 * cross_len;

    // Raw UV edges use the baseline dimensions without wrap, clamp, or normalization.
    let wf = w as f32;
    let hf = h as f32;
    let ta = Vec2::new((uv1.x - uv0.x) * wf, (uv1.y - uv0.y) * hf);
    let tb = Vec2::new((uv2.x - uv0.x) * wf, (uv2.y - uv0.y) * hf);
    let texel_area = 0.5 * (ta.x * tb.y - ta.y * tb.x).abs();

    // Orthonormal basis on the triangle; express both edge sets in it to form the Jacobian.
    let tx = ea.normalize();
    let n = cross / cross_len;
    let ty = n.cross(tx);
    let world = Mat2::from_cols(Vec2::new(ea.dot(tx), ea.dot(ty)), Vec2::new(eb.dot(tx), eb.dot(ty)));
    if world.determinant().abs() <= WORLD_EPS {
        return Err(TriangleFailure::UncertainWorldMapping);
    }

    // The singular values classify the mapping's rank. Both being at or below the epsilon means the
    // UVs are constant across the triangle; a rank-1 mapping still measures, but its zero texel area
    // yields a zero density, which `select_longest` treats as "keep the baseline".
    let j = Mat2::from_cols(ta, tb) * world.inverse();
    let [_, s_max] = singular_values_2x2(j).ok_or(TriangleFailure::NonFinite)?;
    if s_max <= DENSITY_EPS {
        return Err(TriangleFailure::ConstantUv);
    }

    let area_density = (texel_area / world_area).sqrt();
    if !area_density.is_finite() {
        return Err(TriangleFailure::NonFinite);
    }
    Ok(area_density)
}

/// Returns `[σ_min, σ_max]` of a 2×2 matrix from the eigenvalues of `MᵀM` (closed form, no SVD).
///
/// Returns `None` on any non-finite intermediate.
pub(crate) fn singular_values_2x2(m: Mat2) -> Option<[f32; 2]> {
    // MᵀM is symmetric `[[a, b], [b, c]]` where the columns of `m` are `c0`, `c1`.
    let c0 = m.x_axis;
    let c1 = m.y_axis;
    let a = c0.dot(c0);
    let b = c0.dot(c1);
    let c = c1.dot(c1);
    if !a.is_finite() || !b.is_finite() || !c.is_finite() {
        return None;
    }

    // Eigenvalues λ = (a+c)/2 ± sqrt(((a-c)/2)² + b²); σ = sqrt(λ.max(0)).
    let mid = 0.5 * (a + c);
    let diff = 0.5 * (a - c);
    let disc = (diff * diff + b * b).max(0.0).sqrt();
    let s_max = (mid + disc).max(0.0).sqrt();
    let s_min = (mid - disc).max(0.0).sqrt();
    if !s_min.is_finite() || !s_max.is_finite() {
        return None;
    }
    Some([s_min, s_max])
}

#[cfg(test)]
mod tests;
