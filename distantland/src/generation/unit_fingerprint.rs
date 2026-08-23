//! Construction of immutable unit fingerprints at their valid input lifetimes.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;

use hashbrown::HashMap;
use itertools::Itertools;

use crate::generation::cache::{
    STATIC_BUNDLE_INPUT_FINGERPRINT_MAGIC, STATIC_SHARD_ASSIGNMENT_MAGIC, STATIC_SHARD_INPUT_FINGERPRINT_MAGIC,
    TERRAIN_PACKAGE_INPUT_FINGERPRINT_MAGIC,
};
use crate::generation::identity::ContentIdentityCollector;
use crate::generation::identity::MeshResolutionRule;
use crate::generation::job::write_generation_settings_canonical;
use crate::generation::output::STATIC_MESH_SHARD_COUNT;
use crate::generation::projection::{LandscapeCellProjection, Projections};
use crate::generation::state_db::{FingerprintTable, GenerationState, ReverseIndexes, UnitFingerprintTables};
use crate::generation::units::{
    CanonicalWrite, CanonicalWriter, MergeUnitKey, STATICS_RECIPE_VERSION, TERRAIN_RECIPE_VERSION, TerrainCellUnitKey,
    TerrainChunkUnitKey, UnitSettingsPartitions, write_hash_option,
};
use crate::mge_xe::distant_statics::StaticType;
use crate::statics::atlas::sizing::{AtlasDomain, SizingPlan, SourceTextureInfo};
use crate::terrain::package::TerrainGateInputs;
use crate::usage::StableRefKey;
use crate::{AtlasTextureSet, DistantStatics, GenerationSettings, IndexSet, TerrainAtlasLayout, UsageInfo, Vfs};

/// Builds all statics-domain state before atlas lowering and merge mutation destroy logical inputs.
pub(crate) fn build_static_state(
    settings: &GenerationSettings,
    projections: &Projections,
    identities: &ContentIdentityCollector,
    usage: &UsageInfo<'_>,
    distant_statics: &DistantStatics,
    vfs: &Vfs,
    textures: &AtlasTextureSet<IndexSet<String>>,
    source_info: &HashMap<String, SourceTextureInfo>,
    sizing_plan: &SizingPlan,
) -> GenerationState {
    let partitions = UnitSettingsPartitions::from_settings(settings);
    let mut mesh_keys = BTreeSet::new();
    mesh_keys.extend(usage.mesh_scale_maximums.keys().map(|key| (*key).to_owned()));
    mesh_keys.extend(identities.meshes().keys().cloned());
    mesh_keys.extend(identities.mesh_resolutions().keys().cloned());
    mesh_keys.extend(distant_statics.keys().cloned());

    let mut texture_to_mesh: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    let mut writer = CanonicalWriter::new();
    let mut mesh_entries = Vec::with_capacity(mesh_keys.len());
    for key in mesh_keys {
        let distant_static = distant_statics.get(&key);
        writer.write_str("mesh_unit_v1");
        writer.write_u32(STATICS_RECIPE_VERSION);
        writer.write_str(&key);
        partitions.mesh.write_canonical(&mut writer);
        write_mesh_resolution(&mut writer, identities.mesh_resolutions().get(&key));
        write_content_identity(&mut writer, identities.meshes().get(&key));
        write_option_f32(&mut writer, usage.mesh_scale_maximums.get(key.as_str()).copied());
        writer.write_bool(usage.door_meshes.contains(key.as_str()));
        writer.write_bool(usage.forced_meshes.contains(key.as_str()));
        match projections.overrides.meshes.get(&key) {
            Some(override_) => {
                writer.write_bool(true);
                override_.write_canonical(&mut writer);
            }
            None => writer.write_bool(false),
        }
        match distant_static {
            Some(distant_static) => {
                writer.write_u8(1);
                writer.write_u8(distant_static.static_type as u8);
                write_vec3(&mut writer, distant_static.bounding_sphere.center);
                writer.write_f32(distant_static.bounding_sphere.radius);
                write_vec3(&mut writer, distant_static.bounding_box.min);
                write_vec3(&mut writer, distant_static.bounding_box.max);
                writer.write_f32(distant_static.max_scale);
                writer.write_bool(distant_static.is_door);
                writer.write_u64(distant_static.subsets.len() as u64);
                for subset in &distant_static.subsets {
                    writer.write_bool(subset.has_alpha);
                    writer.write_bool(subset.has_uv_controller);
                    match subset.texture.source_sym().and_then(|sym| vfs.texture_key_for_sym(sym)) {
                        Some(texture) => {
                            writer.write_bool(true);
                            writer.write_str(texture);
                            texture_to_mesh.entry(texture).or_default().insert(key.clone());
                        }
                        None => writer.write_bool(false),
                    }
                }
            }
            None => writer.write_u8(absent_mesh_status(
                identities.mesh_resolutions().contains_key(&key),
                identities.meshes().contains_key(&key),
            )),
        }
        mesh_entries.push((key, writer.finish_and_clear()));
    }
    let mesh = FingerprintTable::from_entries(mesh_entries);
    // Point lookups only, during merge-unit construction; the ordering lives in `mesh.entries`.
    let mesh_fingerprints: HashMap<_, _> = mesh.entries.iter().cloned().collect();

    let (merge, mesh_to_merge) = build_merge_units(usage, distant_statics, &mesh_fingerprints, &partitions, projections);
    let texture = build_texture_units(textures, source_info, sizing_plan, &partitions);
    let statics_global = fingerprint_statics_global(projections, usage);

    GenerationState {
        settings_identity: fingerprint_settings(settings),
        units: UnitFingerprintTables {
            mesh,
            merge,
            texture,
            ..UnitFingerprintTables::default()
        },
        reverse: ReverseIndexes {
            mesh_to_merge,
            texture_to_mesh: finish_reverse_index(texture_to_mesh),
            texture_to_terrain_cell: Vec::new(),
        },
        statics_global,
        terrain_global: [0; 32],
        terrain_enabled: false,
        ..GenerationState::default()
    }
}

