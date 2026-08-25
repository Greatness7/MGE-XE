//! Terrain mesh generation for the terrain package.

use std::mem::size_of;
use std::time::Instant;

use anyhow::{Result, bail};
use glam::{Vec3, Vec4};
use hashbrown::{HashMap, HashSet};
use rayon::prelude::*;
use tracing::{debug, info_span};

use crate::UsageInfo;
use crate::layout::TerrainAtlasRegion;
use crate::mge_xe::distant_statics::{BoundingBox, BoundingSphere};
use crate::mge_xe::distant_terrain::{TerrainMesh, TerrainVertex, pack_d3dcolor_vclr, pack_ubyte4n_bias_normal};
use crate::mge_xe::world::LAND_CELL_SIZE;
use crate::texture::TerrainCells;

/// World-space distance between adjacent 65x65 LAND grid vertices.
const LAND_GRID_STEP: f32 = 128.0;
/// LAND grid intervals spanning one exterior cell along each axis: 65 vertices,
/// so 64 intervals. Equal to `LAND_CELL_SIZE / LAND_GRID_STEP`, and the number of
/// distinct local vertex coordinates (0..64) before a step crosses into the next cell.
const LAND_STEPS_PER_CELL: usize = 64;
/// Fixed terrain mesh chunk width in LAND cells.
///
/// This is a compile-time layout invariant, not a per-run setting: work keys, the
/// shared dense index grid, and the smoothed-normal neighborhood are all derived
/// from it. Changing it changes mesh identity, so it is written into
/// [`TerrainGateInputs::mesh_chunk_cells_per_side`](crate::package::TerrainGateInputs)
/// and a source change invalidates cached terrain.
pub(crate) const MESH_CHUNK_CELLS_PER_SIDE: usize = 4;
/// Deep-water Z sentinel used when a patch corner has no landscape data.
const DEEP_WATER_Z: f32 = -2048.0;
const DEEP_WATER_EPSILON: f32 = 0.001;
const SIMPLIFIER_NORMAL_SMOOTHING_RADIUS: i32 = 1;
const DEFAULT_FALLBACK_NORMAL: Vec3 = Vec3::new(0.0, 0.0, 1.0);
const DEFAULT_FALLBACK_COLOR: Vec4 = Vec4::new(1.0, 1.0, 1.0, 1.0);

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct DenseVertex {
    position: Vec3,
    raw_normal: Vec3,
    smoothed_normal: Vec3,
    color: [f32; 4],
}

const _: () = {
    assert!(std::mem::offset_of!(DenseVertex, smoothed_normal) == 24);
    assert!(size_of::<DenseVertex>() == 52);
};

#[derive(Clone, Copy, Debug, PartialEq)]
struct MeshChunkBounds {
    left: f32,
    right: f32,
    bottom: f32,
    top: f32,
}

mod config;
pub use config::*;

mod work;
use distantland_foundation::units::TerrainMeshWorkKey;
use work::WorkCellRect;
pub use work::{TerrainMeshWorkItem, enumerate_terrain_mesh_work};

struct DenseMeshSimplification {
    indices: Vec<u32>,
    achieved_error: f32,
}

/// Per-thread reusable buffers for terrain-mesh generation and simplification.
#[derive(Default)]
struct TerrainMeshContext {
    dense_verts: Vec<DenseVertex>,
    remap: Vec<u32>,
}

fn mesh_chunk_size_world(mesh_chunk_cells_per_side: usize) -> f32 {
    mesh_chunk_cells_per_side as f32 * LAND_CELL_SIZE
}

fn dense_steps_per_edge(mesh_chunk_cells_per_side: usize) -> usize {
    mesh_chunk_cells_per_side * LAND_STEPS_PER_CELL
}

fn dense_vertices_per_edge(mesh_chunk_cells_per_side: usize) -> usize {
    dense_steps_per_edge(mesh_chunk_cells_per_side) + 1
}

fn dense_triangle_count() -> usize {
    let steps_per_edge = dense_steps_per_edge(MESH_CHUNK_CELLS_PER_SIDE);
    2 * steps_per_edge * steps_per_edge
}

