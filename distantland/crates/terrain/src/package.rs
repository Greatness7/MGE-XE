use crate::landscape_plan::{LandscapePatchTexturing, SampledTextureIdentity, plan_landscape_cell_with_sampler};
use anyhow::{Result, bail, ensure};
use glam::{Vec2, Vec4};
use hashbrown::HashMap;
use image::{Rgba, RgbaImage};

use crate::UsageInfo;
use crate::mge_xe::distant_terrain::{TERRAIN_FILE_VERSION, TerrainFile, TerrainMesh};
use crate::mge_xe::world::LAND_CELL_SIZE;
use crate::texture::{
    Bc1Mip, PreparedTextureSlot, SampleGrid, TerrainTexture, TerrainTextureCache, barycentric_weights, bc1_payload_size,
    lerp4, linear_to_srgb_u8, prepare_texture_slot, raw_vtex_index_at_patch, resolve_raw_vtex,
    sample_precomputed_texture_lod_row, srgb_sample_to_linear, terrain_triangle_at_cell_coord,
};
use crate::texture_dedupe::{TextureDedupeDomainStats, TextureDedupeMode, build_alias_map};
use crate::vfs::Vfs;
use distantland_foundation::identity::SourceHash;
use distantland_foundation::units::TERRAIN_RECIPE_VERSION;

mod atlas;
mod material;

pub use atlas::*;
pub(crate) use material::*;

const DEFAULT_PATCH_SIZE: f32 = 512.0;
const DEFAULT_PATTERN_TILE_SIZE: u32 = 32;
const DEFAULT_PATTERN_GUTTER_SIZE: u32 = 2;
const LOW_FILL_RATIO_RECOMMENDATION_THRESHOLD: f64 = 0.5;

/// Cheap inputs and fingerprint computed before materialization.
///
/// Holds the loaded texture cache so later planning and encoding can reuse it without reloading.
pub struct TerrainPackageInputs<'a> {
    terrain_cache: TerrainTextureCache<'a>,
    texture_ids: TerrainTextureIds<'a>,
    region: TerrainControlRegion,
    atlas_spec: TerrainAtlasSpec,
    mesh_simplifier_config: crate::mesh::MeshSimplifierConfig,
    estimated_source_atlas_bytes: u64,
}

impl<'a> TerrainPackageInputs<'a> {
    /// Returns the already-loaded facts that define the coarse terrain gate.
    pub fn gate_inputs(&self) -> TerrainGateInputs {
        TerrainGateInputs {
            recipe_version: TERRAIN_RECIPE_VERSION,
            terrain_file_version: TERRAIN_FILE_VERSION,
            land_cell_size: LAND_CELL_SIZE,
            default_patch_size: DEFAULT_PATCH_SIZE,
            pattern_tile_size: DEFAULT_PATTERN_TILE_SIZE,
            pattern_gutter_size: DEFAULT_PATTERN_GUTTER_SIZE,
            origin_cell: self.region.origin_cell,
            cell_size_xy: self.region.cell_size_xy,
            material_size_xy: self.region.material_size_xy,
            atlas_spec: [
                self.atlas_spec.logical_tile_size,
                self.atlas_spec.gutter_size,
                self.atlas_spec.physical_tile_size,
                self.atlas_spec.tiles_per_row,
                self.atlas_spec.atlas_size,
                self.atlas_spec.atlas_max_lod,
            ],
            mesh_chunk_cells_per_side: crate::mesh::MESH_CHUNK_CELLS_PER_SIDE as u64,
            mesh_target_error: self.mesh_simplifier_config.target_error,
            mesh_weights: [
                self.mesh_simplifier_config.weights.smoothed_normal,
                self.mesh_simplifier_config.weights.color,
            ],
            mesh_option_bits: crate::mesh::MESH_SIMPLIFIER_OPTION_BITS,
            ordered_texture_paths: self.texture_ids.ordered_paths.iter().map(|path| (*path).to_owned()).collect(),
            default_texture_key: self.terrain_cache.default_key.to_owned(),
            default_texture_hash: self.terrain_cache.default_texture().bytes_hash,
        }
    }