pub(crate) fn populate_terrain_state(
    state: &mut GenerationState,
    settings: &GenerationSettings,
    projections: &Projections,
    identities: &ContentIdentityCollector,
    layout: &TerrainAtlasLayout,
    gate_inputs: Option<&TerrainGateInputs>,
) {
    if !settings.generate_terrain {
        state.units.terrain_cell = FingerprintTable::default();
        state.units.terrain_chunk = FingerprintTable::default();
        state.reverse.texture_to_terrain_cell.clear();
        state.terrain_global = [0; 32];
        state.terrain_records.clear();
        state.terrain_dds_domain_digest = [0; 32];
        state.terrain_enabled = false;
        return;
    }

    let partitions = UnitSettingsPartitions::from_settings(settings);
    let mut texture_to_cells: BTreeMap<&str, BTreeSet<TerrainCellUnitKey>> = BTreeMap::new();
    let mut writer = CanonicalWriter::new();
    let mut terrain_cells = Vec::with_capacity(projections.landscapes.len());
    // Hoisted rather than rebuilt per cell. A `BTreeSet` cannot be reused across iterations.
    // `BTreeSet::clear` drops the root and frees every node, while `Vec::clear` keeps the
    // capacity, and sorting `&str` reproduces the set's iteration order exactly.
    let mut dependencies: Vec<&str> = Vec::new();
    for (&(x, y), own) in &projections.landscapes {
        let cell_key = TerrainCellUnitKey::new(x, y);
        writer.write_str("terrain_cell_unit_v1");
        writer.write_u32(TERRAIN_RECIPE_VERSION);
        writer.write_i32(x);
        writer.write_i32(y);
        partitions.terrain_cell.write_canonical(&mut writer);
        write_landscape_projection(&mut writer, own, true);
        dependencies.clear();
        for (dx, dy) in [(0, 0), (0, 1), (-1, 0), (-1, 1)] {
            writer.write_i32(dx);
            writer.write_i32(dy);
            match projections.landscapes.get(&(x + dx, y + dy)) {
                Some(source) => {
                    writer.write_bool(true);
                    write_hash_option(&mut writer, source.material_hash);
                    for texture in &source.referenced_textures {
                        dependencies.push(texture.as_str());
                    }
                }
                None => writer.write_bool(false),
            }
        }
        dependencies.sort_unstable();
        dependencies.dedup();
        writer.write_u64(dependencies.len() as u64);
        for &texture in &dependencies {
            writer.write_str(texture);
            match identities.terrain_textures().get(texture) {
                Some(hash) => {
                    writer.write_bool(true);
                    writer.write_bytes(&hash.0);
                }
                None => writer.write_bool(false),
            }
            texture_to_cells.entry(texture).or_default().insert(cell_key);
        }
        terrain_cells.push((cell_key, writer.finish_and_clear()));
    }

    let chunks = projections
        .landscapes
        .keys()
        .map(|&(x, y)| TerrainChunkUnitKey::from_cell(TerrainCellUnitKey::new(x, y)))
        .collect::<BTreeSet<_>>();
    let mut terrain_chunks = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        writer.write_str("terrain_chunk_unit_v1");
        writer.write_u32(TERRAIN_RECIPE_VERSION);
        writer.write_i32(chunk.x);
        writer.write_i32(chunk.y);
        partitions.terrain_chunk.write_canonical(&mut writer);
        let start_x = chunk.x * 4;
        let start_y = chunk.y * 4;
        for y in (start_y - 1)..=(start_y + 4) {
            for x in (start_x - 1)..=(start_x + 4) {
                writer.write_i32(x);
                writer.write_i32(y);
                match projections.landscapes.get(&(x, y)) {
                    Some(cell) => {
                        writer.write_bool(true);
                        write_landscape_projection(&mut writer, cell, true);
                    }
                    None => writer.write_bool(false),
                }
            }
        }
        terrain_chunks.push((chunk, writer.finish_and_clear()));
    }

    state.units.terrain_cell = FingerprintTable::from_entries(terrain_cells);
    state.units.terrain_chunk = FingerprintTable::from_entries(terrain_chunks);
    state.reverse.texture_to_terrain_cell = finish_reverse_index(texture_to_cells);
    state.terrain_global = fingerprint_terrain_global(
        settings,
        layout,
        gate_inputs.expect("enabled terrain state requires prepared gate inputs"),
    );
    state.terrain_enabled = true;
}