fn mesh_chunk_bounds(min_x: f32, min_y: f32, x: usize, y: usize, mesh_chunk_cells_per_side: usize) -> MeshChunkBounds {
    let mesh_chunk_size_world = mesh_chunk_size_world(mesh_chunk_cells_per_side);
    let left = min_x + x as f32 * mesh_chunk_size_world;
    let right = left + mesh_chunk_size_world;
    let bottom = min_y + y as f32 * mesh_chunk_size_world;
    let top = bottom + mesh_chunk_size_world;
    MeshChunkBounds {
        left,
        right,
        bottom,
        top,
    }
}

fn dense_vertex_index(ix: usize, iy: usize, vertices_per_edge: usize) -> u32 {
    (iy * vertices_per_edge + ix) as u32
}

fn append_dense_quad_indices(indices: &mut Vec<u32>, ix: usize, iy: usize, vertices_per_edge: usize) {
    let bl = dense_vertex_index(ix, iy, vertices_per_edge);
    let br = dense_vertex_index(ix + 1, iy, vertices_per_edge);
    let tl = dense_vertex_index(ix, iy + 1, vertices_per_edge);
    let tr = dense_vertex_index(ix + 1, iy + 1, vertices_per_edge);

    // Morrowind splits each quad of its 65x65 LAND vertex grid along a diagonal that
    // alternates in a checkerboard, which is the parity of the linear vertex index
    // `iy * 65 + ix` within a cell. 65 is odd and the cell stride is even, so that parity
    // reduces to the parity of `ix + iy` over the whole chunk grid.
    let backward_winding = (ix + iy) & 1 != 0;

    if !backward_winding {
        indices.extend_from_slice(&[br, tr, bl]);
        indices.extend_from_slice(&[tr, tl, bl]);
    } else {
        indices.extend_from_slice(&[tl, br, tr]);
        indices.extend_from_slice(&[bl, br, tl]);
    }
}

/// Builds the dense index grid for one mesh chunk. Grid extent depends only on
/// [`MESH_CHUNK_CELLS_PER_SIDE`] and winding parity is a pure function of the local
/// `(ix, iy)`, so this buffer is identical for every chunk in a run and can be shared
/// immutably.
fn build_dense_index_grid() -> Vec<u32> {
    let steps_per_edge = dense_steps_per_edge(MESH_CHUNK_CELLS_PER_SIDE);
    let vertices_per_edge = dense_vertices_per_edge(MESH_CHUNK_CELLS_PER_SIDE);
    let mut indices = Vec::with_capacity(dense_triangle_count() * 3);
    for iy in 0..steps_per_edge {
        for ix in 0..steps_per_edge {
            append_dense_quad_indices(&mut indices, ix, iy, vertices_per_edge);
        }
    }
    indices
}

/// Derives the deduplicated set of cell keys the given work items can sample during
/// dense construction.
///
/// Both [`build_smoothed_simplifier_normals`] and [`build_default_cells`] need exactly this
/// per-item neighborhood, since [`default_chunk_uniform_height`] queries the identical
/// `start..=start+span` range this function derives.
///
/// A chunk beginning at absolute cell `(start_x, start_y)` with side
/// [`MESH_CHUNK_CELLS_PER_SIDE`] (`S`) builds a full `S`-cell dense grid whose world
/// X spans `[start_x * LAND_CELL_SIZE, (start_x + S) * LAND_CELL_SIZE]`; those world
/// coordinates floor to smoothed-map keys `start_x..=start_x + S` (and likewise Y),
/// so the inclusive `+X` / `+Y` boundary key is sampled and must be included. The
/// range is taken from the nominal square via the stable work key, never from the
/// region-clipped `cell_rect`, so clipped edge work still lists every key its
/// unclipped dense grid can read. Negative absolute coordinates are handled
/// directly by signed arithmetic.
fn smoothed_normal_target_keys(work: &[TerrainMeshWorkItem]) -> HashSet<(i32, i32)> {
    let span = i32::try_from(MESH_CHUNK_CELLS_PER_SIDE).expect("mesh chunk cells per side must fit in i32");
    let mut keys = HashSet::new();
    for item in work {
        let start_x = item.key.start_cell_x;
        let start_y = item.key.start_cell_y;
        for cell_y in start_y..=start_y + span {
            for cell_x in start_x..=start_x + span {
                keys.insert((cell_x, cell_y));
            }
        }
    }
    keys
}

