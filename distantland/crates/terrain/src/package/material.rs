use super::*;
use hashbrown::HashSet;
use itertools::Itertools;
use rayon::prelude::*;
use smallvec::{SmallVec, smallvec};

pub(super) const FLAG_HAS_DECAL: u8 = 1 << 0;
pub(super) const FLAG_USES_DEFAULT_TEXTURE: u8 = 1 << 1;
pub(super) type BlendAlphaGrid = [[u8; 5]; 5];
pub(super) const EMPTY_BLEND_ALPHA_GRID: BlendAlphaGrid = [[0; 5]; 5];

#[derive(Clone, Debug)]
pub(super) struct TerrainTextureIds<'a> {
    pub(super) ordered_paths: Vec<&'a str>,
    pub(super) ids_by_path: HashMap<&'a str, u16>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(super) struct PatchMaterial {
    pub(super) base_id: u16,
    pub(super) decal_id: u16,
    pub(super) pattern_id: u8,
    pub(super) flags: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct BlendPatternAtlasSpec {
    pub(super) pattern_count: u32,
    pub(super) logical_tile_size: u32,
    pub(super) gutter_size: u32,
    pub(super) physical_tile_size: u32,
    pub(super) tiles_per_row: u32,
    pub(super) atlas_width: u32,
    pub(super) atlas_height: u32,
}

#[derive(Clone, Debug)]
pub(super) struct TerrainMaterialPlan {
    pub(super) patch_materials: Vec<PatchMaterial>,
    pub(super) patterns: Vec<BlendAlphaGrid>,
    pub(super) pattern_atlas: BlendPatternAtlasSpec,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct UnassignedPatchMaterial {
    pub(super) base_id: u16,
    pub(super) decal_id: u16,
    pub(super) alpha_grid: BlendAlphaGrid,
    pub(super) flags: u8,
}

pub(super) fn collect_terrain_texture_ids<'a>(
    cache: &TerrainTextureCache<'a>,
    mode: TextureDedupeMode,
) -> Result<TerrainTextureIds<'a>> {
    let default_key = cache.default_key;

    // Successfully loaded originals (excluding the default), in deterministic sorted order.
    let loaded = cache
        .images
        .keys()
        .copied()
        .filter(|path| *path != default_key)
        .sorted_unstable()
        .dedup()
        .collect_vec();

    // Tier 0 source-byte dedupe over `bytes_hash` (the BLAKE3 of the on-disk bytes). Terrain
    // Material identity is Tier-0 only: decoded mip/BC1 identity is deferred, so we never hash `levels`.
    // Under `Off` every original is its own canonical, preserving pre-dedupe ID assignment.
    let (canonical_paths, alias): (Vec<&'a str>, HashMap<&'a str, &'a str>) = match mode {
        TextureDedupeMode::Off => (loaded.clone(), loaded.iter().map(|path| (*path, *path)).collect()),
        TextureDedupeMode::Exact => build_alias_map(loaded.iter().map(|path| (*path, cache.images[*path].bytes_hash))),
    };

    ensure!(
        canonical_paths.len() < usize::from(u16::MAX),
        "Too many terrain textures for u16 material IDs: {}",
        canonical_paths.len()
    );

    // One material ID / source-atlas tile per canonical, with the default texture at ID 0.
    let mut ordered_paths = Vec::with_capacity(canonical_paths.len() + 1);
    ordered_paths.push(default_key);
    ordered_paths.extend(canonical_paths.iter().copied());
    let canon_id = ordered_paths
        .iter()
        .enumerate()
        .map(|(index, path)| (*path, u16::try_from(index).expect("terrain texture ID fits in u16")))
        .collect::<HashMap<&'a str, u16>>();

    // `ids_by_path` covers every loaded original: an aliased original resolves to its canonical's
    // ID, so a cell referencing an aliased-away texture never silently falls back to ID 0.
    let mut ids_by_path = HashMap::with_capacity(loaded.len() + 1);
    ids_by_path.insert(default_key, 0);
    for original in &loaded {
        let canonical = alias.get(original).copied().unwrap_or(*original);
        ids_by_path.insert(*original, canon_id[canonical]);
    }

    Ok(TerrainTextureIds {
        ordered_paths,
        ids_by_path,
    })
}