/// Builds one unit per merge-eligible exterior cell.
///
/// Merged geometry is trimmed against the LAND heightmap (see `statics::SubterrainCull`), so these
/// units carry terrain inputs even though the products are statics: the cull margin via
/// `partitions.merge`, and the height hash of each cell in the 3x3 block around the unit's own.
///
/// The halo bounds how far a member's geometry may overhang its own cell before an edit to the
/// terrain under that overhang stops invalidating this unit. One full cell (8192 units) of reach
/// past the owning cell covers every placement the merge admits in practice.
fn build_merge_units(
    usage: &UsageInfo<'_>,
    distant_statics: &DistantStatics,
    mesh_fingerprints: &HashMap<String, [u8; 32]>,
    partitions: &UnitSettingsPartitions,
    projections: &Projections,
) -> (FingerprintTable<MergeUnitKey>, crate::generation::state_db::MergeReverseIndex) {
    let mut cells: BTreeMap<(i32, i32), Vec<_>> = BTreeMap::new();
    if let Some(references) = usage.exterior_references() {
        for (&key, reference) in references {
            if reference.vis_index != 0 {
                continue;
            }
            let Some(distant_static) = distant_statics.get(reference.id.as_ref()) else {
                continue;
            };
            if distant_static.static_type == StaticType::StaticGrass
                || distant_static.bounding_sphere.radius * reference.scale < 32.0
            {
                continue;
            }
            cells
                .entry(reference.cell_coords())
                .or_default()
                .push((key, reference, distant_static));
        }
    }
    cells.retain(|_, members| members.len() > 1);

    let mut mesh_to_merge: BTreeMap<&str, BTreeSet<MergeUnitKey>> = BTreeMap::new();
    let mut writer = CanonicalWriter::new();
    let mut entries = Vec::with_capacity(cells.len());
    for ((x, y), mut members) in cells {
        members.sort_unstable_by_key(|&(key, ..)| (reference_source_label(usage, key), key.index()));
        let cell_key = MergeUnitKey::new(x, y);
        writer.write_str("merge_unit_v1");
        writer.write_u32(STATICS_RECIPE_VERSION);
        cell_key.write_canonical(&mut writer);
        partitions.merge.write_canonical(&mut writer);
        // Heights the below-terrain cull may sample while building this cell's groups. Offsets are
        // written alongside the hashes so a shifted or absent neighbour cannot alias another block.
        for dy in -1..=1 {
            for dx in -1..=1 {
                writer.write_i32(dx);
                writer.write_i32(dy);
                write_hash_option(
                    &mut writer,
                    projections
                        .landscapes
                        .get(&(x + dx, y + dy))
                        .and_then(|landscape| landscape.height_hash),
                );
            }
        }
        writer.write_u64(members.len() as u64);
        for (key, reference, distant_static) in members {
            writer.write_str(reference_source_label(usage, key));
            writer.write_u32(key.index());
            writer.write_str(reference.id.as_ref());
            write_vec3(&mut writer, reference.translation);
            write_vec3(&mut writer, reference.rotation);
            writer.write_f32(reference.scale);
            writer.write_u16(reference.vis_index);
            writer.write_u8(distant_static.static_type as u8);
            write_vec3(&mut writer, distant_static.bounding_sphere.center);
            writer.write_f32(distant_static.bounding_sphere.radius);
            write_vec3(&mut writer, distant_static.bounding_box.min);
            write_vec3(&mut writer, distant_static.bounding_box.max);
            match mesh_fingerprints.get(reference.id.as_ref()) {
                Some(fingerprint) => writer.write_bytes(fingerprint),
                None => writer.write_bytes(&[0; 32]),
            }
            mesh_to_merge.entry(reference.id.as_ref()).or_default().insert(cell_key);
        }
        entries.push((cell_key, writer.finish_and_clear()));
    }
    (FingerprintTable::from_entries(entries), finish_reverse_index(mesh_to_merge))
}