/// Filters `target_keys` down to the cells present in `terrain_cells` that hold only
/// default/trivial data (see [`TerrainCell::is_default`]).
///
/// Scoping to `target_keys` bounds this to the rebuilt neighborhood for the same
/// reason [`build_smoothed_simplifier_normals`] does: `default_chunk_uniform_height`
/// only ever queries the identical `start..=start+span` range per work item that
/// [`smoothed_normal_target_keys`] already covers.
fn build_default_cells<'a>(
    terrain_cells: &crate::texture::TerrainCells<'a>,
    target_keys: &HashSet<(i32, i32)>,
) -> HashSet<(i32, i32)> {
    target_keys
        .par_iter()
        .filter_map(|&grid| terrain_cells.get(&grid)?.is_default().then_some(grid))
        .collect()
}

/// Precomputes the smoothed simplifier-normal field for each `target_keys` entry
/// that exists in `terrain_cells`.
///
/// `target_keys` names exactly the cells the rebuilt work items can sample (see
/// [`smoothed_normal_target_keys`]), so computing only these bounds the per-partial-
/// rebuild cost and peak memory to the touched neighborhood rather than the whole
/// world. Each field is still derived from the target cell's full 3x3 raw-normal
/// neighborhood with unchanged missing-neighbor clamping, so a computed field is
/// byte-identical to the previous whole-world result. Target keys absent from
/// `terrain_cells` are skipped, preserving the documented [`DEFAULT_FALLBACK_NORMAL`]
/// fallback applied at sample time.
fn build_smoothed_simplifier_normals<'a>(
    terrain_cells: &crate::texture::TerrainCells<'a>,
    target_keys: &HashSet<(i32, i32)>,
) -> HashMap<(i32, i32), Vec<Vec3>> {
    target_keys
        .par_iter()
        .filter_map(|&grid| {
            let cell = terrain_cells.get(&grid)?;
            let origin_normals = &cell.normals;
            let neighbor_normals: [[Option<&crate::texture::TerrainNormals>; 3]; 3] = std::array::from_fn(|neighbor_y| {
                std::array::from_fn(|neighbor_x| {
                    let neighbor_grid = (grid.0 + neighbor_x as i32 - 1, grid.1 + neighbor_y as i32 - 1);
                    if neighbor_grid == grid {
                        Some(origin_normals)
                    } else {
                        terrain_cells.get(&neighbor_grid).map(|cell| &cell.normals)
                    }
                })
            });
            let sample_normal = |vertex_x: i32, vertex_y: i32| {
                let (neighbor_x, local_x) = if vertex_x < 0 {
                    (0_usize, (vertex_x + 64) as usize)
                } else if vertex_x > 64 {
                    (2_usize, (vertex_x - 64) as usize)
                } else {
                    (1_usize, vertex_x as usize)
                };
                let (neighbor_y, local_y) = if vertex_y < 0 {
                    (0_usize, (vertex_y + 64) as usize)
                } else if vertex_y > 64 {
                    (2_usize, (vertex_y - 64) as usize)
                } else {
                    (1_usize, vertex_y as usize)
                };

                let sample = if let Some(normals) = neighbor_normals[neighbor_y][neighbor_x] {
                    normals.get(local_y * 65 + local_x)
                } else {
                    let clamped_x = vertex_x.clamp(0, 64) as usize;
                    let clamped_y = vertex_y.clamp(0, 64) as usize;
                    origin_normals.get(clamped_y * 65 + clamped_x)
                };
                sample.normalize_or(DEFAULT_FALLBACK_NORMAL)
            };

            let mut field = Vec::with_capacity(cell.normals.iter().len());
            for y in 0..65 {
                for x in 0..65 {
                    let mut accumulated = Vec3::ZERO;
                    let mut sample_count = 0.0;
                    for offset_y in -SIMPLIFIER_NORMAL_SMOOTHING_RADIUS..=SIMPLIFIER_NORMAL_SMOOTHING_RADIUS {
                        for offset_x in -SIMPLIFIER_NORMAL_SMOOTHING_RADIUS..=SIMPLIFIER_NORMAL_SMOOTHING_RADIUS {
                            accumulated += sample_normal(x + offset_x, y + offset_y);
                            sample_count += 1.0;
                        }
                    }
                    field.push((accumulated / sample_count).normalize_or(DEFAULT_FALLBACK_NORMAL));
                }
            }
            Some((grid, field))
        })
        .collect()
}

