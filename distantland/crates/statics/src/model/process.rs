//! meshopt-backed geometry processing and reusable per-thread workspaces.

use std::mem::{align_of, offset_of, size_of};
use std::time::{Duration, Instant};

use bytemuck::{Pod, Zeroable, must_cast_slice};
use glam::Vec3;
use hashbrown::HashSet;
use minsphere::BoundingSphereScratch;
use rayon::prelude::*;

use crate::DistantStatics;

use super::{
    DistantStatic, MergedComponent, StaticMeshSimplifierConfig, Subset, UvBound, Vertex, components_tile_triangle_count,
};

/// Per-thread reusable buffers for static-mesh processing. Rayon gives each worker its own
/// instance, so buffers grow to a high-water mark once per worker.
#[derive(Default)]
pub(crate) struct StaticMeshContext {
    indices32: Vec<u32>,
    vertex_lock: Vec<bool>,
    weld_keys: Vec<WeldKey>,
    remap: Vec<u32>,
    /// Index buffer with [`StaticMeshContext::remap`] applied, then optimized in place.
    ///
    /// Pure scratch: it is narrowed into the returned `[u16; 3]` triangles and never escapes
    /// [`optimize_geometry`].
    remapped_indices: Vec<u32>,
    sphere_points: Vec<[f32; 3]>,
    sphere_scratch: BoundingSphereScratch,
    /// Accumulated signed geometric face normals per vertex, the intermediate used to
    /// derive [`StaticMeshContext::face_orientation_keys`] for the alpha-subset weld guard.
    face_orientation_sums: Vec<Vec3>,
    /// Compact per-vertex orientation keys (see [`quantize_face_orientation`]).
    ///
    /// Key `0` is reserved for ambiguous (near-zero accumulated normal) vertices in alpha
    /// subsets, which the weld predicate treats as non-weldable so two-sided foliage-card
    /// geometry is never quietly merged on authored attributes alone.
    face_orientation_keys: Vec<u32>,
}

/// Deterministic meshoptimizer weld key. Quantized attributes and exact normalized `uv_bound`
/// bits make the equivalence transitive; ambiguous alpha orientation and non-finite data receive
/// a tiebreaker.
#[derive(Clone, Copy, Default, Pod, Zeroable)]
#[repr(C)]
struct WeldKey {
    pos: [i32; 3],
    normal: [i32; 3],
    uv: [i32; 2],
    color: [i32; 4],
    uv_bound: [u32; 4],
    orient: u32,
    tiebreak: u32,
}

#[derive(Default)]
struct WeldKeyStats {
    ambiguous_alpha_orientation_count: usize,
    non_finite_vertex_count: usize,
    non_weldable_vertex_count: usize,
}

/// Warn when one static spends this long in the optimization pipeline.
const SLOW_STATIC_WARN_AFTER: Duration = Duration::from_secs(5);
/// Warn when one subset spends this long in a single optimize pass.
const SLOW_SUBSET_WARN_AFTER: Duration = Duration::from_secs(2);

/// Position weld grid, in world units.
const POSITION_WELD_CELL: f32 = 0.01;
/// UV quantization cell, sized so same-bin vertices stay within a 0.01 L2 threshold.
const UV_WELD_CELL: f32 = 0.005;
/// Color quantization cell, sized so same-bin vertices stay within a 0.01 L2 threshold.
const COLOR_WELD_CELL: f32 = 0.004;
/// Normal quantization cell, sized so same-bin vertices stay within a 0.1 L2 threshold.
const NORMAL_WELD_CELL: f32 = 0.05;

/// Relative target selected for an absolute merge-stage simplification request.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct SimplificationTarget {
    /// Bucketed absolute error divided by this subset's maximum AABB-axis extent.
    pub(crate) requested: f32,
    /// Requested target after applying the configured per-subset relative cap.
    pub(crate) effective: f32,
    /// Whether the configured cap reduced the requested target.
    pub(crate) capped: bool,
    /// Whether another meshopt pass can use a larger target than the initial pass.
    pub(crate) should_simplify: bool,
}