fn build_texture_units(
    textures: &AtlasTextureSet<IndexSet<String>>,
    source_info: &HashMap<String, SourceTextureInfo>,
    sizing_plan: &SizingPlan,
    partitions: &UnitSettingsPartitions,
) -> FingerprintTable<String> {
    let keys = textures
        .opaque
        .iter()
        .chain(&textures.alpha)
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut writer = CanonicalWriter::new();
    let mut entries = Vec::with_capacity(keys.len());
    for key in keys {
        let opaque = textures.opaque.contains(&key);
        let alpha = textures.alpha.contains(&key);
        writer.write_str("texture_unit_v1");
        writer.write_u32(STATICS_RECIPE_VERSION);
        writer.write_str(&key);
        partitions.texture.write_canonical(&mut writer);
        writer.write_bool(opaque);
        writer.write_bool(alpha);
        writer.write_u32(sizing_plan.dim_for(&key, AtlasDomain::Opaque));
        writer.write_u32(sizing_plan.dim_for(&key, AtlasDomain::Alpha));
        match source_info.get(&key) {
            Some(source) => {
                writer.write_bool(true);
                writer.write_u64(source.size);
                writer.write_bytes(&source.hash);
                writer.write_u32(source.width);
                writer.write_u32(source.height);
                writer.write_u32(source.mip_count);
                writer.write_bool(source.is_dds);
            }
            None => writer.write_bool(false),
        }
        entries.push((key, writer.finish_and_clear()));
    }
    FingerprintTable::from_entries(entries)
}