/// Populates `vertices` with the dense vertex grid for one mesh chunk, reusing
/// the caller's allocation. The index grid is loop-invariant and built separately
/// by [`build_dense_index_grid`].
///
/// Every dense vertex lands on an exact 65x65 LAND grid step, so its owning cell and
/// in-cell grid index are derived directly with integer division/modulo by
/// [`LAND_STEPS_PER_CELL`] rather than by re-flooring reconstructed world coordinates.
/// Integer arithmetic gives the same **floor** ownership the previous world-space
/// samplers used: a step landing on a cell boundary (a multiple of
/// [`LAND_STEPS_PER_CELL`]) belongs to the north/east cell at local `0`, never local
/// `64` of the south/west cell, and it stays correct for negative region minima and
/// for the final partial chunk whose grid extends past `region.max`. Because the
/// coordinate is exact, the height, decoded normal, and color are read straight from
/// the cell's grids (`colors` are still clamped with alpha forced to `1`), and the
/// raw and smoothed normals are already unit-length where they are built, so they are
/// not re-normalized.
fn build_dense_mesh_chunk_vertices_into<'a>(
    terrain_cells: &TerrainCells<'a>,
    smoothed_normals: &HashMap<(i32, i32), Vec<Vec3>>,
    region_min_cell_x: i32,
    region_min_cell_y: i32,
    patch_x: usize,
    patch_y: usize,
    mesh_chunk_cells_per_side: usize,
    vertices: &mut Vec<DenseVertex>,
) {
    let region_min_x = region_min_cell_x as f32 * LAND_CELL_SIZE;
    let region_min_y = region_min_cell_y as f32 * LAND_CELL_SIZE;
    let bounds = mesh_chunk_bounds(region_min_x, region_min_y, patch_x, patch_y, mesh_chunk_cells_per_side);
    let steps_per_edge = dense_steps_per_edge(mesh_chunk_cells_per_side);
    let vertices_per_edge = dense_vertices_per_edge(mesh_chunk_cells_per_side);
    let vertex_count = vertices_per_edge * vertices_per_edge;

    vertices.clear();
    vertices.reserve(vertex_count);

    for iy in 0..=steps_per_edge {
        let global_iy = patch_y * steps_per_edge + iy;
        let cell_y = region_min_cell_y + (global_iy / LAND_STEPS_PER_CELL) as i32;
        let local_y = global_iy % LAND_STEPS_PER_CELL;
        let world_y = region_min_y + global_iy as f32 * LAND_GRID_STEP;
        for ix in 0..=steps_per_edge {
            let global_ix = patch_x * steps_per_edge + ix;
            let cell_x = region_min_cell_x + (global_ix / LAND_STEPS_PER_CELL) as i32;
            let local_x = global_ix % LAND_STEPS_PER_CELL;
            let world_x = region_min_x + global_ix as f32 * LAND_GRID_STEP;
            debug_assert!(world_x >= bounds.left && world_x <= bounds.right);
            debug_assert!(world_y >= bounds.bottom && world_y <= bounds.top);

            let index = local_y * 65 + local_x;
            let (height, raw_normal, color) = match terrain_cells.get(&(cell_x, cell_y)) {
                Some(cell) => {
                    let raw_color = cell.colors.get(index);
                    let color = [
                        raw_color.x.clamp(0.0, 1.0),
                        raw_color.y.clamp(0.0, 1.0),
                        raw_color.z.clamp(0.0, 1.0),
                        1.0,
                    ];
                    (cell.heights[local_y][local_x], cell.normals.get(index), color)
                }
                None => (DEEP_WATER_Z, DEFAULT_FALLBACK_NORMAL, DEFAULT_FALLBACK_COLOR.to_array()),
            };
            let smoothed_normal = smoothed_normals
                .get(&(cell_x, cell_y))
                .map_or(DEFAULT_FALLBACK_NORMAL, |field| field[index]);

            vertices.push(DenseVertex {
                position: Vec3::new(world_x, world_y, height),
                raw_normal,
                smoothed_normal,
                color,
            });
        }
    }
}