fn optimize_subsets(ds: &mut DistantStatic, context: &mut StaticMeshContext, mesh_path: &str) {
    for (subset_index, subset) in ds.subsets.iter_mut().enumerate() {
        subset.optimize_with(context, mesh_path, subset_index);
    }
}

fn simplify_subsets(ds: &mut DistantStatic, context: &mut StaticMeshContext, config: StaticMeshSimplifierConfig) {
    for subset in &mut ds.subsets {
        subset.simplify_with(config, context);
    }
}

pub(crate) fn update_bounds_with_context(ds: &mut DistantStatic, context: &mut StaticMeshContext) {
    ds.discard_empty_subsets();
    for subset in &mut ds.subsets {
        subset.update_bounds_with(&mut context.sphere_points, &mut context.sphere_scratch);
    }
    ds.update_bounds_from_subsets();
}

/// Runs the full static-mesh optimization pass in parallel.
pub fn optimize_statics(distant_statics: &mut DistantStatics, config: StaticMeshSimplifierConfig) {
    distant_statics
        .par_iter_mut()
        .for_each_init(StaticMeshContext::default, |context, (mesh_path, ds)| {
            optimize_static(context, mesh_path, ds, config);
        });
}

/// Runs the optimization pass only for entries named in `keys`.
pub fn optimize_statics_keys(
    distant_statics: &mut DistantStatics,
    keys: &HashSet<String>,
    config: StaticMeshSimplifierConfig,
) {
    distant_statics
        .par_iter_mut()
        .filter(|(mesh_path, _)| keys.contains(mesh_path.as_str()))
        .for_each_init(StaticMeshContext::default, |context, (mesh_path, ds)| {
            optimize_static(context, mesh_path, ds, config);
        });
}

fn optimize_static(
    context: &mut StaticMeshContext,
    mesh_path: &str,
    ds: &mut DistantStatic,
    config: StaticMeshSimplifierConfig,
) {
    // Keep this order: optimize -> merge -> simplify eligible subsets -> merge -> optimize ->
    // update bounds.
    let started_at = Instant::now();
    optimize_subsets(ds, context, mesh_path);
    ds.merge_subsets();
    simplify_subsets(ds, context, config);
    ds.merge_subsets();
    optimize_subsets(ds, context, mesh_path);
    update_bounds_with_context(ds, context);

    let elapsed = started_at.elapsed();
    if elapsed >= SLOW_STATIC_WARN_AFTER {
        tracing::warn!(
            mesh_path,
            elapsed_ms = elapsed.as_millis() as u64,
            subset_count = ds.subsets.len(),
            "Slow static mesh optimization"
        );
    }
}

impl Subset {
    /// Alpha-tested cards skip triangle-count-reducing simplification because their silhouette is
    /// encoded in texture alpha.
    pub(crate) fn allows_simplification(&self) -> bool {
        !self.has_alpha
    }

    /// Simplifies with reusable workspace buffers. UV weight is computed from average triangle
    /// UV area; alpha-tested subsets skip decimation but still run optimization and merging.
    pub(super) fn simplify_with(&mut self, config: StaticMeshSimplifierConfig, workspace: &mut StaticMeshContext) {
        if !self.allows_simplification() {
            return;
        }
        self.simplify_with_error(config, config.target_error, workspace);
    }

    /// Test-only in-place form of the absolute-error merge simplification.
    ///
    /// It mirrors the borrowed-input merge-LOD path for unit-test equivalence checks.
    #[cfg(test)]
    pub(crate) fn simplify_absolute_with(
        &mut self,
        config: StaticMeshSimplifierConfig,
        absolute_error: f32,
        workspace: &mut StaticMeshContext,
    ) -> SimplificationTarget {
        let target = self.absolute_simplification_target(config, absolute_error);
        if target.should_simplify {
            self.simplify_with_error(config, target.effective, workspace);
        }
        target
    }