fn fingerprint_statics_global(projections: &Projections, usage: &UsageInfo<'_>) -> [u8; 32] {
    let mut writer = CanonicalWriter::new();
    writer.write_str("statics_global_v3");
    writer.write_u64(projections.interiors.len() as u64);
    for (key, value) in &projections.interiors {
        writer.write_str(key);
        value.write_canonical(&mut writer);
    }
    writer.write_u64(projections.objects.len() as u64);
    for (key, value) in &projections.objects {
        writer.write_str(key);
        value.write_canonical(&mut writer);
    }
    writer.write_u64(projections.overrides.names.len() as u64);
    for (key, value) in &projections.overrides.names {
        writer.write_str(key);
        writer.write_bool(*value);
    }
    writer.write_u64(projections.overrides.interiors.len() as u64);
    for (key, value) in &projections.overrides.interiors {
        writer.write_str(key);
        writer.write_bool(*value);
    }
    projections.overrides.dynamic_visibility.write_canonical(&mut writer);
    projections.settings.write_canonical(&mut writer);
    writer.write_u64(projections.door_meshes.len() as u64);
    for mesh in &projections.door_meshes {
        writer.write_str(mesh);
    }
    writer.write_u64(projections.grass_placements.len() as u64);
    for placement in &projections.grass_placements {
        placement.write_canonical(&mut writer);
    }
    if let Some(references) = usage.exterior_references() {
        writer.write_u64(references.len() as u64);
        for (key, reference) in references
            .iter()
            .sorted_unstable_by_key(|&(key, _)| (reference_source_label(usage, *key), key.index()))
        {
            writer.write_str(reference_source_label(usage, *key));
            writer.write_u32(key.index());
            writer.write_str(reference.id.as_ref());
            write_vec3(&mut writer, reference.translation);
            write_vec3(&mut writer, reference.rotation);
            writer.write_f32(reference.scale);
            writer.write_u16(reference.vis_index);
        }
    } else {
        writer.write_u64(0);
    }
    writer.finish()
}

/// Collapses the statics unit model and atlas lowering outcome into the coarse bundle gate.
pub(crate) fn statics_domain_digest(state: &GenerationState, atlas_binding_digest: &[u8; 32]) -> [u8; 32] {
    statics_domain_digest_with_serializer(state, atlas_binding_digest, STATICS_RECIPE_VERSION)
}

fn statics_domain_digest_with_serializer(
    state: &GenerationState,
    atlas_binding_digest: &[u8; 32],
    serializer_version: u32,
) -> [u8; 32] {
    let mut writer = CanonicalWriter::new();
    writer.write_str("statics_domain_v2");
    writer.write_bytes(STATIC_BUNDLE_INPUT_FINGERPRINT_MAGIC);
    writer.write_u32(serializer_version);
    write_path_fingerprint_table(&mut writer, &state.units.mesh);
    write_coordinate_fingerprint_table(&mut writer, &state.units.merge);
    writer.write_bytes(&state.statics_global);
    // Fold in the record-production recipe so a behavior/recipe bump busts whole-bundle carry the
    // same way it busts owner-partial reuse (which compares the record-global digest directly).
    writer.write_bytes(&state.statics_record_global_digest);
    writer.write_bytes(atlas_binding_digest);
    writer.finish()
}

/// Digest over the static record-production *recipe*: the versioned behavior and layout that decide
/// packed record bytes and their shard placement for otherwise-identical logical inputs.
///
/// Folded into [`statics_domain_digest`] (so a recipe bump busts whole-bundle carry) and compared
/// directly by the owner-partial planner (so a recipe bump forces a full rebuild). It deliberately
/// excludes unit tables, `statics_global`, and the atlas binding digest, which gate through the
/// typed diff and binding closure rather than the recipe. Folding those in would trip
/// `record_recipe_changed` on every ordinary edit and disable partial reuse.
pub(crate) fn statics_record_global_digest() -> [u8; 32] {
    statics_record_global_digest_with(STATICS_RECIPE_VERSION)
}