/// Attribute weights for the simplifier's error metric, laid out to match the
/// [`DenseVertex`] field order starting at `smoothed_normal`.
///
/// Color alpha is deliberately excluded rather than carried at weight `0.0`. It is a
/// constant `1.0` for every dense vertex (see `build_dense_mesh_chunk_vertices_into`) and
/// is re-forced to opaque at pack time, so it can never influence the error metric;
/// meshopt also filters zero-weight attributes before any quadric work, so carrying it
/// would change nothing.
fn dense_mesh_attribute_weights(weights: MeshSimplifierWeights) -> [f32; 6] {
    [
        weights.smoothed_normal,
        weights.smoothed_normal,
        weights.smoothed_normal,
        weights.color,
        weights.color,
        weights.color,
    ]
}

fn simplify_dense_mesh(
    dense_verts: &[DenseVertex],
    indices: &[u32],
    vertex_lock: &[bool],
    config: MeshSimplifierConfig,
) -> DenseMeshSimplification {
    debug_assert_eq!(vertex_lock.len(), dense_verts.len());

    let vertex_bytes: &[u8] = bytemuck::must_cast_slice(dense_verts);
    let vertex_adapter = meshopt::VertexDataAdapter::new(
        vertex_bytes,
        size_of::<DenseVertex>(),
        std::mem::offset_of!(DenseVertex, position),
    )
    .expect("DenseVertex position layout must be valid");

    let attr_offset = std::mem::offset_of!(DenseVertex, smoothed_normal);
    let attr_bytes = &vertex_bytes[attr_offset..];
    let vertex_attributes: &[f32] = bytemuck::cast_slice(attr_bytes);
    let attribute_weights = dense_mesh_attribute_weights(config.weights);

    let mut achieved_error = 0.0_f32;
    let options = meshopt_simplify_options();

    // The meshopt Rust binding requires a full lock slice even when no vertices
    // are locked. It does not expose a null-pointer "no locks" path.
    let simplified_indices = meshopt::simplify_with_attributes_and_locks(
        indices,
        &vertex_adapter,
        vertex_attributes,
        &attribute_weights,
        size_of::<DenseVertex>(),
        vertex_lock,
        0,
        config.target_error,
        options,
        Some(&mut achieved_error),
    );

    DenseMeshSimplification {
        indices: simplified_indices,
        achieved_error,
    }
}

/// One work item's outcome, retained even when the item emits no record.
pub struct TerrainMeshWorkResult {
    /// Identity of the item that produced this result.
    pub key: TerrainMeshWorkKey,
    /// The emitted record, or `None` for work that contributes no terrain record.
    pub mesh: Option<TerrainMesh>,
}

/// A complete terrain mesh set plus the logical key of every record it emitted, in file order.
#[derive(Debug)]
pub struct TerrainMeshSet {
    /// Records in the exact order they are written to `terrain.bin`.
    pub meshes: Vec<TerrainMesh>,
    /// The emitting work key of each record, positionally aligned with `meshes`.
    pub emitted_keys: Vec<TerrainMeshWorkKey>,
}

/// Shared per-run precomputation reused by every mesh work item.
pub struct TerrainMeshBuilder<'a, 'b> {
    usage_info: &'a UsageInfo<'b>,
    simplifier_config: MeshSimplifierConfig,
    smoothed_normals: HashMap<(i32, i32), Vec<Vec3>>,
    default_cells: HashSet<(i32, i32)>,
    dense_indices: Vec<u32>,
    vertex_lock: Vec<bool>,
    dense_vertex_count: usize,
}