    /// Iterates normalized terrain texture keys and their already-computed source hashes.
    pub fn texture_identities(&self) -> impl Iterator<Item = (&str, SourceHash)> + '_ {
        self.terrain_cache
            .images
            .iter()
            .map(|(&key, texture)| (key, SourceHash(texture.bytes_hash)))
    }

    /// Number of unique terrain textures referenced by the load order.
    pub fn texture_count(&self) -> usize {
        self.texture_ids.ordered_paths.len()
    }

    /// Covered cell rectangle for the terrain occlusion asset: `(origin_cell, cell_size_xy)`.
    pub fn occlusion_region(&self) -> ([i32; 2], [u32; 2]) {
        (self.region.origin_cell, self.region.cell_size_xy)
    }

    /// Logical (content) tile size in the source atlas.
    pub fn logical_tile_size(&self) -> u32 {
        self.atlas_spec.logical_tile_size
    }

    /// Physical (content + gutter) tile size in the source atlas.
    pub fn physical_tile_size(&self) -> u32 {
        self.atlas_spec.physical_tile_size
    }

    /// Number of tiles per row in the source atlas.
    pub fn tiles_per_row(&self) -> u32 {
        self.atlas_spec.tiles_per_row
    }

    /// Total source atlas dimension (width = height).
    pub fn atlas_size(&self) -> u32 {
        self.atlas_spec.atlas_size
    }

    /// Maximum mip level stored in the source atlas.
    pub fn atlas_max_lod(&self) -> u32 {
        self.atlas_spec.atlas_max_lod
    }

    /// `[width, height]` of the material / material-flags control textures.
    pub fn material_size_xy(&self) -> [u32; 2] {
        self.region.material_size_xy
    }

    /// Estimated byte footprint of the BC1 source atlas mip chain.
    pub fn estimated_source_atlas_bytes(&self) -> u64 {
        self.estimated_source_atlas_bytes
    }

    /// Estimated byte footprint of the two RGBA8 control textures plus the
    /// BC1 patch-albedo mip chain.
    pub fn estimated_control_texture_bytes(&self) -> u64 {
        estimated_control_texture_bytes(self.region.material_size_xy)
    }
}

/// Loaded, immutable terrain facts used by the coarse package gate.
#[derive(Clone, Debug, PartialEq)]
pub struct TerrainGateInputs {
    /// Invalidation knob covering every terrain recipe: dedupe, blend-pattern rasterization,
    /// patch albedo, normal smoothing, and mesh generation.
    pub recipe_version: u32,
    /// Runtime terrain file format version.
    pub terrain_file_version: u32,
    /// LAND cell width in world units.
    pub land_cell_size: f32,
    /// Default material patch width in world units.
    pub default_patch_size: f32,
    /// Logical blend-pattern tile size.
    pub pattern_tile_size: u32,
    /// Blend-pattern gutter size.
    pub pattern_gutter_size: u32,
    /// Minimum covered absolute LAND cell.
    pub origin_cell: [i32; 2],
    /// Covered LAND-cell dimensions.
    pub cell_size_xy: [u32; 2],
    /// Material control-texture dimensions.
    pub material_size_xy: [u32; 2],
    /// Source-atlas layout parameters.
    pub atlas_spec: [u32; 6],
    /// Terrain mesh chunk width in LAND cells.
    pub mesh_chunk_cells_per_side: u64,
    /// Selected terrain mesh simplification error.
    pub mesh_target_error: f32,
    /// Terrain mesh attribute weights.
    pub mesh_weights: [f32; 2],
    /// Meshopt option bitset.
    pub mesh_option_bits: u32,
    /// Ordered normalized source texture paths.
    pub ordered_texture_paths: Vec<String>,
    /// Normalized default terrain texture key.
    pub default_texture_key: String,
    /// Content hash of the resolved default terrain texture.
    pub default_texture_hash: [u8; 32],
}

/// The five height-independent DDS images produced by the heavy encoding step.
#[derive(Clone, Debug)]
pub struct TerrainDdsImages {
    /// BC1 source terrain texture atlas mip chain.
    pub terrain_atlas: Bc1MipChain,
    /// Per-patch material indices.
    pub terrain_material: RgbaImage,
    /// Per-patch material flags.
    pub terrain_material_flags: RgbaImage,
    /// Per-patch blended albedo.
    pub terrain_patch_albedo: RgbaImage,
    /// Blend-pattern alpha atlas.
    pub terrain_blend_patterns: RgbaImage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TerrainControlRegion {
    pub(super) origin_cell: [i32; 2],
    pub(super) cell_size_xy: [u32; 2],
    pub(super) material_size_xy: [u32; 2],
    pub(super) populated_cell_count: usize,
}

/// Resource limits applied while planning terrain control textures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerrainControlTextureLimits {
    /// Maximum width or height of any control texture.
    pub max_size: u32,
    /// Maximum combined estimated control-texture byte footprint.
    pub max_bytes: u64,
}

