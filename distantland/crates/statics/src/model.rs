//! Intermediate 32-bit distant-static geometry and conversion helpers.
//!
//! The data model is wire-coupled and stable; meshopt processing lives in `process` and
//! serialization into the `static_meshes` records lives in `pack`.

mod horizon;
mod pack;
mod process;

pub(crate) use process::{StaticMeshContext, update_bounds_with_context};
pub use process::{optimize_statics, optimize_statics_keys};

use std::mem::take;

use bytemuck::{Pod, Zeroable};
use glam::{Affine3A, Vec2, Vec3, Vec4};
use minsphere::{BoundingSphere, BoundingSphereScratch};
use tes3::nif::*;

use crate::mge_xe::distant_statics::*;
use crate::usage::{TerrainCells, terrain_height_at};
use crate::vfs::TextureSym;

fn scaled_cutoff_radius(radius: f32, static_type: StaticType, is_door: bool, scale: f32, door_size_multiplier: f32) -> f32 {
    let building_multiplier = if matches!(static_type, StaticType::StaticBuilding) {
        2.0
    } else {
        1.0
    };
    let door_multiplier = if is_door { door_size_multiplier } else { 1.0 };
    radius * building_multiplier * door_multiplier * scale
}

pub(crate) fn passes_min_radius(
    radius: f32,
    static_type: StaticType,
    is_door: bool,
    scale: f32,
    min_radius: f32,
    door_size_multiplier: f32,
) -> bool {
    // An explicit distance tier is an author's statement that the mesh belongs in distant land no
    // matter how small it is. Mods like Distant Lights rely on `= far` to keep lanterns, torches
    // and lit windows that sit far below the cutoff. `inferred_static_type` never produces these
    // three, so they only ever arrive from an override entry; Tree and Building are left subject to
    // the cutoff because those are also inferred from the mesh path.
    if matches!(
        static_type,
        StaticType::StaticGrass | StaticType::StaticNear | StaticType::StaticFar | StaticType::StaticVeryFar
    ) {
        return true;
    }
    scaled_cutoff_radius(radius, static_type, is_door, scale, door_size_multiplier) >= min_radius
}

/// Returns whether a static's effective cutoff radius meets the configured minimum.
pub fn passes_static_min_radius(distant_static: &DistantStatic, min_radius: f32, door_size_multiplier: f32) -> bool {
    passes_min_radius(
        distant_static.bounding_sphere.radius,
        distant_static.static_type,
        distant_static.is_door,
        distant_static.max_scale,
        min_radius,
        door_size_multiplier,
    )
}

/// UV bounds assigned after atlas packing for one vertex.
#[derive(Pod, Zeroable, Clone, Copy, Default, PartialEq)]
#[repr(C)]
pub struct UvBound {
    /// Minimum V coordinate.
    pub min_y: f32,
    /// Maximum U coordinate.
    pub max_x: f32,
    /// Minimum U coordinate.
    pub min_x: f32,
    /// Maximum V coordinate.
    pub max_y: f32,
}

impl UvBound {
    /// Returns the palette identity of this bound: its four lanes as raw bits.
    ///
    /// Every place that compares, dedupes, or counts distinct bounds uses this one key. Do not
    /// substitute `PartialEq`: float equality disagrees with bit equality on signed zero
    /// (`-0.0 == 0.0`) and on NaN (`NaN != NaN`), so a sidecar set built one way and a palette
    /// deduped the other can diverge in count — exactly the divergence the writer's cap check
    /// would then trip on.
    pub fn bits(self) -> [u32; 4] {
        [
            self.min_y.to_bits(),
            self.max_x.to_bits(),
            self.min_x.to_bits(),
            self.max_y.to_bits(),
        ]
    }
}

/// Configuration for meshopt simplification and merge-stage error limits.
///
/// UV weight is computed dynamically per subset rather than configured globally.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StaticMeshSimplifierConfig {
    /// Relative simplification error budget (fraction of the maximum AABB-axis extent).
    pub target_error: f32,
    /// Attribute weight broadcast to the three normal components.
    pub normal_weight: f32,
    /// Attribute weight broadcast to the four color components.
    pub color_weight: f32,
    /// Maximum merge-stage relative error as a multiple of `target_error`.
    pub merge_error_multiplier: f32,
}