/// Computes terrain albedo deduplication stats for manifest reporting.
///
/// Terrain material identity is Tier-0 source-byte only, so `decoded_alias_count` is always 0.
pub(super) fn terrain_dedupe_stats(
    inputs: &crate::texture::TerrainTextureInputs<'_>,
    cache: &TerrainTextureCache<'_>,
    texture_ids: &TerrainTextureIds<'_>,
) -> TextureDedupeDomainStats {
    let referenced = inputs
        .ordered_paths
        .iter()
        .filter(|path| **path != inputs.default_key)
        .count();
    let loaded = cache.images.keys().filter(|path| **path != cache.default_key).count();
    let canonical = texture_ids.ordered_paths.len().saturating_sub(1);
    TextureDedupeDomainStats {
        input_count: referenced,
        canonical_count: canonical,
        source_alias_count: loaded.saturating_sub(canonical),
        decoded_alias_count: 0,
        missing_to_default_count: referenced.saturating_sub(loaded),
    }
}

pub(super) fn build_terrain_material_plan<'a>(
    terrain_cells: &crate::texture::TerrainCells<'a>,
    texture_ids: &TerrainTextureIds<'a>,
    region: TerrainControlRegion,
) -> Result<TerrainMaterialPlan> {
    let default_key = texture_ids
        .ordered_paths
        .first()
        .copied()
        .unwrap_or(crate::texture::DEFAULT_LAND_TEXTURE);
    let mut cell_keys = terrain_cells.keys().copied().collect_vec();
    cell_keys.sort_unstable();

    // At most 256 patterns survive deduplication, so intern grids as they are planned rather
    // than retaining one copy per patch: a large load order would otherwise hold a second
    // multiset the size of every patch in the world and sort it in full.
    //
    // Patches are written straight into the output grid carrying their interned slot in
    // `pattern_id`, which one pass rewrites to the atlas ID once the sorted order is known.
    // Staging every patch in the world in a `Vec` first would cost ~40 bytes each against
    // the 6 bytes each occupies here.
    let mut pattern_slots: HashMap<BlendAlphaGrid, u8> = HashMap::with_capacity(64);
    pattern_slots.insert(EMPTY_BLEND_ALPHA_GRID, 0);

    let [width, height] = region.material_size_xy;
    let mut patch_materials = vec![
        PatchMaterial {
            base_id: 0,
            decal_id: 0,
            // Slot 0 is the empty grid, remapped to its atlas ID with every other patch below.
            pattern_id: 0,
            flags: FLAG_USES_DEFAULT_TEXTURE,
        };
        width as usize * height as usize
    ];

    for (cell_x, cell_y) in cell_keys {
        let origin_x = u32::try_from(cell_x - region.origin_cell[0]).expect("cell X inside terrain region") * 16;
        let origin_y = u32::try_from(cell_y - region.origin_cell[1]).expect("cell Y inside terrain region") * 16;
        let cell_plan = plan_landscape_cell_with_sampler((cell_x, cell_y), |grid, patch_x, patch_y| {
            sample_patch_texture_identity(terrain_cells, texture_ids, default_key, grid, patch_x, patch_y)
        });

        for patch in cell_plan.patches {
            let material = build_unassigned_patch_material(&patch.texturing, texture_ids);
            let slot = match pattern_slots.get(&material.alpha_grid) {
                Some(slot) => *slot,
                None => {
                    let Ok(slot) = u8::try_from(pattern_slots.len()) else {
                        bail!(
                            "Too many distinct terrain blend patterns for u8 pattern IDs: more than {}",
                            usize::from(u8::MAX) + 1
                        );
                    };
                    pattern_slots.insert(material.alpha_grid, slot);
                    slot
                }
            };
            let x = (origin_x + patch.patch_x as u32) as usize;
            let y = (origin_y + patch.patch_y as u32) as usize;
            patch_materials[y * width as usize + x] = PatchMaterial {
                base_id: material.base_id,
                decal_id: material.decal_id,
                pattern_id: slot,
                flags: material.flags,
            };
        }
    }

    let patterns = collect_distinct_blend_patterns(pattern_slots.keys().copied())?;
    let pattern_atlas = choose_blend_pattern_atlas(patterns.len())?;

    // Slots are numbered first-seen; atlas IDs are the sorted pattern order.
    let mut atlas_id_for_slot = [0u8; 256];
    for (grid, slot) in &pattern_slots {
        atlas_id_for_slot[usize::from(*slot)] = u8::try_from(
            patterns
                .binary_search(grid)
                .expect("interned alpha grid must be in the deduplicated atlas"),
        )
        .expect("pattern ID fits in u8");
    }
    for material in &mut patch_materials {
        material.pattern_id = atlas_id_for_slot[usize::from(material.pattern_id)];
    }

    Ok(TerrainMaterialPlan {
        patch_materials,
        patterns,
        pattern_atlas,
    })
}