fn statics_record_global_digest_with(recipe_version: u32) -> [u8; 32] {
    let mut writer = CanonicalWriter::new();
    writer.write_str("statics_record_global_v1");
    writer.write_u32(recipe_version);
    writer.write_bytes(STATIC_SHARD_ASSIGNMENT_MAGIC);
    writer.write_bytes(STATIC_SHARD_INPUT_FINGERPRINT_MAGIC);
    writer.write_u32(STATIC_MESH_SHARD_COUNT as u32);
    writer.finish()
}

/// Writes a path-keyed fingerprint table into a domain digest (legacy length-prefixed key text).
fn write_path_fingerprint_table(writer: &mut CanonicalWriter, table: &FingerprintTable<String>) {
    writer.write_u64(table.entries.len() as u64);
    for (key, fingerprint) in &table.entries {
        writer.write_str(key);
        writer.write_bytes(fingerprint);
    }
}

const MAX_COORDINATE_TEXT_LEN: usize = "-2147483648,-2147483648".len();

/// Writes a coordinate-keyed fingerprint table using the legacy `"{x},{y}"` digest form.
fn write_coordinate_fingerprint_table<K: std::fmt::Display>(writer: &mut CanonicalWriter, table: &FingerprintTable<K>) {
    writer.write_u64(table.entries.len() as u64);
    let mut key_text = [0; MAX_COORDINATE_TEXT_LEN];
    for (key, fingerprint) in &table.entries {
        let mut cursor = std::io::Cursor::new(key_text.as_mut_slice());
        write!(&mut cursor, "{key}").expect("coordinate key text exceeds the maximum i32 pair length");
        let key_len = cursor.position() as usize;
        writer.write_bytes(&key_text[..key_len]);
        writer.write_bytes(fingerprint);
    }
}

/// Maps terrain fingerprint inputs into the unit model: per-cell LAND and referenced
/// texture hashes live in `terrain_cell`; halo geometry lives in `terrain_chunk`; layout,
/// versions, atlas/control facts, simplifier configuration, ordered paths, and the default
/// texture identity live here.
fn fingerprint_terrain_global(
    settings: &GenerationSettings,
    layout: &TerrainAtlasLayout,
    inputs: &TerrainGateInputs,
) -> [u8; 32] {
    let mut writer = CanonicalWriter::new();
    writer.write_str("terrain_global_v2");
    writer.write_i32(layout.span_x);
    writer.write_i32(layout.span_y);
    writer.write_u64(layout.regions.len() as u64);
    for region in &layout.regions {
        writer.write_i32(region.min_x);
        writer.write_i32(region.max_x);
        writer.write_i32(region.min_y);
        writer.write_i32(region.max_y);
        writer.write_i32(region.offset_x);
        writer.write_i32(region.offset_y);
    }
    writer.write_u32(settings.max_terrain_texture_size);
    writer.write_u32(settings.max_terrain_atlas_size);
    writer.write_u32(settings.max_terrain_control_texture_size);
    writer.write_u64(settings.max_terrain_control_texture_bytes);
    writer.write_str(&settings.terrain_detail.to_string());
    writer.write_u8(crate::generation::job::texture_dedupe_mode_tag(settings.texture_dedupe_mode));
    writer.write_u32(inputs.recipe_version);
    writer.write_u32(inputs.terrain_file_version);
    writer.write_f32(inputs.land_cell_size);
    writer.write_f32(inputs.default_patch_size);
    writer.write_u32(inputs.pattern_tile_size);
    writer.write_u32(inputs.pattern_gutter_size);
    for value in inputs.origin_cell {
        writer.write_i32(value);
    }
    for value in inputs.cell_size_xy {
        writer.write_u32(value);
    }
    for value in inputs.material_size_xy {
        writer.write_u32(value);
    }
    for value in inputs.atlas_spec {
        writer.write_u32(value);
    }
    writer.write_u64(inputs.mesh_chunk_cells_per_side);
    writer.write_f32(inputs.mesh_target_error);
    for value in inputs.mesh_weights {
        writer.write_f32(value);
    }
    writer.write_u32(inputs.mesh_option_bits);
    writer.write_u64(inputs.ordered_texture_paths.len() as u64);
    for path in &inputs.ordered_texture_paths {
        writer.write_str(path);
    }
    writer.write_str(&inputs.default_texture_key);
    writer.write_bytes(&inputs.default_texture_hash);
    writer.finish()
}