impl Default for StaticMeshSimplifierConfig {
    fn default() -> Self {
        Self {
            target_error: crate::DEFAULT_STATIC_MESH_TARGET_ERROR,
            normal_weight: crate::DEFAULT_STATIC_MESH_NORMAL_WEIGHT,
            color_weight: crate::DEFAULT_STATIC_MESH_COLOR_WEIGHT,
            merge_error_multiplier: crate::DEFAULT_STATIC_MESH_MERGE_ERROR_MULTIPLIER,
        }
    }
}

/// Uncompressed vertex used during intermediate static processing.
///
/// Converted field-by-field into [`PackedVertex`] for `static_meshes` serialization.
#[derive(Pod, Zeroable, Clone, Copy, Default)]
#[repr(C)]
pub struct Vertex {
    pub position: Vec3,
    pub normal: Vec3,
    pub uv: Vec2,
    pub color: Vec4,
    pub uv_bound: UvBound,
}

/// Texture identity carried by an intermediate static subset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubsetTexture {
    /// Source texture in the VFS texture map.
    Source(TextureSym),
    /// Static atlas page assigned after atlas packing.
    AtlasPage(u32),
}

impl Default for SubsetTexture {
    fn default() -> Self {
        Self::Source(TextureSym::EMPTY)
    }
}

/// Uncompressed subset used during intermediate static processing.
///
/// Converted into [`PackedSubset`] for `static_meshes` serialization.
#[derive(Clone)]
pub struct Subset {
    pub bounding_sphere: NiBound,
    pub bounding_box: BoundingBox,
    pub vertices: Vec<Vertex>,
    pub triangles: Vec<[u16; 3]>,
    /// Source-component provenance for merged synthetic statics.
    ///
    /// Empty means this subset has no provenance and should render at all tiers.
    /// Non-empty records are sorted, contiguous, and tile `triangles` exactly.
    pub components: Vec<MergedComponent>,
    /// Bit-distinct set of the `UvBound`s this subset's vertices carry, maintained incrementally.
    ///
    /// It bounds the palette that packing will build, so merges are refused when the union would
    /// exceed [`UV_BOUND_PALETTE_CAP`]. It may over-approximate — culling can drop every vertex
    /// of a contribution — which is safe: over-approximation only refuses a merge that would have
    /// fit. It must never under-approximate, which is why `extract` seeds it rather than the
    /// atlas stage (see `atlas::uv::update_uv_bounds_from_maps`).
    pub uv_bounds: Vec<UvBound>,
    pub has_alpha: bool,
    pub has_uv_controller: bool,
    /// Average emissive material contribution packed into `PackedVertex.normal[3]`.
    pub emissive: f32,
    pub texture: SubsetTexture,
}

/// Returns whether the bit-distinct union of two subsets' bounds still fits the palette cap.
///
/// Both inputs are already bit-distinct, and both are capped, so the linear scan is bounded by
/// `UV_BOUND_PALETTE_CAP` squared in the worst case and by the ~6-entry mean in practice.
pub(crate) fn uv_bound_union_fits(a: &[UvBound], b: &[UvBound]) -> bool {
    let mut keys: Vec<[u32; 4]> = a.iter().map(|bound| bound.bits()).collect();
    for bound in b {
        let key = bound.bits();
        if !keys.contains(&key) {
            keys.push(key);
            if keys.len() > UV_BOUND_PALETTE_CAP as usize {
                return false;
            }
        }
    }
    true
}

/// Unions `source` into `destination`, keeping the destination bit-distinct.
pub(crate) fn union_uv_bounds(destination: &mut Vec<UvBound>, source: &[UvBound]) {
    for bound in source {
        let key = bound.bits();
        if !destination.iter().any(|existing| existing.bits() == key) {
            destination.push(*bound);
        }
    }
}

/// Provenance of one appended run of source geometry inside a merged subset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MergedComponent {
    /// First triangle in the owning subset.
    pub first_triangle: u32,
    /// Number of triangles in this source run.
    pub triangle_count: u32,
    /// Center of the transformed source-model bounding sphere in merged-static local space.
    pub center: Vec3,
    /// Whole-source-model bounding-sphere radius times instance scale.
    ///
    /// Building doubling is intentionally not pre-applied.
    pub radius: f32,
    /// Source static classification exactly as authored for the source static.
    pub classification: StaticType,
}