pub(super) fn sample_patch_texture_identity<'a>(
    terrain_cells: &crate::texture::TerrainCells<'a>,
    texture_ids: &TerrainTextureIds<'a>,
    default_key: &'a str,
    grid: (i32, i32),
    patch_x: usize,
    patch_y: usize,
) -> SampledTextureIdentity<'a> {
    let Some(cell) = terrain_cells.get(&grid) else {
        return SampledTextureIdentity::default();
    };
    let raw_vtex_index = raw_vtex_index_at_patch(cell, patch_x, patch_y);
    if raw_vtex_index == 0 {
        return SampledTextureIdentity::default();
    }

    let path = resolve_raw_vtex(cell, raw_vtex_index, default_key);
    let id = texture_ids.ids_by_path.get(path).copied().unwrap_or(0);
    SampledTextureIdentity {
        id: Some(u32::from(id)),
        name: Some(path),
    }
}

pub(super) fn build_unassigned_patch_material<'a>(
    texturing: &LandscapePatchTexturing<'a>,
    texture_ids: &TerrainTextureIds<'a>,
) -> UnassignedPatchMaterial {
    let base_id = texture_id_for_planned_texture(texturing.base_texture, texture_ids);
    let has_decal = texturing.decal_texture.is_some();
    let decal_id = if has_decal {
        texture_id_for_planned_texture(texturing.decal_texture, texture_ids)
    } else {
        base_id
    };
    let mut flags = 0;
    if has_decal {
        flags |= FLAG_HAS_DECAL;
    }
    if base_id == 0 || (has_decal && decal_id == 0) {
        flags |= FLAG_USES_DEFAULT_TEXTURE;
    }

    UnassignedPatchMaterial {
        base_id,
        decal_id,
        alpha_grid: texturing.alpha_grid,
        flags,
    }
}

fn texture_id_for_planned_texture<'a>(texture: Option<&'a str>, texture_ids: &TerrainTextureIds<'a>) -> u16 {
    match texture {
        None => 0,
        Some(texture) if is_default_texture_name(texture) => 0,
        Some(texture) => texture_ids.ids_by_path.get(texture).copied().unwrap_or(0),
    }
}

pub(super) fn is_default_texture_name(texture: &str) -> bool {
    texture.eq_ignore_ascii_case(crate::texture::DEFAULT_LAND_TEXTURE)
        || texture.eq_ignore_ascii_case(crate::landscape_plan::DEFAULT_LAND_TEXTURE_DDS)
}