/// Digests the exact union of inputs consumed by the five height-independent terrain DDS products.
///
/// The source atlas, material map, material-flags map, patch albedo, and blend-pattern atlas are
/// built from terrain textures, per-cell material/VTEX data, the control region, the atlas recipe,
/// and their rasterizer versions. None of them reads LAND height, normal, or vertex-color data, so
/// those are deliberately excluded and a height-only edit leaves this digest unchanged.
pub(crate) fn terrain_dds_domain_digest(
    settings: &GenerationSettings,
    projections: &Projections,
    identities: &ContentIdentityCollector,
    layout: &TerrainAtlasLayout,
    inputs: &TerrainGateInputs,
) -> [u8; 32] {
    let mut writer = CanonicalWriter::new();
    writer.write_str("terrain_dds_domain_v1");
    writer.write_bytes(TERRAIN_PACKAGE_INPUT_FINGERPRINT_MAGIC);
    writer.write_u32(TERRAIN_RECIPE_VERSION);

    writer.write_i32(layout.span_x);
    writer.write_i32(layout.span_y);
    writer.write_u64(layout.regions.len() as u64);
    for region in &layout.regions {
        writer.write_i32(region.min_x);
        writer.write_i32(region.max_x);
        writer.write_i32(region.min_y);
        writer.write_i32(region.max_y);
        writer.write_i32(region.offset_x);
        writer.write_i32(region.offset_y);
    }

    writer.write_u32(settings.max_terrain_texture_size);
    writer.write_u32(settings.max_terrain_atlas_size);
    writer.write_u32(settings.max_terrain_control_texture_size);
    writer.write_u64(settings.max_terrain_control_texture_bytes);
    writer.write_str(&settings.terrain_detail.to_string());
    writer.write_u8(crate::generation::job::texture_dedupe_mode_tag(settings.texture_dedupe_mode));

    writer.write_u32(inputs.recipe_version);
    writer.write_f32(inputs.land_cell_size);
    writer.write_f32(inputs.default_patch_size);
    writer.write_u32(inputs.pattern_tile_size);
    writer.write_u32(inputs.pattern_gutter_size);
    for value in inputs.origin_cell {
        writer.write_i32(value);
    }
    for value in inputs.cell_size_xy {
        writer.write_u32(value);
    }
    for value in inputs.material_size_xy {
        writer.write_u32(value);
    }
    for value in inputs.atlas_spec {
        writer.write_u32(value);
    }
    writer.write_u64(inputs.ordered_texture_paths.len() as u64);
    for path in &inputs.ordered_texture_paths {
        writer.write_str(path);
    }
    writer.write_str(&inputs.default_texture_key);
    writer.write_bytes(&inputs.default_texture_hash);

    let partitions = UnitSettingsPartitions::from_settings(settings);
    partitions.terrain_cell.write_canonical(&mut writer);
    writer.write_u64(projections.landscapes.len() as u64);
    for (&(x, y), cell) in &projections.landscapes {
        writer.write_i32(x);
        writer.write_i32(y);
        write_hash_option(&mut writer, cell.material_hash);
        writer.write_u64(cell.referenced_textures.len() as u64);
        for texture in &cell.referenced_textures {
            writer.write_str(texture);
            match identities.terrain_textures().get(texture) {
                Some(hash) => {
                    writer.write_bool(true);
                    writer.write_bytes(&hash.0);
                }
                None => writer.write_bool(false),
            }
        }
    }
    writer.finish()
}