    pub(crate) fn absolute_simplification_target(
        &self,
        config: StaticMeshSimplifierConfig,
        absolute_error: f32,
    ) -> SimplificationTarget {
        if !self.allows_simplification() {
            return SimplificationTarget::default();
        }

        let mut min = Vec3::MAX;
        let mut max = Vec3::MIN;
        for vertex in &self.vertices {
            min = min.min(vertex.position);
            max = max.max(vertex.position);
        }

        let extent = (max - min).max_element();
        Self::absolute_simplification_target_for_extent(config, absolute_error, extent)
    }

    pub(crate) fn absolute_simplification_target_with_extent(
        &self,
        config: StaticMeshSimplifierConfig,
        absolute_error: f32,
        extent: f32,
    ) -> SimplificationTarget {
        if !self.allows_simplification() {
            return SimplificationTarget::default();
        }

        Self::absolute_simplification_target_for_extent(config, absolute_error, extent)
    }

    /// Converts an absolute merge-stage error into a meshopt-relative target, caps it at the
    /// configured merge multiplier, and reports whether it exceeds the initial target.
    pub(crate) fn absolute_simplification_target_for_extent(
        config: StaticMeshSimplifierConfig,
        absolute_error: f32,
        extent: f32,
    ) -> SimplificationTarget {
        if extent <= 0.0 || !extent.is_finite() {
            return SimplificationTarget::default();
        }

        let requested = absolute_error / extent;
        let maximum = config.target_error * config.merge_error_multiplier;
        let effective = requested.min(maximum).max(config.target_error);
        SimplificationTarget {
            requested,
            effective,
            capped: requested > maximum,
            should_simplify: effective > config.target_error,
        }
    }

    pub(super) fn simplify_with_error(
        &mut self,
        config: StaticMeshSimplifierConfig,
        target_error: f32,
        workspace: &mut StaticMeshContext,
    ) {
        if !self.allows_simplification() || self.vertices.is_empty() || self.triangles.is_empty() {
            return;
        }

        let result = simplify_indices(&self.vertices, &self.triangles, config, target_error, workspace);
        write_indices_to_triangles(&result, &mut self.triangles);
    }

    /// Builds an owned merge-LOD subset from borrowed source geometry without deep-cloning first.
    ///
    /// Applies the absolute target when it permits more error than the initial pass, then runs
    /// the shared optimization core into newly allocated final buffers while copying metadata.
    pub(crate) fn build_merge_lod_from(
        source: &Subset,
        config: StaticMeshSimplifierConfig,
        absolute_error: f32,
        workspace: &mut StaticMeshContext,
        mesh_path: &str,
        subset_index: usize,
    ) -> Subset {
        let target = source.absolute_simplification_target(config, absolute_error);

        if source.vertices.is_empty() || source.triangles.is_empty() {
            return Subset {
                bounding_sphere: source.bounding_sphere,
                bounding_box: source.bounding_box,
                vertices: source.vertices.clone(),
                triangles: source.triangles.clone(),
                components: source.components.clone(),
                has_alpha: source.has_alpha,
                has_uv_controller: source.has_uv_controller,
                emissive: source.emissive,
                texture: source.texture,
            };
        }

        if target.should_simplify && source.allows_simplification() {
            workspace.indices32 = simplify_indices(&source.vertices, &source.triangles, config, target.effective, workspace);
        } else {
            workspace.indices32.clear();
            workspace
                .indices32
                .extend(source.triangles.as_flattened().iter().map(|&i| i as u32));
        }

        if workspace.indices32.is_empty() {
            return Subset {
                bounding_sphere: source.bounding_sphere,
                bounding_box: source.bounding_box,
                vertices: source.vertices.clone(),
                triangles: Vec::new(),
                components: source.components.clone(),
                has_alpha: source.has_alpha,
                has_uv_controller: source.has_uv_controller,
                emissive: source.emissive,
                texture: source.texture,
            };
        }

        let (vertices, triangles) = optimize_geometry(
            &source.vertices,
            source.has_alpha,
            &source.components,
            workspace,
            mesh_path,
            subset_index,
        );

        Subset {
            bounding_sphere: source.bounding_sphere,
            bounding_box: source.bounding_box,
            vertices,
            triangles,
            components: source.components.clone(),
            has_alpha: source.has_alpha,
            has_uv_controller: source.has_uv_controller,
            emissive: source.emissive,
            texture: source.texture,
        }
    }