pub(super) fn collect_distinct_blend_patterns(
    patterns: impl IntoIterator<Item = BlendAlphaGrid>,
) -> Result<Vec<BlendAlphaGrid>> {
    let mut patterns = patterns.into_iter().sorted_unstable().dedup().collect_vec();
    if patterns.is_empty() {
        patterns.push(EMPTY_BLEND_ALPHA_GRID);
    }
    ensure!(
        patterns.len() <= usize::from(u8::MAX) + 1,
        "Too many distinct terrain blend patterns for u8 pattern IDs: {}",
        patterns.len()
    );
    Ok(patterns)
}

pub(super) fn choose_blend_pattern_atlas(pattern_count: usize) -> Result<BlendPatternAtlasSpec> {
    ensure!(
        pattern_count <= usize::from(u8::MAX) + 1,
        "Too many distinct terrain blend patterns for u8 pattern IDs: {}",
        pattern_count
    );
    let pattern_count = u32::try_from(pattern_count.max(1)).expect("pattern count fits in u32");
    let logical_tile_size = DEFAULT_PATTERN_TILE_SIZE;
    let gutter_size = DEFAULT_PATTERN_GUTTER_SIZE;
    let physical_tile_size = logical_tile_size + gutter_size * 2;
    let tiles_per_row = ceil_sqrt_u32(pattern_count);
    let rows = pattern_count.div_ceil(tiles_per_row);
    Ok(BlendPatternAtlasSpec {
        pattern_count,
        logical_tile_size,
        gutter_size,
        physical_tile_size,
        tiles_per_row,
        atlas_width: tiles_per_row * physical_tile_size,
        atlas_height: rows * physical_tile_size,
    })
}

pub(super) fn build_terrain_material_image(material_size_xy: [u32; 2], patch_materials: &[PatchMaterial]) -> RgbaImage {
    let [width, height] = material_size_xy;
    RgbaImage::from_fn(width, height, |x, y| {
        let material = patch_materials[y as usize * width as usize + x as usize];
        Rgba(pack_material_texel(material.base_id, material.decal_id))
    })
}

pub(super) fn build_terrain_material_flags_image(
    material_size_xy: [u32; 2],
    patch_materials: &[PatchMaterial],
) -> RgbaImage {
    let [width, height] = material_size_xy;
    RgbaImage::from_fn(width, height, |x, y| {
        let material = patch_materials[y as usize * width as usize + x as usize];
        Rgba(pack_material_flags_texel(material.pattern_id, material.flags))
    })
}

pub(super) fn build_patch_albedo_image<'a>(
    cache: &TerrainTextureCache<'a>,
    texture_ids: &TerrainTextureIds<'a>,
    material_size_xy: [u32; 2],
    patch_materials: &[PatchMaterial],
    pattern_images: &[RgbaImage],
) -> RgbaImage {
    let [width, height] = material_size_xy;
    // Blending indexes the rasterized alpha tile at the patch sample coordinate, so the sample
    // grid is derived from the tile size the patterns were rasterized at rather than passed
    // separately, where the two could drift apart.
    let patch_grid = patch_sample_grid(pattern_images.first().map_or(1, RgbaImage::width));

    // Averaging is expensive per unique material, so dedupe first and compute in parallel.
    let unique_materials: Vec<PatchMaterial> = {
        let mut seen = HashSet::with_capacity(64);
        patch_materials.iter().copied().filter(|m| seen.insert(*m)).collect()
    };
    let material_albedos: HashMap<PatchMaterial, Rgba<u8>> = unique_materials
        .into_par_iter()
        .map(|material| {
            let albedo = average_patch_material_albedo(cache, texture_ids, material, pattern_images, &patch_grid);
            (material, albedo)
        })
        .collect();

    RgbaImage::from_fn(width, height, |x, y| {
        let material = patch_materials[y as usize * width as usize + x as usize];
        material_albedos[&material]
    })
}