/// Collapses terrain cell/chunk state and global preparation facts into the package gate.
///
/// This also gates the occlusion carry, so the occlusion file's own recipe constants are hashed
/// here: every other occlusion input (per-cell heights, and the control region's origin and
/// extent) already reaches the digest through the terrain-cell table and `terrain_global`.
pub(crate) fn terrain_domain_digest(state: &GenerationState) -> [u8; 32] {
    let mut writer = CanonicalWriter::new();
    writer.write_str("terrain_domain_v2");
    writer.write_bytes(TERRAIN_PACKAGE_INPUT_FINGERPRINT_MAGIC);
    writer.write_u32(crate::mge_xe::terrain_occlusion::TERRAIN_OCCLUSION_VERSION);
    writer.write_f32(crate::mge_xe::terrain_occlusion::TERRAIN_OCCLUSION_GRID_SPACING);
    write_coordinate_fingerprint_table(&mut writer, &state.units.terrain_cell);
    write_coordinate_fingerprint_table(&mut writer, &state.units.terrain_chunk);
    writer.write_bytes(&state.terrain_global);
    writer.finish()
}

fn fingerprint_settings(settings: &GenerationSettings) -> [u8; 32] {
    let mut writer = CanonicalWriter::new();
    writer.write_str("generation_settings_identity_v1");
    writer.write_u32(1);
    write_generation_settings_canonical(&mut writer, settings);
    writer.finish()
}

fn write_mesh_resolution(
    writer: &mut CanonicalWriter,
    resolution: Option<&crate::generation::identity::MeshResolutionFact>,
) {
    match resolution {
        Some(resolution) => {
            writer.write_bool(true);
            writer.write_u8(match resolution.rule {
                MeshResolutionRule::Original => 0,
                MeshResolutionRule::Dist => 1,
                MeshResolutionRule::XWithKf => 2,
            });
            writer.write_str(&resolution.resolved_key);
        }
        None => writer.write_bool(false),
    }
}

fn write_content_identity(writer: &mut CanonicalWriter, identity: Option<&crate::generation::identity::ContentIdentity>) {
    match identity {
        Some(identity) => {
            writer.write_bool(true);
            writer.write_u64(identity.byte_len);
            writer.write_bytes(&identity.hash.0);
        }
        None => writer.write_bool(false),
    }
}

/// Distinguishes rejected, read-failed, and unresolved mesh inputs in the unit fingerprint.
fn absent_mesh_status(has_resolution: bool, has_content: bool) -> u8 {
    match (has_resolution, has_content) {
        (true, true) => 2,
        (true, false) => 3,
        (false, _) => 4,
    }
}

fn write_option_f32(writer: &mut CanonicalWriter, value: Option<f32>) {
    match value {
        Some(value) => {
            writer.write_bool(true);
            writer.write_f32(value);
        }
        None => writer.write_bool(false),
    }
}

fn write_landscape_projection(writer: &mut CanonicalWriter, projection: &LandscapeCellProjection, include_material: bool) {
    write_hash_option(writer, projection.height_hash);
    write_hash_option(writer, projection.terrain_hash);
    if include_material {
        write_hash_option(writer, projection.material_hash);
    }
}

/// Resolves the source label for a reference key, in both sort order and written bytes.
///
/// Synthetic references carry no plugin filename. The `<synthetic>` stand-in is fed to the
/// comparators *and* to the fingerprint stream, so the two can never be updated independently.
fn reference_source_label<'usage>(usage: &'usage UsageInfo<'_>, key: StableRefKey) -> &'usage str {
    usage.reference_source_name(key.source()).unwrap_or("<synthetic>")
}

fn write_vec3(writer: &mut CanonicalWriter, value: glam::Vec3) {
    writer.write_f32(value.x);
    writer.write_f32(value.y);
    writer.write_f32(value.z);
}

/// Finalizes a reverse index: own each outer path once; `BTreeSet` iteration already yields `Ord` order.
fn finish_reverse_index<V: Ord>(index: BTreeMap<&str, BTreeSet<V>>) -> Vec<(String, Vec<V>)> {
    index
        .into_iter()
        .map(|(key, values)| (key.to_owned(), values.into_iter().collect()))
        .collect()
}

#[cfg(test)]
mod tests;