/// Returns whether `components` exactly tile `triangle_count` triangles.
///
/// Tiling requires each component to be non-empty, to start where the previous one ended, to carry
/// a finite non-negative radius, and to name a provenance other than grass (grass never
/// participates in merged component ranges). An empty component list tiles vacuously.
///
/// This answers a yes/no question used to skip optional work on the in-memory model. The same rule
/// is re-checked on the packed wire records immediately before serialization, where violations are
/// hard errors rather than a reason to skip; see `statics::write::validate_component_records`.
pub(crate) fn components_tile_triangle_count(components: &[MergedComponent], triangle_count: u32) -> bool {
    if components.is_empty() {
        return true;
    }

    let mut expected_first = 0u32;
    for component in components {
        if component.triangle_count == 0 || component.first_triangle != expected_first {
            return false;
        }
        if !component.radius.is_finite() || component.radius < 0.0 || component.classification == StaticType::StaticGrass {
            return false;
        }
        expected_first = expected_first.saturating_add(component.triangle_count);
    }
    expected_first == triangle_count
}

impl Default for Subset {
    fn default() -> Self {
        Self {
            bounding_sphere: NiBound::default(),
            bounding_box: BoundingBox::default(),
            vertices: Vec::default(),
            triangles: Vec::default(),
            components: Vec::default(),
            uv_bounds: Vec::default(),
            has_alpha: false,
            has_uv_controller: false,
            emissive: 0.0,
            texture: SubsetTexture::default(),
        }
    }
}

/// Intermediate distant static with uncompressed geometry and per-subset metadata.
///
/// Converted into [`PackedDistantStatic`] for `static_meshes` serialization.
#[derive(Default, Clone)]
pub struct DistantStatic {
    pub bounding_sphere: NiBound,
    pub bounding_box: BoundingBox,
    pub static_type: StaticType,
    pub subsets: Vec<Subset>,
    /// Maximum reference scale seen for this mesh in usage scanning.
    pub max_scale: f32,
    /// Whether this static is a door (DOOR record), eligible for the door-size multiplier.
    pub is_door: bool,
    /// Whether generated horizon footprints may be emitted for this synthetic static.
    pub horizon_footprint_eligible: bool,
}

impl DistantStatic {
    pub fn update_bounds(&mut self) {
        update_bounds_with_context(self, &mut StaticMeshContext::default());
    }

    pub(super) fn update_bounds_from_subsets(&mut self) {
        self.bounding_sphere = self
            .subsets
            .iter()
            .map(|sub| sub.bounding_sphere)
            .reduce(|a, b| a.merged_with(b))
            .unwrap_or_default();

        self.bounding_box = self
            .subsets
            .iter()
            .map(|sub| sub.bounding_box)
            .reduce(|a, b| BoundingBox {
                min: a.min.min(b.min),
                max: a.max.max(b.max),
            })
            .unwrap_or_default();
    }

    /// Merges subsets with the same `has_alpha` into larger subsets.
    ///
    /// Subsets are kept separate when their texture paths differ, because UVs are later
    /// reinterpreted relative to the atlas page that owns that texture. Callers must
    /// recompute bounds after any subsequent geometry-changing passes settle.
    pub fn merge_subsets(&mut self) {
        self.discard_empty_subsets();

        if self.subsets.len() <= 1 {
            return;
        }

        let mut original_subsets = take(&mut self.subsets);

        self.subsets.reserve(2); // Avoids re-allocation for usual case.

        for has_alpha in [true, false] {
            let shared_alpha_subsets = original_subsets.extract_if(.., |s| s.has_alpha == has_alpha);

            for subset in shared_alpha_subsets {
                if let Some(merged) = self.subsets.last_mut()
                    && subset.can_merge_vertices(merged)
                {
                    let has_alpha_same = merged.has_alpha == subset.has_alpha;
                    let texture_same = merged.texture == subset.texture;
                    let has_uv_controller_same = merged.has_uv_controller == subset.has_uv_controller;
                    let emissive_same = merged.emissive == subset.emissive;

                    // This runs post-atlas, so `texture` equality compares atlas *page* ordinals,
                    // not source textures. Two subsets that came from different source textures
                    // therefore merge freely while carrying different UV bounds — which is the
                    // mechanism that produces multi-bound subsets in the first place. Refuse when
                    // the union would outgrow the shader's fixed palette array; refusal starts a
                    // new output subset through the existing path below.
                    let palette_fits = uv_bound_union_fits(&merged.uv_bounds, &subset.uv_bounds);

                    // Important: even though textures are later packed into a global atlas,
                    // `texture` here still identifies which atlas page/file UVs were computed
                    // for. Merging across different pages leads to wrong UV interpretation.
                    if !has_alpha_same || !texture_same || !has_uv_controller_same || !emissive_same || !palette_fits {
                        self.subsets.push(subset);
                        continue;
                    }

                    let mixed_provenance = (!merged.components.is_empty() && subset.components.is_empty())
                        || (merged.components.is_empty() && !merged.triangles.is_empty() && !subset.components.is_empty());
                    debug_assert!(
                        !mixed_provenance,
                        "cannot merge component-bearing and component-less subsets without losing tier provenance"
                    );
                    if mixed_provenance {
                        merged.components.clear();
                    }

                    let triangle_offset = merged.triangles.len() as u32;
                    merged.append_triangles(&subset.triangles);
                    merged.append_vertices(&subset.vertices);
                    union_uv_bounds(&mut merged.uv_bounds, &subset.uv_bounds);
                    if !mixed_provenance {
                        merged.append_components_shifted(&subset.components, triangle_offset);
                    }
                } else {
                    self.subsets.push(subset);
                }
            }
        }
    }