pub(super) fn build_blend_patterns_image(pattern_images: &[RgbaImage], pattern_atlas: BlendPatternAtlasSpec) -> RgbaImage {
    let mut atlas = RgbaImage::from_pixel(
        pattern_atlas.atlas_width.max(1),
        pattern_atlas.atlas_height.max(1),
        Rgba([0, 0, 0, 255]),
    );

    for (index, logical) in pattern_images.iter().enumerate() {
        let tile_x = u32::try_from(index).expect("pattern index fits in u32") % pattern_atlas.tiles_per_row;
        let tile_y = u32::try_from(index).expect("pattern index fits in u32") / pattern_atlas.tiles_per_row;
        blit_clamped_tile(
            &mut atlas,
            tile_x * pattern_atlas.physical_tile_size,
            tile_y * pattern_atlas.physical_tile_size,
            logical,
            pattern_atlas.gutter_size,
        );
    }

    atlas
}

/// Rasterizes every blend pattern once at `logical_tile_size`.
///
/// The patch-albedo pass and the blend-pattern atlas consume the same tiles, so they share
/// one rasterization instead of each producing its own copy.
pub(super) fn rasterize_blend_patterns(patterns: &[BlendAlphaGrid], logical_tile_size: u32) -> Vec<RgbaImage> {
    patterns
        .iter()
        .map(|pattern| rasterize_blend_pattern(pattern, logical_tile_size))
        .collect()
}

pub(super) fn rasterize_blend_pattern(alpha_grid: &BlendAlphaGrid, logical_tile_size: u32) -> RgbaImage {
    RgbaImage::from_fn(logical_tile_size.max(1), logical_tile_size.max(1), |x, y| {
        let patch_x = ((x as f32 + 0.5) / logical_tile_size.max(1) as f32) * 4.0;
        let patch_y = ((y as f32 + 0.5) / logical_tile_size.max(1) as f32) * 4.0;
        let alpha = sample_alpha_grid(alpha_grid, patch_x, patch_y);
        Rgba([alpha, 0, 0, 255])
    })
}

pub(super) fn sample_alpha_grid(alpha_grid: &BlendAlphaGrid, x: f32, y: f32) -> u8 {
    let points = terrain_triangle_at_cell_coord(x, y);
    let coords = points.map(|(px, py)| Vec2::new(px as f32, py as f32));
    let bary = barycentric_weights(Vec2::new(x, y), coords[0], coords[1], coords[2]);
    let values = points.map(|(px, py)| alpha_grid[py][px] as f32);
    (bary.x * values[0] + bary.y * values[1] + bary.z * values[2])
        .round()
        .clamp(0.0, 255.0) as u8
}

fn blit_clamped_tile(atlas: &mut RgbaImage, origin_x: u32, origin_y: u32, tile: &RgbaImage, gutter_size: u32) {
    let logical_tile_size = tile.width();
    let physical_tile_size = logical_tile_size + gutter_size * 2;
    for y in 0..physical_tile_size {
        let source_y = y.saturating_sub(gutter_size).min(logical_tile_size.saturating_sub(1));
        for x in 0..physical_tile_size {
            let source_x = x.saturating_sub(gutter_size).min(logical_tile_size.saturating_sub(1));
            atlas.put_pixel(origin_x + x, origin_y + y, *tile.get_pixel(source_x, source_y));
        }
    }
}

pub(crate) const fn pack_material_texel(base_id: u16, decal_id: u16) -> [u8; 4] {
    [
        (base_id & 0x00ff) as u8,
        (base_id >> 8) as u8,
        (decal_id & 0x00ff) as u8,
        (decal_id >> 8) as u8,
    ]
}

pub(crate) const fn pack_material_flags_texel(pattern_id: u8, flags: u8) -> [u8; 4] {
    [pattern_id, flags, 0, u8::MAX]
}