impl Default for TerrainControlTextureLimits {
    fn default() -> Self {
        Self {
            max_size: crate::MAX_TERRAIN_CONTROL_TEXTURE_SIZE,
            max_bytes: crate::MAX_TERRAIN_CONTROL_TEXTURE_BYTES,
        }
    }
}

/// Cheap preparation: loads and hashes textures (unavoidable for the fingerprint), derives the
/// atlas specification and region, computes the fingerprint, and runs the guard-rail check.
pub fn prepare_terrain_package_inputs<'a>(
    usage_info: &UsageInfo<'a>,
    vfs: &'a Vfs,
    target_error: f32,
    terrain_mesh_weights: crate::mesh::MeshSimplifierWeights,
    max_terrain_texture_size: u32,
    max_terrain_atlas_size: u32,
    texture_dedupe_mode: TextureDedupeMode,
    control_texture_limits: TerrainControlTextureLimits,
) -> Result<TerrainPackageInputs<'a>> {
    let terrain_inputs = {
        let _span = tracing::info_span!("terrain.collect_texture_inputs", report = true).entered();
        TerrainTextureCache::collect_inputs(vfs, &usage_info.terrain_cells)
    };
    let atlas_spec = choose_source_atlas(
        terrain_inputs.ordered_paths.len(),
        max_terrain_atlas_size,
        max_terrain_texture_size,
    )?;
    let terrain_cache = TerrainTextureCache::load_inputs(vfs, &terrain_inputs);
    let texture_ids = {
        let _span = tracing::info_span!("terrain.collect_texture_ids", report = true).entered();
        collect_terrain_texture_ids(&terrain_cache, texture_dedupe_mode)?
    };
    let dedupe_stats = terrain_dedupe_stats(&terrain_inputs, &terrain_cache, &texture_ids);
    tracing::info!(
        "Terrain dedupe: inputs {} -> {} (source {}, missing_to_default {})",
        dedupe_stats.input_count,
        dedupe_stats.canonical_count,
        dedupe_stats.source_alias_count,
        dedupe_stats.missing_to_default_count
    );
    let region = terrain_control_region(&usage_info.terrain_cells);
    let mesh_simplifier_config = crate::mesh::selected_mesh_simplifier_config(target_error, terrain_mesh_weights);
    let estimated_source_atlas_bytes =
        bc1_payload_size(atlas_spec.atlas_size, atlas_spec.atlas_size, Some(atlas_spec.atlas_max_lod));
    validate_control_texture_region(region, control_texture_limits, estimated_source_atlas_bytes)?;

    Ok(TerrainPackageInputs {
        terrain_cache,
        texture_ids,
        region,
        atlas_spec,
        mesh_simplifier_config,
        estimated_source_atlas_bytes,
    })
}

/// Builds the height-independent package plan.
///
/// Computes the material plan and retains the loaded texture cache, atlas spec, and control region.
/// The material plan determines the five DDS products' content and carries the blend-pattern facts
/// the terrain file header needs; building the source atlas itself is deferred to
/// [`TerrainPackagePlan::build_dds_images`] along with the rest of the rasterize/encode work.
///
/// None of this reads LAND heights, so a mesh-only rebuild takes the plan for its header facts and
/// never calls [`TerrainPackagePlan::build_dds_images`].
pub fn plan_terrain_package<'a>(
    inputs: TerrainPackageInputs<'a>,
    usage_info: &UsageInfo<'a>,
) -> Result<TerrainPackagePlan<'a>> {
    let TerrainPackageInputs {
        terrain_cache,
        texture_ids,
        region,
        atlas_spec,
        mesh_simplifier_config,
        ..
    } = inputs;
    let material_plan = {
        let _span = tracing::info_span!("terrain.build_material_plan", report = true).entered();
        build_terrain_material_plan(&usage_info.terrain_cells, &texture_ids, region)?
    };
    Ok(TerrainPackagePlan {
        terrain_cache,
        texture_ids,
        region,
        atlas_spec,
        mesh_simplifier_config,
        material_plan,
    })
}

/// The height-independent half of a terrain package.
pub struct TerrainPackagePlan<'a> {
    terrain_cache: TerrainTextureCache<'a>,
    texture_ids: TerrainTextureIds<'a>,
    region: TerrainControlRegion,
    atlas_spec: TerrainAtlasSpec,
    mesh_simplifier_config: crate::mesh::MeshSimplifierConfig,
    material_plan: TerrainMaterialPlan,
}

impl<'a> TerrainPackagePlan<'a> {
    /// Mesh simplifier configuration selected for this run.
    pub fn mesh_simplifier_config(&self) -> crate::mesh::MeshSimplifierConfig {
        self.mesh_simplifier_config
    }