impl<'a, 'b> TerrainMeshBuilder<'a, 'b> {
    /// Computes the shared inputs the supplied `work` items need.
    ///
    /// Smoothed simplifier normals and the default-cell flag map are both precomputed
    /// only for the cells `work` can sample (see `smoothed_normal_target_keys`), so a
    /// partial rebuild pays for its touched neighborhood rather than the whole world.
    /// `default_chunk_uniform_height` scans the identical `start..=start+span` range
    /// per work item, so `target_keys` covers every cell it queries. Callers must pass
    /// exactly the work items they will hand to [`build_all`](Self::build_all); a
    /// narrower slice would omit fields those items sample.
    pub fn new(
        usage_info: &'a UsageInfo<'b>,
        simplifier_config: MeshSimplifierConfig,
        work: &[TerrainMeshWorkItem],
    ) -> Self {
        let target_keys = smoothed_normal_target_keys(work);
        let smoothed_normals = if simplifier_config.weights.smoothed_normal == 0.0 {
            HashMap::new()
        } else {
            let _s = info_span!("terrain.build_smoothed_simplifier_normals").entered();
            build_smoothed_simplifier_normals(&usage_info.terrain_cells, &target_keys)
        };
        let default_cells = build_default_cells(&usage_info.terrain_cells, &target_keys);
        let vertices_per_edge = dense_vertices_per_edge(MESH_CHUNK_CELLS_PER_SIDE);
        let dense_vertex_count = vertices_per_edge * vertices_per_edge;
        Self {
            usage_info,
            simplifier_config,
            smoothed_normals,
            default_cells,
            dense_indices: build_dense_index_grid(),
            vertex_lock: vec![false; dense_vertex_count],
            dense_vertex_count,
        }
    }

    /// Builds every supplied work item in parallel, retaining absent results.
    pub fn build_all(&self, work: &[TerrainMeshWorkItem]) -> Vec<TerrainMeshWorkResult> {
        let _s = info_span!("terrain.build_and_simplify_mesh_chunks").entered();
        work.par_iter()
            .map_init(TerrainMeshContext::default, |context, item| self.build(item, context))
            .collect()
    }

    fn build(&self, item: &TerrainMeshWorkItem, context: &mut TerrainMeshContext) -> TerrainMeshWorkResult {
        TerrainMeshWorkResult {
            key: item.key,
            mesh: self.build_mesh(item, context),
        }
    }

    fn build_mesh(&self, item: &TerrainMeshWorkItem, context: &mut TerrainMeshContext) -> Option<TerrainMesh> {
        let TerrainMeshWorkItem {
            region,
            patch_x,
            patch_y,
            ..
        } = *item;
        if !mesh_chunk_contains_populated_cells(&self.usage_info.terrain_cells, item.cell_rect) {
            return None;
        }

        let min_x = region.min_x as f32 * LAND_CELL_SIZE;
        let min_y = region.min_y as f32 * LAND_CELL_SIZE;

        if let Some(uniform_height) =
            default_chunk_uniform_height(&self.usage_info.terrain_cells, &self.default_cells, &region, patch_x, patch_y)
        {
            return build_default_terrain_mesh(uniform_height, min_x, min_y, patch_x, patch_y);
        }

        let dense_started = Instant::now();
        build_dense_mesh_chunk_vertices_into(
            &self.usage_info.terrain_cells,
            &self.smoothed_normals,
            region.min_x,
            region.min_y,
            patch_x,
            patch_y,
            MESH_CHUNK_CELLS_PER_SIDE,
            &mut context.dense_verts,
        );
        debug_assert_eq!(context.dense_verts.len(), self.dense_vertex_count);
        let dense_build_ms = dense_started.elapsed().as_secs_f64() * 1000.0;

        let simplify_started = Instant::now();
        let simplified = simplify_dense_mesh(
            &context.dense_verts,
            &self.dense_indices,
            &self.vertex_lock,
            self.simplifier_config,
        );
        let simplify_ms = simplify_started.elapsed().as_secs_f64() * 1000.0;

        let dense_triangle_count = self.dense_indices.len() / 3;
        debug!(
            region_min_x = region.min_x,
            region_min_y = region.min_y,
            patch_x,
            patch_y,
            achieved_error = simplified.achieved_error,
            dense_triangle_count,
            simplified_triangle_count = simplified.indices.len() / 3,
            dense_build_ms,
            simplify_ms,
            "Simplified terrain mesh chunk"
        );

        build_simplified_terrain_mesh(&context.dense_verts, &simplified.indices, &mut context.remap)
            .expect("meshopt must return valid dense-grid terrain indices")
    }
}