    pub fn discard_empty_subsets(&mut self) {
        self.subsets.retain(|s| !s.vertices.is_empty() && !s.triangles.is_empty());
    }
}

impl Subset {
    pub(super) fn update_bounds_with(
        &mut self,
        sphere_points: &mut Vec<[f32; 3]>,
        sphere_scratch: &mut BoundingSphereScratch,
    ) {
        if self.vertices.is_empty() {
            self.bounding_sphere = NiBound::default();
            self.bounding_box = BoundingBox::default();
            return;
        }

        let mut min = Vec3::MAX;
        let mut max = Vec3::MIN;
        let mut has_finite_position = false;
        let component_bound = self.component_bounding_sphere();

        sphere_points.clear();
        if component_bound.is_none() {
            sphere_points.reserve(self.vertices.len());
        }

        for vertex in self.vertices.iter() {
            let position = vertex.position;
            if component_bound.is_none() {
                sphere_points.push(position.to_array());
            }
            if !position.is_finite() {
                continue;
            }
            has_finite_position = true;
            min = min.min(position);
            max = max.max(position);
        }

        if !has_finite_position {
            self.bounding_sphere = NiBound::default();
            self.bounding_box = BoundingBox::default();
            return;
        }

        self.bounding_box.min = min;
        self.bounding_box.max = max;

        if let Some(bound) = component_bound {
            self.bounding_sphere = bound;
            return;
        }

        let bound = BoundingSphere::from_points_with_scratch(sphere_points, sphere_scratch);
        self.bounding_sphere.center = bound.center.map(|v| v as f32).into();
        self.bounding_sphere.radius = bound.radius as f32;
    }

    fn component_bounding_sphere(&self) -> Option<NiBound> {
        if self.components.is_empty() || !self.components_tile_triangles() {
            return None;
        }

        let mut bound: Option<NiBound> = None;
        for component in &self.components {
            if !component.center.is_finite() || !component.radius.is_finite() || component.radius < 0.0 {
                return None;
            }

            let component_bound = NiBound {
                center: component.center,
                radius: component.radius,
            };
            bound = Some(match bound {
                Some(bound) => bound.merged_with(component_bound),
                None => component_bound,
            });
        }
        bound
    }

    pub fn is_opaque(&self) -> bool {
        !self.has_alpha
    }

    pub fn has_alpha(&self) -> bool {
        self.has_alpha
    }

    /// Returns true if merging the subsets would not exceed the vertex limit.
    ///
    /// The limit is `u16::MAX` because triangle indices are stored as `u16`.
    pub fn can_merge_vertices(&self, other: &Subset) -> bool {
        (self.vertices.len() + other.vertices.len()) <= (u16::MAX as usize)
    }

    /// Returns true if the subsets can be merged based on vertex count and alpha compatibility.
    ///
    /// Two subsets are mergeable only when their alpha flags agree so that they end up on the
    /// same atlas and the joint index buffer stays within `u16` range.
    ///
    /// The palette-cap test sits outside the empty-vertex short circuit deliberately: a
    /// contribution that culling emptied still unions its bounds into the destination, so
    /// skipping the test for it could push the destination past the cap.
    pub fn can_merge_with(&self, other: &Subset) -> bool {
        self.can_merge_vertices(other)
            && uv_bound_union_fits(&self.uv_bounds, &other.uv_bounds)
            && (self.vertices.is_empty()
                || (self.is_opaque() == other.is_opaque()
                    && self.texture == other.texture
                    && self.has_uv_controller == other.has_uv_controller
                    && self.emissive == other.emissive))
    }

