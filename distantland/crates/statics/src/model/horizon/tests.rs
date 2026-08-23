use super::*;
use glam::Vec3;

fn vertex(x: f32, y: f32, z: f32) -> Vertex {
    Vertex {
        position: Vec3::new(x, y, z),
        ..Vertex::default()
    }
}

fn aabb(min_x: f32, min_y: f32, max_x: f32, max_y: f32, max_z: f32) -> BoundingBox {
    BoundingBox {
        min: Vec3::new(min_x, min_y, 0.0),
        max: Vec3::new(max_x, max_y, max_z),
    }
}

fn footprint_points(footprint: HorizonFootprint) -> Vec<Vec2> {
    footprint.footprint_xy[..footprint.vertex_count as usize]
        .iter()
        .map(|point| Vec2::new(point[0], point[1]))
        .collect()
}

#[test]
fn collinear_vertices_fail_open() {
    let vertices = [vertex(0.0, 0.0, 1.0), vertex(1.0, 1.0, 2.0), vertex(2.0, 2.0, 3.0)];

    let footprint = horizon_footprint_from_vertices(&vertices, aabb(0.0, 0.0, 2.0, 2.0, 3.0));

    assert_eq!(footprint.vertex_count, 0);
}

#[test]
fn hull_with_six_vertices_is_preserved() {
    let vertices = [
        vertex(-2.0, 0.0, 1.0),
        vertex(-1.0, -2.0, 2.0),
        vertex(1.0, -2.0, 3.0),
        vertex(2.0, 0.0, 4.0),
        vertex(1.0, 2.0, 5.0),
        vertex(-1.0, 2.0, 6.0),
        vertex(0.0, 0.0, 7.0),
    ];

    let footprint = horizon_footprint_from_vertices(&vertices, aabb(-2.0, -2.0, 2.0, 2.0, 7.0));
    let points = footprint_points(footprint);

    assert_eq!(footprint.vertex_count, 6);
    assert_eq!(footprint.max_z, 7.0);
    assert!((polygon_area(&points) - 12.0).abs() < 1.0e-4);
}

#[test]
fn capped_hull_contains_original_hull() {
    let hull = vec![
        Vec2::new(-4.0, -2.0),
        Vec2::new(-2.0, -4.0),
        Vec2::new(2.0, -4.0),
        Vec2::new(4.0, -2.0),
        Vec2::new(4.0, 2.0),
        Vec2::new(2.0, 4.0),
        Vec2::new(-2.0, 4.0),
        Vec2::new(-4.0, 2.0),
    ];

    let capped = cap_hull_to_max_vertices(&hull, HORIZON_FOOTPRINT_MAX_VERTS).expect("capped hull");

    assert!(capped.len() <= HORIZON_FOOTPRINT_MAX_VERTS);
    assert!(polygon_contains_all(&capped, &hull));
}

#[test]
fn capped_hull_outside_aabb_is_kept_when_tighter_than_enclosing_rectangle() {
    let vertices = [
        vertex(0.0, 10.0, 1.0),
        vertex(7.0, 7.0, 2.0),
        vertex(10.0, 0.0, 3.0),
        vertex(7.0, -7.0, 4.0),
        vertex(0.0, -10.0, 5.0),
        vertex(-7.0, -7.0, 6.0),
        vertex(-10.0, 0.0, 7.0),
        vertex(-7.0, 7.0, 8.0),
    ];

    let footprint = horizon_footprint_from_vertices(&vertices, aabb(-10.0, -10.0, 10.0, 10.0, 8.0));
    let points = footprint_points(footprint);
    let original_points: Vec<_> = vertices.iter().map(|vertex| vertex.position.truncate()).collect();

    assert_eq!(footprint.vertex_count, 6);
    assert!(
        points
            .iter()
            .any(|point| point.x < -10.0 || point.x > 10.0 || point.y < -10.0 || point.y > 10.0)
    );
    assert!(polygon_contains_all(&points, &original_points));
    assert!(polygon_area(&points) < 400.0);
}

#[test]
fn footprint_keeps_half_float_position_expansion() {
    let raw_x = 1.0006;
    let packed_x = f16::from_f32(raw_x).to_f32();
    let vertices = [vertex(0.0, 0.0, 1.0), vertex(raw_x, 0.0, 2.0), vertex(0.0, 1.0, 3.0)];

    let footprint = horizon_footprint_from_vertices(&vertices, aabb(0.0, 0.0, raw_x, 1.0, 3.0));
    let max_footprint_x = footprint_points(footprint)
        .iter()
        .map(|point| point.x)
        .fold(f32::NEG_INFINITY, f32::max);

    assert!(packed_x > raw_x);
    assert!(max_footprint_x >= packed_x);
}

#[test]
fn hull_prefilter_discards_interior_points_without_changing_hull() {
    let mut points = vec![
        Vec2::new(-4.0, -2.0),
        Vec2::new(-2.0, -4.0),
        Vec2::new(2.0, -4.0),
        Vec2::new(4.0, -2.0),
        Vec2::new(4.0, 2.0),
        Vec2::new(2.0, 4.0),
        Vec2::new(-2.0, 4.0),
        Vec2::new(-4.0, 2.0),
    ];
    for y in -8..=8 {
        for x in -8..=8 {
            points.push(Vec2::new(x as f32 * 0.25, y as f32 * 0.25));
        }
    }

    let original_hull = monotone_chain_hull(points.clone());
    let original_len = points.len();

    prefilter_hull_points(&mut points);
    let filtered_hull = monotone_chain_hull(points.clone());

    assert!(points.len() < original_len / 2);
    assert_eq!(filtered_hull, original_hull);
}

#[test]
fn footprint_keeps_half_float_expansion_for_rounded_interior_point() {
    let raw_hull = [
        Vec2::new(0.079693034, 0.8015764),
        Vec2::new(0.2550272, -0.06383644),
        Vec2::new(0.8354348, -0.8882848),
    ];
    let interior = Vec2::new(0.59929717, -0.55251503);
    let packed_interior = packed_xy(interior);
    let true_hull = convex_hull_xy(raw_hull.to_vec());
    let mut hull_points = Vec::with_capacity(true_hull.len() * 2);
    hull_points.extend_from_slice(&true_hull);
    hull_points.extend(true_hull.iter().copied().map(packed_xy));
    let hull_with_only_rounded_hull_points = convex_hull_xy(hull_points);

    assert_eq!(true_hull.len(), 3);
    assert!(point_in_convex_polygon(interior, &true_hull));
    assert!(!point_in_convex_polygon(packed_interior, &hull_with_only_rounded_hull_points));

    let vertices = [
        vertex(raw_hull[0].x, raw_hull[0].y, 1.0),
        vertex(raw_hull[1].x, raw_hull[1].y, 2.0),
        vertex(raw_hull[2].x, raw_hull[2].y, 3.0),
        vertex(interior.x, interior.y, 4.0),
    ];
    let footprint = horizon_footprint_from_vertices(&vertices, aabb(0.0, -0.9, 0.9, 0.9, 4.0));
    let points = footprint_points(footprint);

    assert!(footprint.vertex_count > 0);
    assert!(point_in_convex_polygon(packed_interior, &points));
}