    /// Lowers a complete mesh list into the runtime terrain file using this plan's global facts.
    pub fn assemble_terrain_file(&self, meshes: Vec<TerrainMesh>) -> TerrainFile {
        assemble_terrain_file(self.region, self.atlas_spec, self.material_plan.pattern_atlas, meshes)
    }

    /// Decodes the source textures, then rasterizes and encodes the five height-independent DDS
    /// images.
    ///
    /// Decoding is deferred all the way to here: `terrain_cache` was only read and hashed by
    /// [`prepare_terrain_package_inputs`], since the unit identity and package gate need nothing
    /// but `bytes_hash`. This is the first point that actually needs decoded pixels, and it's
    /// only reached on a real DDS rebuild.
    pub fn build_dds_images(&mut self) -> TerrainDdsImages {
        self.terrain_cache.decode(self.atlas_spec.logical_tile_size);
        let material_size_xy = self.region.material_size_xy;
        let pattern_images = rasterize_blend_patterns(
            &self.material_plan.patterns,
            self.material_plan.pattern_atlas.logical_tile_size,
        );

        // The source atlas shares no inputs with the four control images, and neither arm
        // saturates the pool on its own: the atlas loop walks tiles serially and only the BC1
        // fallback encode is parallel, while the control images are parallel only inside patch
        // albedo. Spans are opened inside each arm with an explicit parent so the report keeps
        // its nesting (worker threads carry no contextual span) and still times just the work.
        let parent = tracing::Span::current();
        let (terrain_atlas, (terrain_material, terrain_material_flags, terrain_patch_albedo, terrain_blend_patterns)) =
            rayon::join(
                || {
                    let _span =
                        tracing::info_span!(parent: &parent, "terrain.build_atlas_bc1_chain", report = true).entered();
                    build_terrain_atlas_bc1_chain(&self.texture_ids, &self.terrain_cache, self.atlas_spec)
                },
                || {
                    let _span =
                        tracing::info_span!(parent: &parent, "terrain.build_control_images", report = true).entered();
                    let terrain_material = {
                        let _s = tracing::info_span!("terrain.build_material_image", report = true).entered();
                        build_terrain_material_image(material_size_xy, &self.material_plan.patch_materials)
                    };
                    let terrain_material_flags = {
                        let _s = tracing::info_span!("terrain.build_material_flags_image", report = true).entered();
                        build_terrain_material_flags_image(material_size_xy, &self.material_plan.patch_materials)
                    };
                    let terrain_patch_albedo = {
                        let _s = tracing::info_span!("terrain.build_patch_albedo_image", report = true).entered();
                        build_patch_albedo_image(
                            &self.terrain_cache,
                            &self.texture_ids,
                            material_size_xy,
                            &self.material_plan.patch_materials,
                            &pattern_images,
                        )
                    };
                    let terrain_blend_patterns =
                        build_blend_patterns_image(&pattern_images, self.material_plan.pattern_atlas);
                    (
                        terrain_material,
                        terrain_material_flags,
                        terrain_patch_albedo,
                        terrain_blend_patterns,
                    )
                },
            );

        TerrainDdsImages {
            terrain_atlas,
            terrain_material,
            terrain_material_flags,
            terrain_patch_albedo,
            terrain_blend_patterns,
        }
    }
}

fn terrain_control_region<'a>(terrain_cells: &crate::texture::TerrainCells<'a>) -> TerrainControlRegion {
    let Some((&(first_x, first_y), _)) = terrain_cells.first() else {
        return TerrainControlRegion {
            origin_cell: [0, 0],
            cell_size_xy: [0, 0],
            material_size_xy: [1, 1],
            populated_cell_count: 0,
        };
    };

    let ((min_x, min_y), (max_x, max_y)) = terrain_cells.keys().copied().fold(
        ((first_x, first_y), (first_x, first_y)),
        |((min_x, min_y), (max_x, max_y)), (x, y)| ((min_x.min(x), min_y.min(y)), (max_x.max(x), max_y.max(y))),
    );
    let width = (max_x - min_x + 1) as u32;
    let height = (max_y - min_y + 1) as u32;
    TerrainControlRegion {
        origin_cell: [min_x, min_y],
        cell_size_xy: [width, height],
        material_size_xy: [width.max(1) * 16, height.max(1) * 16],
        populated_cell_count: terrain_cells.len(),
    }
}