    /// Optimizes with reusable workspace buffers. Alpha subsets include face orientation in their
    /// weld keys so opposite-winding foliage-card vertices are not collapsed; ambiguous or
    /// non-finite vertices receive a tiebreaker and remain non-weldable.
    pub(crate) fn optimize_with(&mut self, workspace: &mut StaticMeshContext, mesh_path: &str, subset_index: usize) {
        if self.vertices.is_empty() || self.triangles.is_empty() {
            return;
        }

        workspace.indices32.clear();
        workspace
            .indices32
            .extend(self.triangles.as_flattened().iter().map(|&i| i as u32));

        let (vertices, triangles) = optimize_geometry(
            &self.vertices,
            self.has_alpha,
            &self.components,
            workspace,
            mesh_path,
            subset_index,
        );
        self.vertices = vertices;
        self.triangles = triangles;
    }
}

/// Simplifies triangle indices with a meshopt-relative target.
fn simplify_indices(
    vertices: &[Vertex],
    triangles: &[[u16; 3]],
    config: StaticMeshSimplifierConfig,
    target_error: f32,
    workspace: &mut StaticMeshContext,
) -> Vec<u32> {
    // Widen 16-bit indices to 32-bit into workspace.
    workspace.indices32.clear();
    workspace.indices32.extend(triangles.as_flattened().iter().map(|&i| i as u32));

    let uv_weight = uv_weight(vertices, &workspace.indices32);

    // Attribute buffer: normal(3) + uv(2) + color(4) = 9 floats per vertex, contiguous
    // at offset 12 within each Vertex. uv_bound is metadata, not a shading attribute.
    // Guard against layout changes at compile time.
    const {
        assert!(offset_of!(Vertex, normal) == 12);
        assert!(offset_of!(Vertex, uv) == 24);
        assert!(offset_of!(Vertex, color) == 32);
        assert!(offset_of!(Vertex, uv_bound) == 48);
        assert!(size_of::<Vertex>() == 64);
        // `Vec4` forces 16-byte alignment, so a nominally smaller field layout pads
        // back to 64 bytes. Shrinking `Vertex` requires changing this first.
        assert!(align_of::<Vertex>() == 16);
    }
    let vertex_bytes: &[u8] = bytemuck::must_cast_slice(vertices);

    // Safety: `attr_bytes` points into `Vertex` data, starting at the `normal` field,
    // it's aligned to 4 bytes (f32). Length is a multiple of 4 since vertex size is 64.
    let attr_offset = offset_of!(Vertex, normal);
    let attr_bytes = &vertex_bytes[attr_offset..];
    let vertex_attributes: &[f32] = bytemuck::cast_slice(attr_bytes);

    // Subsets with near-uniform vertex colors (e.g. meshes extracted without NIF colors
    // are all-white) gain nothing from color quadrics; a zero weight makes meshopt drop
    // all four color channels internally, shrinking attribute quadrics from 9 to 5.
    // Tolerance matches the vertex-equivalence color threshold used in `optimize_with`.
    let first_color = vertices[0].color;
    let color_weight = if vertices.iter().all(|v| (v.color - first_color).length_squared() < 1e-4) {
        0.0
    } else {
        config.color_weight
    };

    // Weights: normals are normalized (-1..1),
    // UVs use recommended weight based on average triangle UV area (see `uv_weight`),
    // colors are normalized (0..1).
    let normal_weight = config.normal_weight;
    let attribute_weights: &[f32] = &[
        normal_weight,
        normal_weight,
        normal_weight,
        uv_weight,
        uv_weight,
        color_weight,
        color_weight,
        color_weight,
        color_weight,
    ];

    // Reuse the lock buffer; all entries are false (no vertices locked).
    workspace.vertex_lock.clear();
    workspace.vertex_lock.resize(vertices.len(), false);

    let options = {
        meshopt::SimplifyOptions::LockBorder // .
        // | meshopt::SimplifyOptions::Prune
        // | meshopt::SimplifyOptions::Permissive
        // | meshopt::SimplifyOptions::ErrorAbsolute
    };

    let target_count = 0;

    meshopt::simplify_with_attributes_and_locks(
        &workspace.indices32,
        &vertex_adapter(vertices),
        vertex_attributes,
        attribute_weights,
        size_of::<Vertex>(),
        &workspace.vertex_lock,
        target_count,
        target_error,
        options,
        None,
    )
}