pub(super) fn average_patch_material_albedo<'a>(
    cache: &TerrainTextureCache<'a>,
    texture_ids: &TerrainTextureIds<'a>,
    material: PatchMaterial,
    pattern_images: &[RgbaImage],
    patch_grid: &SampleGrid,
) -> Rgba<u8> {
    let width = patch_grid.width() as usize;
    let height = patch_grid.height() as usize;
    let base_slot = prepare_texture_slot(texture_for_material_id(cache, texture_ids, material.base_id), patch_grid);
    let has_decal = (material.flags & FLAG_HAS_DECAL) != 0;
    let decal_slot =
        has_decal.then(|| prepare_texture_slot(texture_for_material_id(cache, texture_ids, material.decal_id), patch_grid));
    let alpha_image = has_decal.then(|| pattern_images.get(material.pattern_id as usize).unwrap_or(&pattern_images[0]));

    let mut base_row: SmallVec<[Vec4; DEFAULT_PATTERN_TILE_SIZE as usize]> = smallvec![Vec4::ZERO; width];
    let mut base_scratch: SmallVec<[Vec4; DEFAULT_PATTERN_TILE_SIZE as usize]> = smallvec![Vec4::ZERO; width];
    let mut decal_row: SmallVec<[Vec4; DEFAULT_PATTERN_TILE_SIZE as usize]> = smallvec![Vec4::ZERO; width];
    let mut decal_scratch: SmallVec<[Vec4; DEFAULT_PATTERN_TILE_SIZE as usize]> = smallvec![Vec4::ZERO; width];
    let mut rgb_sum = [0.0_f32; 3];

    for py in 0..height {
        sample_precomputed_texture_lod_row(&base_slot, py, &mut base_row, &mut base_scratch);
        if let Some(slot) = decal_slot.as_ref() {
            sample_precomputed_texture_lod_row(slot, py, &mut decal_row, &mut decal_scratch);
        }

        for px in 0..width {
            let sample = blend_patch_material_sample(&base_row, decal_slot.as_ref(), &decal_row, alpha_image, px, py);
            rgb_sum[0] += srgb_sample_to_linear(sample.x);
            rgb_sum[1] += srgb_sample_to_linear(sample.y);
            rgb_sum[2] += srgb_sample_to_linear(sample.z);
        }
    }

    let sample_count = (width * height).max(1) as f32;
    Rgba([
        linear_to_srgb_u8(rgb_sum[0] / sample_count),
        linear_to_srgb_u8(rgb_sum[1] / sample_count),
        linear_to_srgb_u8(rgb_sum[2] / sample_count),
        u8::MAX,
    ])
}

fn blend_patch_material_sample(
    base_row: &[Vec4],
    decal_slot: Option<&PreparedTextureSlot<'_>>,
    decal_row: &[Vec4],
    alpha_image: Option<&RgbaImage>,
    px: usize,
    py: usize,
) -> Vec4 {
    let base = base_row[px];
    let Some(_decal_slot) = decal_slot else {
        return base;
    };
    let alpha = alpha_image
        .map(|image| image.get_pixel(px as u32, py as u32)[0] as f32 / 255.0)
        .unwrap_or(0.0);
    lerp4(base, decal_row[px], alpha)
}

fn texture_for_material_id<'a>(
    cache: &'a TerrainTextureCache<'a>,
    texture_ids: &TerrainTextureIds<'a>,
    texture_id: u16,
) -> &'a TerrainTexture {
    let path = texture_ids
        .ordered_paths
        .get(texture_id as usize)
        .copied()
        .unwrap_or_else(|| texture_ids.ordered_paths[0]);
    cache.get(path)
}

pub(super) fn patch_sample_grid(logical_tile_size: u32) -> SampleGrid {
    SampleGrid {
        x: patch_sample_coords(logical_tile_size),
        y: patch_sample_coords(logical_tile_size),
    }
}

fn patch_sample_coords(logical_tile_size: u32) -> Vec<f32> {
    let logical_tile_size = logical_tile_size.max(1);
    (0..logical_tile_size)
        .map(|sample| ((sample as f32 + 0.5) * 4.0 / logical_tile_size as f32).clamp(0.0, 3.999))
        .collect()
}