    pub fn append_triangles(&mut self, triangles: &[[u16; 3]]) {
        let offset = self.vertices.len() as u16;
        self.triangles.reserve(triangles.len());
        self.triangles
            .extend(triangles.iter().map(|&[i0, i1, i2]| [i0 + offset, i1 + offset, i2 + offset]));
    }

    pub fn append_vertices(&mut self, vertices: &[Vertex]) -> &mut [Vertex] {
        let start = self.vertices.len();
        self.vertices.extend_from_slice(vertices);
        &mut self.vertices[start..]
    }

    /// Adds a component record, coalescing with the previous record when possible.
    pub(crate) fn push_component(
        &mut self,
        first_triangle: u32,
        triangle_count: u32,
        center: Vec3,
        radius: f32,
        classification: StaticType,
    ) {
        if triangle_count == 0 {
            return;
        }

        debug_assert!(center.is_finite());
        debug_assert!(radius.is_finite() && radius >= 0.0);
        debug_assert_ne!(classification, StaticType::StaticGrass);
        debug_assert_eq!(
            self.components
                .last()
                .map_or(0, |component| component.first_triangle + component.triangle_count),
            first_triangle
        );

        if let Some(previous) = self.components.last_mut()
            && previous.first_triangle + previous.triangle_count == first_triangle
            && previous.center == center
            && previous.radius.to_bits() == radius.to_bits()
            && previous.classification == classification
        {
            previous.triangle_count += triangle_count;
            return;
        }

        self.components.push(MergedComponent {
            first_triangle,
            triangle_count,
            center,
            radius,
            classification,
        });
    }

    fn append_components_shifted(&mut self, components: &[MergedComponent], triangle_offset: u32) {
        for component in components {
            self.push_component(
                component.first_triangle + triangle_offset,
                component.triangle_count,
                component.center,
                component.radius,
                component.classification,
            );
        }
    }

    pub(crate) fn components_tile_triangles(&self) -> bool {
        // A buffer longer than `u32::MAX` can never be tiled by u32 component ranges.
        u32::try_from(self.triangles.len()).is_ok_and(|count| components_tile_triangle_count(&self.components, count))
    }

    pub fn merge_transformed(&mut self, subset: &Subset, transform: Affine3A, center: Vec3, opaque: bool) {
        self.append_triangles(&subset.triangles);

        for vertex in self.append_vertices(&subset.vertices) {
            place_vertex(vertex, transform, center);
        }

        self.adopt_source_identity(subset, opaque);
    }

    /// Merges another subset while dropping triangles entirely below the terrain surface.
    ///
    /// A triangle survives when *any* corner is above the threshold, so surviving geometry always
    /// straddles the ground line and holes can only open strictly inside the buried region.
    ///
    /// Filtering here rather than in a later compaction pass keeps the caller's component ranges
    /// correctly tiled by construction, because the caller derives each range from this subset's
    /// triangle-buffer growth.
    pub fn merge_transformed_culled(
        &mut self,
        subset: &Subset,
        transform: Affine3A,
        center: Vec3,
        opaque: bool,
        culler: &mut SubterrainCuller<'_>,
    ) {
        culler.classify(subset, transform);

        // The caller has already checked that the untrimmed source fits, so the trimmed vertices
        // cannot push this subset past the `u16` index limit either.
        let base = self.vertices.len() as u16;
        let mut kept_vertices: u32 = 0;
        self.triangles.reserve(subset.triangles.len());
        self.vertices.reserve(subset.vertices.len());

        for triangle in &subset.triangles {
            if triangle.iter().all(|&index| culler.buried[index as usize]) {
                culler.tally.triangles += 1;
                continue;
            }

            let mut merged_triangle = [0u16; 3];
            for (slot, &index) in merged_triangle.iter_mut().zip(triangle) {
                let mapped = &mut culler.remap[index as usize];
                if *mapped == SubterrainCuller::UNMAPPED {
                    // Copying on first reference numbers survivors in triangle order, which is the
                    // vertex-fetch-optimized layout anyway, so the trim costs no locality.
                    *mapped = kept_vertices;
                    kept_vertices += 1;
                    let mut vertex = subset.vertices[index as usize];
                    place_vertex(&mut vertex, transform, center);
                    self.vertices.push(vertex);
                }
                *slot = base + *mapped as u16;
            }
            self.triangles.push(merged_triangle);
        }

        culler.tally.vertices += subset.vertices.len() - kept_vertices as usize;

        self.adopt_source_identity(subset, opaque);
    }