fn validate_control_texture_region(
    region: TerrainControlRegion,
    control_texture_limits: TerrainControlTextureLimits,
    estimated_source_atlas_bytes: u64,
) -> Result<()> {
    let [material_width, material_height] = region.material_size_xy;
    let estimated_control_texture_bytes = estimated_control_texture_bytes(region.material_size_xy);
    let dimension_over_limit =
        material_width > control_texture_limits.max_size || material_height > control_texture_limits.max_size;
    let bytes_over_limit = estimated_control_texture_bytes > control_texture_limits.max_bytes;

    if !dimension_over_limit && !bytes_over_limit {
        return Ok(());
    }

    let bounding_cell_count = u64::from(region.cell_size_xy[0]) * u64::from(region.cell_size_xy[1]);
    let fill_ratio = if bounding_cell_count == 0 {
        0.0
    } else {
        region.populated_cell_count as f64 / bounding_cell_count as f64
    };
    let mut failure_kinds = Vec::new();
    if dimension_over_limit {
        failure_kinds.push(format!(
            "dimension limit exceeded (material_size_xy={}x{} > {}x{})",
            material_width, material_height, control_texture_limits.max_size, control_texture_limits.max_size
        ));
    }
    if bytes_over_limit {
        failure_kinds.push(format!(
            "memory limit exceeded (estimated_control_texture_bytes={} > {})",
            estimated_control_texture_bytes, control_texture_limits.max_bytes
        ));
    }
    let recommendation = if fill_ratio < LOW_FILL_RATIO_RECOMMENDATION_THRESHOLD {
        " Low fill_ratio suggests a future sparse-region path or a user-provided cell clip."
    } else {
        ""
    };

    bail!(
        "Terrain control texture guard failed: {}. origin_cell=({}, {}), cell_size_xy={}x{}, bounding_cell_count={}, populated_cell_count={}, fill_ratio={fill_ratio:.3}, material_size_xy={}x{}, estimated_control_texture_bytes={}, estimated_source_atlas_bytes={}.{}",
        failure_kinds.join("; "),
        region.origin_cell[0],
        region.origin_cell[1],
        region.cell_size_xy[0],
        region.cell_size_xy[1],
        bounding_cell_count,
        region.populated_cell_count,
        material_width,
        material_height,
        estimated_control_texture_bytes,
        estimated_source_atlas_bytes,
        recommendation,
    );
}

/// Lowers a complete mesh list plus the package-global facts into the runtime terrain file.
fn assemble_terrain_file(
    region: TerrainControlRegion,
    atlas_spec: TerrainAtlasSpec,
    pattern_atlas: BlendPatternAtlasSpec,
    meshes: Vec<TerrainMesh>,
) -> TerrainFile {
    let world_origin = Vec2::new(
        region.origin_cell[0] as f32 * LAND_CELL_SIZE,
        region.origin_cell[1] as f32 * LAND_CELL_SIZE,
    );
    let world_size = Vec2::new(
        region.cell_size_xy[0] as f32 * LAND_CELL_SIZE,
        region.cell_size_xy[1] as f32 * LAND_CELL_SIZE,
    );
    TerrainFile {
        cell_size: LAND_CELL_SIZE,
        patch_size: DEFAULT_PATCH_SIZE,
        origin_cell: region.origin_cell,
        cell_size_xy: region.cell_size_xy,
        world_origin,
        world_size,
        atlas_size: atlas_spec.atlas_size,
        logical_tile_size: atlas_spec.logical_tile_size,
        gutter_size: atlas_spec.gutter_size,
        physical_tile_size: atlas_spec.physical_tile_size,
        tiles_per_row: atlas_spec.tiles_per_row,
        atlas_max_lod: atlas_spec.atlas_max_lod,
        material_size_xy: region.material_size_xy,
        pattern_count: pattern_atlas.pattern_count,
        pattern_tile_size: pattern_atlas.logical_tile_size,
        pattern_gutter_size: pattern_atlas.gutter_size,
        pattern_physical_size: pattern_atlas.physical_tile_size,
        patterns_per_row: pattern_atlas.tiles_per_row,
        meshes,
    }
}

fn estimated_control_texture_bytes(material_size_xy: [u32; 2]) -> u64 {
    let [width, height] = material_size_xy;
    let area = u64::from(width) * u64::from(height);
    area * 4 * 2 + bc1_payload_size(width, height, Some(u32::MAX))
}

fn ceil_sqrt_u32(value: u32) -> u32 {
    let mut root = 1_u32;
    while root.saturating_mul(root) < value {
        root += 1;
    }
    root
}

#[cfg(test)]
mod tests;