/// Computes a recommended simplification weight for UV attributes.
///
/// The weight is the inverse square root of the average per-triangle UV area, so textures that
/// cover a small UV region get a proportionally higher weight and are better preserved during
/// simplification. Falls back to `10.0` when the UV area is effectively zero (flat or
/// degenerate UVs).
#[rustfmt::skip]
fn uv_weight(vertices: &[Vertex], indices: &[u32]) -> f32 {
    let (chunks, _) = indices.as_chunks::<3>();
    let uv_area_sum: f32 = chunks
        .iter()
        .map(|&[i, j, k]| {
            let uv1 = vertices[i as usize].uv;
            let uv2 = vertices[j as usize].uv;
            let uv3 = vertices[k as usize].uv;

            let cross = (uv2.x - uv1.x)
                      * (uv3.y - uv1.y)
                      - (uv3.x - uv1.x)
                      * (uv2.y - uv1.y);

            0.5 * cross.abs()
        })
        .sum();

    let tri_count = indices.len() / 3;
    let avg_uv_area = if tri_count > 0 {
        uv_area_sum / (tri_count as f32)
    } else {
        0.0
    };

    if avg_uv_area > 0.0 {
        1.0 / avg_uv_area.sqrt()
    } else {
        10.0
    }
}

/// Runs remap/weld, vertex-cache, overdraw, and vertex-fetch optimization without mutating input.
fn optimize_geometry(
    vertices: &[Vertex],
    has_alpha: bool,
    components: &[MergedComponent],
    workspace: &mut StaticMeshContext,
    mesh_path: &str,
    subset_index: usize,
) -> (Vec<Vertex>, Vec<[u16; 3]>) {
    let started_at = Instant::now();
    let source_vertex_count = vertices.len();
    let source_triangle_count = workspace.indices32.len() / 3;

    // Alpha subsets use a face-orientation weld key so opposite-winding foliage cards remain
    // distinct; opaque subsets keep attribute-only equivalence.
    if has_alpha {
        compute_face_orientation_keys(
            vertices,
            &workspace.indices32,
            &mut workspace.face_orientation_sums,
            &mut workspace.face_orientation_keys,
        );
    } else {
        workspace.face_orientation_keys.clear();
    }

    let key_stats = build_weld_keys(
        vertices,
        has_alpha,
        &workspace.face_orientation_keys,
        &mut workspace.weld_keys,
    );

    let unique_count = generate_vertex_remap_into(&workspace.weld_keys, &workspace.indices32, &mut workspace.remap)
        .expect("non-empty subsets always have indices");

    remap_indices_into(&workspace.indices32, &workspace.remap, &mut workspace.remapped_indices);
    let mut remapped_vertices = meshopt::remap_vertex_buffer(vertices, unique_count, &workspace.remap);
    let indices = workspace.remapped_indices.as_mut_slice();

    if components.is_empty() {
        meshopt::optimize_vertex_cache_in_place(indices, remapped_vertices.len());
        meshopt::optimize_overdraw_in_place(indices, &vertex_adapter(&remapped_vertices), 1.05);
    } else {
        debug_assert!(
            components_tile_triangle_count(components, source_triangle_count as u32),
            "component ranges must tile the subset before partition-aware optimization"
        );
        let vertex_adapter = vertex_adapter(&remapped_vertices);
        for component in components {
            let start = component.first_triangle as usize * 3;
            let end = (component.first_triangle + component.triangle_count) as usize * 3;
            meshopt::optimize_vertex_cache_in_place(&mut indices[start..end], remapped_vertices.len());
            meshopt::optimize_overdraw_in_place(&mut indices[start..end], &vertex_adapter, 1.05);
        }
    }

    let next_vertex = meshopt::optimize_vertex_fetch_in_place(indices, &mut remapped_vertices);
    remapped_vertices.truncate(next_vertex);

    let mut triangles = Vec::new();
    write_indices_to_triangles(indices, &mut triangles);

    let elapsed = started_at.elapsed();
    if elapsed >= SLOW_SUBSET_WARN_AFTER {
        tracing::warn!(
            mesh_path,
            subset_index,
            has_alpha,
            source_vertex_count,
            source_triangle_count,
            output_vertex_count = remapped_vertices.len(),
            output_triangle_count = triangles.len(),
            ambiguous_alpha_orientation_count = key_stats.ambiguous_alpha_orientation_count,
            non_finite_vertex_count = key_stats.non_finite_vertex_count,
            non_weldable_vertex_count = key_stats.non_weldable_vertex_count,
            elapsed_ms = elapsed.as_millis() as u64,
            "Slow static subset optimization"
        );
    }

    (remapped_vertices, triangles)
}