/// Orders work results by key and lowers them into the terrain record set.
///
/// Sorting here rather than relying on the parallel iterator's collection order is what makes
/// the emitted record order a property of the work keys alone.
///
/// # Errors
///
/// Returns an error when two results share a work key.
pub fn assemble_terrain_mesh_set(mut results: Vec<TerrainMeshWorkResult>) -> Result<TerrainMeshSet> {
    results.sort_unstable_by_key(|result| result.key);
    if let Some(pair) = results.windows(2).find(|pair| pair[0].key == pair[1].key) {
        bail!("duplicate terrain mesh work key {:?} in assembled results", pair[0].key);
    }
    let (emitted_keys, meshes) = results
        .into_iter()
        .filter_map(|result| result.mesh.map(|mesh| (result.key, mesh)))
        .unzip();
    Ok(TerrainMeshSet { meshes, emitted_keys })
}

fn mesh_chunk_contains_populated_cells(terrain_cells: &TerrainCells<'_>, rect: WorkCellRect) -> bool {
    for cell_y in rect.min_y..=rect.max_y {
        for cell_x in rect.min_x..=rect.max_x {
            if terrain_cells.contains_key(&(cell_x, cell_y)) {
                return true;
            }
        }
    }
    false
}

/// Returns a shared flat height for chunks whose dense-grid samples would read only
/// default/trivial cells at one uniform height. Missing cells contribute the same
/// `DEEP_WATER_Z` fallback used by the dense builder.
fn default_chunk_uniform_height(
    terrain_cells: &TerrainCells<'_>,
    default_cells: &HashSet<(i32, i32)>,
    region: &TerrainAtlasRegion,
    patch_x: usize,
    patch_y: usize,
) -> Option<f32> {
    let start_x = region.min_x + (patch_x * MESH_CHUNK_CELLS_PER_SIDE) as i32;
    let start_y = region.min_y + (patch_y * MESH_CHUNK_CELLS_PER_SIDE) as i32;
    let span = MESH_CHUNK_CELLS_PER_SIDE as i32;

    let mut consensus = None;
    for cell_y in start_y..=start_y + span {
        for cell_x in start_x..=start_x + span {
            let height = match terrain_cells.get(&(cell_x, cell_y)) {
                Some(cell) if default_cells.contains(&(cell_x, cell_y)) => cell.heights[0][0],
                Some(_) => return None,
                None => DEEP_WATER_Z,
            };
            match consensus {
                Some(existing) if existing != height => return None,
                Some(_) => {}
                None => consensus = Some(height),
            }
        }
    }

    consensus
}

/// Builds a flat quad for a chunk classified as uniformly default/trivial.
fn build_default_terrain_mesh(
    uniform_height: f32,
    region_min_x: f32,
    region_min_y: f32,
    patch_x: usize,
    patch_y: usize,
) -> Option<TerrainMesh> {
    if is_deep_water_z(uniform_height) {
        return None;
    }

    let bounds = mesh_chunk_bounds(region_min_x, region_min_y, patch_x, patch_y, MESH_CHUNK_CELLS_PER_SIDE);
    let vertices = [
        TerrainVertex {
            position: Vec3::new(bounds.left, bounds.bottom, uniform_height),
            normal: pack_ubyte4n_bias_normal(DEFAULT_FALLBACK_NORMAL),
            color: pack_vertex_color(DEFAULT_FALLBACK_COLOR),
        },
        TerrainVertex {
            position: Vec3::new(bounds.right, bounds.bottom, uniform_height),
            normal: pack_ubyte4n_bias_normal(DEFAULT_FALLBACK_NORMAL),
            color: pack_vertex_color(DEFAULT_FALLBACK_COLOR),
        },
        TerrainVertex {
            position: Vec3::new(bounds.left, bounds.top, uniform_height),
            normal: pack_ubyte4n_bias_normal(DEFAULT_FALLBACK_NORMAL),
            color: pack_vertex_color(DEFAULT_FALLBACK_COLOR),
        },
        TerrainVertex {
            position: Vec3::new(bounds.right, bounds.top, uniform_height),
            normal: pack_ubyte4n_bias_normal(DEFAULT_FALLBACK_NORMAL),
            color: pack_vertex_color(DEFAULT_FALLBACK_COLOR),
        },
    ];
    let bmin = Vec3::new(bounds.left, bounds.bottom, uniform_height);
    let bmax = Vec3::new(bounds.right, bounds.top, uniform_height);
    let center = (bmin + bmax) * 0.5;

    Some(TerrainMesh {
        bounding_sphere: terrain_bounding_sphere(&vertices, center),
        bounding_box: BoundingBox { min: bmin, max: bmax },
        vertices: vertices.to_vec(),
        triangles: vec![[1, 3, 0], [3, 2, 0]],
    })
}