    /// Copies atlas-identity fields even when culling empties the subset, so merging stays
    /// partitioned like the source.
    ///
    /// The UV-bound set is a full union, never a single-bound insert: an incoming contribution
    /// has already been through `DistantStatic::merge_subsets` post-atlas, so it may itself carry
    /// several bounds. `Subset::can_merge_with` has already established that the union fits.
    fn adopt_source_identity(&mut self, subset: &Subset, opaque: bool) {
        self.has_alpha = !opaque; // Ensure this as default() does not
        self.has_uv_controller = subset.has_uv_controller;
        self.emissive = subset.emissive;
        self.texture = subset.texture;
        union_uv_bounds(&mut self.uv_bounds, &subset.uv_bounds);
    }
}

#[inline]
fn place_vertex(vertex: &mut Vertex, transform: Affine3A, center: Vec3) {
    vertex.position = transform.transform_point3(vertex.position) - center;
    // A degenerate transform (zero scale on an axis, collapsed basis) maps the normal to
    // the zero vector, and a bare `normalize` would turn that into NaN. That NaN survives:
    // welding treats it as non-finite and force-tiebreaks the vertex out of every merge
    // group, so degenerate input silently costs geometry instead of being absorbed.
    vertex.normal = transform.transform_vector3(vertex.normal).normalize_or_zero();
}

/// Heightmap inputs for removing merged geometry below `terrain_height - margin`.
///
/// The margin is the terrain simplifier's absolute error budget. This applies only to merged
/// records because ordinary records are instanced at different placements.
#[derive(Clone, Copy)]
pub struct SubterrainCull<'a> {
    terrain_cells: &'a TerrainCells<'a>,
    /// World units of slack below the sampled height before a vertex counts as buried.
    margin: f32,
}

impl<'a> SubterrainCull<'a> {
    /// Creates a cull using the terrain simplifier's absolute error as `margin`.
    pub fn new(terrain_cells: &'a TerrainCells<'a>, margin: f32) -> Self {
        Self { terrain_cells, margin }
    }

    /// Returns whether a world-space position sits below the safe ground threshold.
    ///
    /// Positions no LAND cell covers count as not buried, so unmapped regions keep their geometry.
    fn is_buried(&self, world: Vec3) -> bool {
        terrain_height_at(self.terrain_cells, Vec2::new(world.x, world.y))
            .is_some_and(|height| height - world.z > self.margin)
    }
}

/// Geometry a [`SubterrainCull`] removed while merging.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SubterrainCullTally {
    /// Triangles removed because all three corners were buried.
    pub triangles: usize,
    /// Vertices removed because only culled triangles referenced them.
    pub vertices: usize,
}

/// Per-worker cull state: the heightmap inputs, reusable scratch, and the running tally.
///
/// Terrain height is sampled once per source vertex and reused by every triangle touching it.
pub struct SubterrainCuller<'a> {
    /// Heightmap inputs and threshold.
    cull: SubterrainCull<'a>,
    /// Per source vertex: whether it sits below the safe ground threshold.
    buried: Vec<bool>,
    /// Per source vertex: its index in the destination subset, or [`SubterrainCuller::UNMAPPED`].
    remap: Vec<u32>,
    /// Geometry removed so far.
    tally: SubterrainCullTally,
}

impl<'a> SubterrainCuller<'a> {
    /// Marks a source vertex that no surviving triangle has referenced yet.
    ///
    /// A `u32` sentinel rather than `u16::MAX` so it cannot collide with a real index in a source
    /// subset that uses the full `u16` range.
    const UNMAPPED: u32 = u32::MAX;

    pub fn new(cull: SubterrainCull<'a>) -> Self {
        Self {
            cull,
            buried: Vec::new(),
            remap: Vec::new(),
            tally: SubterrainCullTally::default(),
        }
    }

    pub fn tally(&self) -> SubterrainCullTally {
        self.tally
    }

    fn classify(&mut self, subset: &Subset, transform: Affine3A) {
        let cull = self.cull;
        self.buried.clear();
        self.buried.extend(
            subset
                .vertices
                .iter()
                .map(|vertex| cull.is_buried(transform.transform_point3(vertex.position))),
        );
        self.remap.clear();
        self.remap.resize(subset.vertices.len(), Self::UNMAPPED);
    }
}

#[cfg(test)]
use crate::Vfs;
#[cfg(test)]
use crate::extract::{inferred_static_type, resolve_static_type};
#[cfg(test)]
use crate::overrides::StaticOverrides;

#[cfg(test)]
mod tests;