/// Computes compact per-vertex face-orientation keys for alpha-subset welding.
fn compute_face_orientation_keys(vertices: &[Vertex], indices: &[u32], sums: &mut Vec<Vec3>, keys: &mut Vec<u32>) {
    let n = vertices.len();

    sums.clear();
    sums.resize(n, Vec3::ZERO);

    // Accumulate signed face normals into each triangle's three vertices.
    let (chunks, _) = indices.as_chunks::<3>();
    for &[i0, i1, i2] in chunks {
        let p0 = vertices[i0 as usize].position;
        let p1 = vertices[i1 as usize].position;
        let p2 = vertices[i2 as usize].position;
        let face = (p1 - p0).cross(p2 - p0);
        // Skip degenerate triangles, whose winding carries no orientation signal.
        if face.length_squared() < 1e-12 {
            continue;
        }
        sums[i0 as usize] += face;
        sums[i1 as usize] += face;
        sums[i2 as usize] += face;
    }

    keys.clear();
    keys.resize(n, 0u32);
    for (sum, key) in sums.iter().zip(keys.iter_mut()) {
        *key = quantize_face_orientation(*sum);
    }
}

fn build_weld_keys(vertices: &[Vertex], has_alpha: bool, orientation_keys: &[u32], keys: &mut Vec<WeldKey>) -> WeldKeyStats {
    keys.clear();
    keys.resize(vertices.len(), WeldKey::default());

    let mut stats = WeldKeyStats::default();

    for (index, (vertex, key)) in vertices.iter().zip(keys.iter_mut()).enumerate() {
        let mut non_finite = false;
        let mut force_tiebreak = false;

        if let Some(pos) = quantize_lanes(vertex.position.to_array(), POSITION_WELD_CELL) {
            key.pos = pos;
        } else {
            non_finite = true;
            force_tiebreak = true;
        }

        if let Some(normal) = quantize_lanes(vertex.normal.to_array(), NORMAL_WELD_CELL) {
            key.normal = normal;
        } else {
            non_finite = true;
            force_tiebreak = true;
        }

        if let Some(uv) = quantize_lanes(vertex.uv.to_array(), UV_WELD_CELL) {
            key.uv = uv;
        } else {
            non_finite = true;
            force_tiebreak = true;
        }

        if let Some(color) = quantize_lanes(vertex.color.to_array(), COLOR_WELD_CELL) {
            key.color = color;
        } else {
            non_finite = true;
            force_tiebreak = true;
        }

        if let Some(uv_bound) = uv_bound_bits(vertex.uv_bound) {
            key.uv_bound = uv_bound;
        } else {
            non_finite = true;
            force_tiebreak = true;
        }

        if has_alpha {
            key.orient = orientation_keys.get(index).copied().unwrap_or(0);
            if key.orient == 0 {
                stats.ambiguous_alpha_orientation_count += 1;
                force_tiebreak = true;
            }
        } else {
            key.orient = 0;
        }

        if non_finite {
            stats.non_finite_vertex_count += 1;
        }
        if force_tiebreak {
            key.tiebreak = vertex_tiebreak(index);
            stats.non_weldable_vertex_count += 1;
        } else {
            key.tiebreak = 0;
        }
    }

    stats
}