fn build_simplified_terrain_mesh(
    dense_verts: &[DenseVertex],
    simplified_indices: &[u32],
    remap: &mut Vec<u32>,
) -> Result<Option<TerrainMesh>> {
    let (source_triangles, []) = simplified_indices.as_chunks::<3>() else {
        bail!("meshopt returned a non-triangular simplified index buffer");
    };
    if source_triangles.is_empty() {
        return Ok(None);
    }

    let mut mesh = TerrainMesh::default();

    remap.clear();
    remap.resize(dense_verts.len(), u32::MAX);

    mesh.triangles.reserve(source_triangles.len());

    for &[i0, i1, i2] in source_triangles {
        mesh.triangles.push([
            remap_simplified_vertex(i0, dense_verts, remap, &mut mesh.vertices)?,
            remap_simplified_vertex(i1, dense_verts, remap, &mut mesh.vertices)?,
            remap_simplified_vertex(i2, dense_verts, remap, &mut mesh.vertices)?,
        ]);
    }

    let Some(bounding_box) = terrain_bounding_box(&mesh.vertices) else {
        return Ok(None);
    };
    if is_deep_water_z(bounding_box.min.z) && is_deep_water_z(bounding_box.max.z) {
        return Ok(None);
    }

    let center = (bounding_box.min + bounding_box.max) * 0.5;

    mesh.bounding_box = bounding_box;
    mesh.bounding_sphere = terrain_bounding_sphere(&mesh.vertices, center);

    let raw_indices = mesh.triangles.as_flattened_mut();
    meshopt::optimize_vertex_cache_in_place(raw_indices, mesh.vertices.len());
    // Reorder in place and drop the unreferenced tail, matching `statics::model::process`;
    // the allocating `optimize_vertex_fetch` would copy the whole buffer per emitted chunk.
    let next_vertex = meshopt::optimize_vertex_fetch_in_place(raw_indices, &mut mesh.vertices);
    mesh.vertices.truncate(next_vertex);

    Ok(Some(mesh))
}

#[inline]
fn remap_simplified_vertex(
    source_index: u32,
    dense_verts: &[DenseVertex],
    remap: &mut [u32],
    vertices: &mut Vec<TerrainVertex>,
) -> Result<u32> {
    let source_index = source_index as usize;
    let Some(dense) = dense_verts.get(source_index) else {
        bail!(
            "meshopt index {} exceeds dense vertex count {}",
            source_index,
            dense_verts.len()
        );
    };

    let out_index = &mut remap[source_index];
    if *out_index == u32::MAX {
        *out_index = vertices.len() as u32;
        vertices.push(TerrainVertex {
            position: dense.position,
            normal: pack_ubyte4n_bias_normal(dense.raw_normal),
            color: pack_vertex_color(Vec4::from_array(dense.color)),
        });
    }

    Ok(*out_index)
}

#[inline]
fn is_deep_water_z(z: f32) -> bool {
    (z - DEEP_WATER_Z).abs() <= DEEP_WATER_EPSILON
}

fn terrain_bounding_box(vertices: &[TerrainVertex]) -> Option<BoundingBox> {
    let (&first, rest) = vertices.split_first()?;
    let first_position = first.position;
    let mut min = first_position;
    let mut max = first_position;

    for vertex in rest {
        let position = vertex.position;
        min = min.min(position);
        max = max.max(position);
    }

    Some(BoundingBox { min, max })
}

fn terrain_bounding_sphere(vertices: &[TerrainVertex], center: Vec3) -> BoundingSphere {
    let radius = vertices
        .iter()
        .fold(0.0f32, |acc, vertex| acc.max(vertex.position.distance_squared(center)))
        .sqrt();

    BoundingSphere { center, radius }
}

fn pack_vertex_color(color: Vec4) -> [u8; 4] {
    pack_d3dcolor_vclr(float_to_u8(color.x), float_to_u8(color.y), float_to_u8(color.z), u8::MAX)
}

fn float_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests;
