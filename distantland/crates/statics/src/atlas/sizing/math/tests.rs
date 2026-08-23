use super::*;

/// A unit right triangle in the XY plane: edges of length 1 along +X and +Y.
fn unit_triangle() -> (Vec3, Vec3, Vec3) {
    (Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0))
}

fn density(p: (Vec3, Vec3, Vec3), uv: (Vec2, Vec2, Vec2), w: u32, h: u32, scale: f32) -> f32 {
    analyze_triangle_density(p.0, p.1, p.2, uv.0, uv.1, uv.2, w, h, scale).expect("expected a valid density")
}

#[test]
fn singular_values_of_diagonal_scale_are_the_scales() {
    let m = Mat2::from_cols(Vec2::new(3.0, 0.0), Vec2::new(0.0, 7.0));
    let [s_min, s_max] = singular_values_2x2(m).unwrap();
    assert!((s_min - 3.0).abs() < 1e-4);
    assert!((s_max - 7.0).abs() < 1e-4);
}

#[test]
fn singular_values_invariant_under_rotation() {
    let scale = Mat2::from_cols(Vec2::new(3.0, 0.0), Vec2::new(0.0, 7.0));
    let theta = 0.9_f32;
    let rot = Mat2::from_cols(Vec2::new(theta.cos(), theta.sin()), Vec2::new(-theta.sin(), theta.cos()));
    let [s_min, s_max] = singular_values_2x2(rot * scale).unwrap();
    assert!((s_min - 3.0).abs() < 1e-3);
    assert!((s_max - 7.0).abs() < 1e-3);
}

#[test]
fn rank_one_mapping_uses_its_active_direction() {
    // UV varies only along the first edge: rank-1. It is accepted as a measurement rather than
    // rejected as constant UV, but it spans no texel *area*, so it measures zero density, which
    // `select_longest` turns into "keep the baseline". Rank-0 is rejected outright instead. See
    // `rank_zero_mapping_is_constant_uv`.
    let p = unit_triangle();
    let uv = (Vec2::new(0.0, 0.0), Vec2::new(1.0, 0.0), Vec2::new(0.0, 0.0));
    assert_eq!(density(p, uv, 256, 256, 1.0), 0.0);
}

#[test]
fn rank_zero_mapping_is_constant_uv() {
    let p = unit_triangle();
    let uv = (Vec2::ZERO, Vec2::ZERO, Vec2::ZERO);
    let err = analyze_triangle_density(p.0, p.1, p.2, uv.0, uv.1, uv.2, 256, 256, 1.0).unwrap_err();
    assert_eq!(err, TriangleFailure::ConstantUv);
}

#[test]
fn degenerate_world_triangle_is_ignored() {
    let p = (Vec3::ZERO, Vec3::ZERO, Vec3::ZERO);
    let uv = (Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0));
    let err = analyze_triangle_density(p.0, p.1, p.2, uv.0, uv.1, uv.2, 256, 256, 1.0).unwrap_err();
    assert_eq!(err, TriangleFailure::IgnoredDegenerateWorld);
}

#[test]
fn non_finite_input_and_scale_are_rejected() {
    let p = unit_triangle();
    let uv = (Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0));
    assert_eq!(
        analyze_triangle_density(p.0, p.1, p.2, uv.0, uv.1, uv.2, 256, 256, 0.0).unwrap_err(),
        TriangleFailure::NonFinite
    );
    assert_eq!(
        analyze_triangle_density(p.0, p.1, p.2, uv.0, uv.1, uv.2, 256, 256, f32::NAN).unwrap_err(),
        TriangleFailure::NonFinite
    );
    let bad = Vec3::new(f32::INFINITY, 0.0, 0.0);
    assert_eq!(
        analyze_triangle_density(bad, p.1, p.2, uv.0, uv.1, uv.2, 256, 256, 1.0).unwrap_err(),
        TriangleFailure::NonFinite
    );
}

#[test]
fn doubling_scale_halves_density() {
    let p = unit_triangle();
    let uv = (Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0));
    let base = density(p, uv, 256, 256, 1.0);
    let scaled = density(p, uv, 256, 256, 2.0);
    assert!((base / scaled - 2.0).abs() < 1e-3);
}

#[test]
fn doubling_both_dims_doubles_density() {
    let p = unit_triangle();
    let uv = (Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0));
    let base = density(p, uv, 256, 256, 1.0);
    let bigger = density(p, uv, 512, 512, 1.0);
    assert!((bigger / base - 2.0).abs() < 1e-3);
}

#[test]
fn raw_uv_span_scales_density_and_ignores_integer_offsets() {
    let p = unit_triangle();
    // Span 0->1.
    let span1 = density(p, (Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)), 256, 256, 1.0);
    // Span 0->8 (8 tiling repetitions) on both axes -> 64x texel area -> 8x area density.
    let span8 = density(p, (Vec2::ZERO, Vec2::new(8.0, 0.0), Vec2::new(0.0, 8.0)), 256, 256, 1.0);
    assert!((span8 / span1 - 8.0).abs() < 1e-2);

    // Integer offset (8->9 instead of 0->1) leaves density unchanged: raw delta is identical.
    let offset = density(
        p,
        (Vec2::new(8.0, 8.0), Vec2::new(9.0, 8.0), Vec2::new(8.0, 9.0)),
        256,
        256,
        1.0,
    );
    assert!((offset - span1).abs() < 1e-3);
}

#[test]
fn wrapped_raw_delta_is_used_verbatim_not_shortest_wrapped() {
    // u: 0.98 -> 0.02 is a raw delta of -0.96 (the runtime interpolates straight across), not
    // +0.04. The density must reflect the large raw span, not the small wrapped one.
    let p = unit_triangle();
    let big_span = density(
        p,
        (Vec2::new(0.98, 0.0), Vec2::new(0.02, 0.0), Vec2::new(0.98, 1.0)),
        256,
        256,
        1.0,
    );
    let small_span = density(
        p,
        (Vec2::new(0.0, 0.0), Vec2::new(0.04, 0.0), Vec2::new(0.0, 1.0)),
        256,
        256,
        1.0,
    );
    // The U-axis texel area reflects the full 0.96 raw span, 24x the 0.04 wrapped delta. Area
    // density is the square root of that, so it shows as ~4.9x, still nowhere near the 1x a
    // `frac()`-before-analysis bug would produce by collapsing both to the same small span.
    assert!(big_span > small_span * 4.0);
}

#[test]
fn winding_reversal_preserves_density_magnitude() {
    let p = unit_triangle();
    let uv = (Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0));
    let forward = density(p, uv, 256, 256, 1.0);
    // Reverse winding: swap the second and third vertices and their UVs.
    let reversed = analyze_triangle_density(p.0, p.2, p.1, uv.0, uv.2, uv.1, 256, 256, 1.0).unwrap();
    assert!((forward - reversed).abs() < 1e-3);
}
