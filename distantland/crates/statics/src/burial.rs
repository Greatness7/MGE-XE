//! Static-geometry operations applied to parsed plugin usage.

use glam::Vec3Swizzles;

use crate::model::DistantStatic;
use crate::usage::{
    BurialStats, DistantReference, EXPOSED_AREA_RATIO_THRESHOLD, EXPOSED_HEIGHT_EPSILON, MAX_EXPOSED_HEIGHT_THRESHOLD,
    MIN_VALID_TRIANGLE_RATIO, MIN_VALID_TRIANGLES, TerrainCells, terrain_height_at,
};

/// Returns whether a reference's mesh is mostly obscured by terrain.
pub fn is_buried(
    terrain_cells: &TerrainCells<'_>,
    reference: &DistantReference<'_>,
    distant_static: &DistantStatic,
    stats: &mut BurialStats,
) -> bool {
    stats.refs_considered += 1;
    let transform = reference.get_transform();

    let mut sampled = 0usize;
    let mut valid = 0usize;
    let mut total_area = 0.0f32;
    let mut exposed_area = 0.0f32;
    let mut max_exposed_height = 0.0f32;

    for subset in &distant_static.subsets {
        let world_sphere = subset.bounding_sphere.transformed_by(&transform);
        let sphere_terrain_z = terrain_height_at(terrain_cells, world_sphere.center.xy());

        if sphere_terrain_z.is_some_and(|height| world_sphere.center.z + world_sphere.radius < height) {
            sampled += subset.triangles.len();
            continue;
        }

        let world_triangle = |triangle: &[u16; 3]| {
            let corners = triangle.map(|index| transform.transform_point3(subset.vertices[index as usize].position));
            let [a, b, c] = corners;
            (corners, 0.5 * (b - a).cross(c - a).length())
        };

        let clearance = sphere_terrain_z
            .map(|height| world_sphere.center.z - world_sphere.radius - height)
            .filter(|height| *height > EXPOSED_HEIGHT_EPSILON);

        if clearance.is_some_and(|height| height >= MAX_EXPOSED_HEIGHT_THRESHOLD)
            && subset
                .triangles
                .iter()
                .any(|triangle| world_triangle(triangle).1 > f32::EPSILON)
        {
            stats.keep_clearance_shortcut += 1;
            return false;
        }

        for triangle in &subset.triangles {
            sampled += 1;
            stats.tris_visited += 1;

            let ([a, b, c], area) = world_triangle(triangle);
            if area <= f32::EPSILON {
                continue;
            }

            // `height` drives the exposed-area ratio and stays centroid-based, so the area
            // statistics keep their original meaning. `exposure` drives the early-keep test and
            // additionally considers the corners: a triangle may span many terrain quads, and a
            // centroid sitting under a ridge says nothing about a corner standing clear of it.
            // Without this, coarse geometry (large flat panels such as the Ghostfence) reads as
            // buried while over a kilometre of it is plainly visible.
            let (height, exposure) = match clearance {
                Some(height) => (height, height),
                None => {
                    let centroid = (a + b + c) / 3.0;
                    stats.centroid_height_samples += 1;
                    let Some(terrain_height) = terrain_height_at(terrain_cells, centroid.xy()) else {
                        continue;
                    };
                    let centroid_height = centroid.z - terrain_height;

                    // Only worth sampling corners while the triangle still looks buried; once the
                    // centroid alone clears the threshold the early-keep below fires regardless.
                    let mut exposure = centroid_height;
                    if exposure < MAX_EXPOSED_HEIGHT_THRESHOLD {
                        for corner in [a, b, c] {
                            if let Some(terrain_height) = terrain_height_at(terrain_cells, corner.xy()) {
                                exposure = exposure.max(corner.z - terrain_height);
                            }
                        }
                    }

                    (centroid_height, exposure)
                }
            };

            valid += 1;
            total_area += area;
            max_exposed_height = max_exposed_height.max(exposure);
            if height > EXPOSED_HEIGHT_EPSILON {
                exposed_area += area;
            }
            if max_exposed_height >= MAX_EXPOSED_HEIGHT_THRESHOLD {
                stats.keep_height_early += 1;
                return false;
            }
        }
    }

    let valid_threshold = ((sampled as f32 * MIN_VALID_TRIANGLE_RATIO).ceil() as usize)
        .max(MIN_VALID_TRIANGLES)
        .min(sampled);

    if valid < valid_threshold || total_area <= 0.0 {
        stats.keep_insufficient += 1;
        return false;
    }

    let buried =
        max_exposed_height < MAX_EXPOSED_HEIGHT_THRESHOLD && exposed_area / total_area < EXPOSED_AREA_RATIO_THRESHOLD;
    if buried {
        stats.buried += 1;
    } else {
        stats.keep_exposed += 1;
    }
    buried
}