fn quantize_component(value: f32, cell: f32) -> Option<i32> {
    let quantized = (value / cell).round();
    (quantized.is_finite() && quantized >= i32::MIN as f32 && quantized <= i32::MAX as f32).then_some(quantized as i32)
}

fn quantize_lanes<const N: usize>(values: [f32; N], cell: f32) -> Option<[i32; N]> {
    let mut lanes = [0_i32; N];
    for (lane, value) in lanes.iter_mut().zip(values) {
        *lane = quantize_component(value, cell)?;
    }
    Some(lanes)
}

fn uv_bound_bits(uv_bound: UvBound) -> Option<[u32; 4]> {
    Some([
        normalized_f32_bits(uv_bound.min_y)?,
        normalized_f32_bits(uv_bound.max_x)?,
        normalized_f32_bits(uv_bound.min_x)?,
        normalized_f32_bits(uv_bound.max_y)?,
    ])
}

fn normalized_f32_bits(value: f32) -> Option<u32> {
    value
        .is_finite()
        .then(|| if value == 0.0 { 0.0f32.to_bits() } else { value.to_bits() })
}

fn vertex_tiebreak(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX - 1).saturating_add(1)
}

/// Quantizes a per-vertex accumulated face-normal direction into a compact key.
///
/// Returns the sentinel `0` when the accumulated direction is near-zero, i.e. the vertex's
/// incident triangles have cancelling windings (ambiguous orientation). Such vertices are
/// treated as non-weldable in alpha subsets to avoid quietly merging two-sided foliage-card
/// geometry.
///
/// Otherwise encodes the normalized direction as three 3-bit components (8 levels per axis,
/// 9 bits total) packed into a `u32`, offset by one so real keys never collide with the
/// sentinel. The grid is deliberately coarse: the goal is only to keep opposite or
/// substantially different face orientations apart, not to measure precise normals.
fn quantize_face_orientation(sum: Vec3) -> u32 {
    /// Squared-length threshold below which an accumulated normal is treated as ambiguous
    /// (near-zero) and assigned the non-weldable sentinel key.
    const NEAR_ZERO_SQ: f32 = 1e-12;

    if sum.length_squared() < NEAR_ZERO_SQ {
        return 0;
    }

    let n = sum.normalize();
    // Guard against NaN from non-finite input positions.
    if !n.is_finite() {
        return 0;
    }

    // Map each component from [-1, 1] to [0, 7] (8 levels, 3 bits).
    let quantize = |c: f32| -> u32 { (((c + 1.0) * 0.5 * 7.0).round()).clamp(0.0, 7.0) as u32 };
    let qx = quantize(n.x);
    let qy = quantize(n.y);
    let qz = quantize(n.z);
    // Offset by 1 so the packed value never equals the sentinel 0. The addition must
    // wrap the *entire* packed field; grouping it as `(1 + (qx << 6)) | ...` would force
    // bit 0 and mask the low bit of `qz`, halving z-axis resolution.
    1 + ((qx << 6) | (qy << 3) | qz)
}

/// Scratch-backed replacement for [`meshopt::remap_index_buffer`].
///
/// The safe wrapper allocates and zero-fills a fresh vector; this reproduces its
/// `destination[i] = remap[indices[i]]` mapping into caller-owned scratch storage.
fn remap_indices_into(indices: &[u32], remap: &[u32], out: &mut Vec<u32>) {
    debug_assert!(indices.iter().all(|&index| (index as usize) < remap.len()));

    out.clear();
    out.extend(indices.iter().map(|&index| remap[index as usize]));
}

/// Scratch-backed wrapper around [`meshopt::ffi::meshopt_generateVertexRemap`].
///
/// `keys` must be padding-free POD data because meshoptimizer hashes and compares all bytes in
/// each key. The remap table is resized in caller-owned scratch storage to avoid the allocation in
/// the safe `meshopt::generate_vertex_remap` wrapper.
fn generate_vertex_remap_into(keys: &[WeldKey], indices: &[u32], remap: &mut Vec<u32>) -> Option<usize> {
    if indices.is_empty() {
        return None;
    }

    debug_assert!(indices.iter().all(|&index| (index as usize) < keys.len()));

    remap.clear();
    remap.resize(keys.len(), 0u32);

    let unique_count = unsafe {
        // SAFETY: `remap` has one entry per key, `indices` points to initialized u32 index data,
        // and `WeldKey` is `Pod` with no uninitialized padding bytes for meshoptimizer to hash.
        meshopt::ffi::meshopt_generateVertexRemap(
            remap.as_mut_ptr(),
            indices.as_ptr(),
            indices.len(),
            keys.as_ptr().cast(),
            keys.len(),
            size_of::<WeldKey>(),
        )
    };

    Some(unique_count)
}

/// Narrows 32-bit indices to 16-bit and writes them as `[u16; 3]` triangles.
///
/// Reuses the capacity of `triangles`. Panics if the index count is not a multiple of 3.
#[inline(always)]
fn write_indices_to_triangles(indices: &[u32], triangles: &mut Vec<[u16; 3]>) {
    let (chunks, _) = indices.as_chunks::<3>();
    triangles.clear();
    triangles.extend(
        // Morrowind static meshes and MGE-XE distant statics limit subsets to
        // at most u16::MAX (65,535) vertices, which is enforced during merging and loading.
        // Since meshopt only generates indices within the bounds of the input vertex buffer,
        // no index can ever exceed u16::MAX, meaning these truncation casts will never overflow.
        chunks.iter().map(|&[i0, i1, i2]| [i0 as u16, i1 as u16, i2 as u16]),
    )
}

fn vertex_adapter(vertices: &[Vertex]) -> meshopt::VertexDataAdapter<'_> {
    meshopt::VertexDataAdapter::new(must_cast_slice(vertices), size_of::<Vertex>(), offset_of!(Vertex, position)).unwrap()
}

#[cfg(test)]
mod selective_tests {
    use super::*;
    use glam::Vec4;

    fn duplicate_triangle_static() -> DistantStatic {
        let positions = [Vec3::ZERO, Vec3::X, Vec3::Y];
        let vertices = positions
            .into_iter()
            .chain(positions)
            .map(|position| Vertex {
                position,
                normal: Vec3::Z,
                color: Vec4::ONE,
                ..Vertex::default()
            })
            .collect();
        DistantStatic {
            subsets: vec![Subset {
                vertices,
                triangles: vec![[0, 1, 2], [3, 4, 5]],
                has_alpha: true,
                ..Subset::default()
            }],
            ..DistantStatic::default()
        }
    }

    #[test]
    fn keyed_optimize_matches_full_output_and_leaves_other_entries_untouched() {
        let mut full = DistantStatics::from_iter([
            ("selected.nif".to_owned(), duplicate_triangle_static()),
            ("skipped.nif".to_owned(), duplicate_triangle_static()),
        ]);
        let mut selective = full.clone();
        optimize_statics(&mut full, StaticMeshSimplifierConfig::default());
        optimize_statics_keys(
            &mut selective,
            &HashSet::from(["selected.nif".to_owned()]),
            StaticMeshSimplifierConfig::default(),
        );

        let full_selected = &full["selected.nif"];
        let selective_selected = &selective["selected.nif"];
        assert_eq!(selective_selected.bounding_sphere, full_selected.bounding_sphere);
        assert_eq!(selective_selected.bounding_box, full_selected.bounding_box);
        assert_eq!(selective_selected.subsets.len(), full_selected.subsets.len());
        for (selective_subset, full_subset) in selective_selected.subsets.iter().zip(&full_selected.subsets) {
            assert_eq!(selective_subset.triangles, full_subset.triangles);
            assert_eq!(
                bytemuck::cast_slice::<Vertex, u8>(&selective_subset.vertices),
                bytemuck::cast_slice::<Vertex, u8>(&full_subset.vertices)
            );
        }

        assert_eq!(selective["skipped.nif"].bounding_sphere, Default::default());
        assert_eq!(selective["skipped.nif"].subsets[0].vertices.len(), 6);
        assert!(full["skipped.nif"].subsets[0].vertices.len() < 6);
    }
}
