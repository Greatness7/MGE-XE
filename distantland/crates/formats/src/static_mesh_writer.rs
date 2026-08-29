//! Serialization of one MGE-XE `static_meshes` v6 POD container.

use anyhow::{anyhow, bail};
use hashbrown::HashMap;

use crate::PackedDistantStatics;
use crate::distant_statics::{
    COMPONENT_RECORD_SIZE, ComponentRecord, GRASS_VERTEX_STRIDE, HEADER_SIZE, INDEX_ELEMENT_SIZE, PALETTE_RECORD_SIZE,
    STATIC_MESHES_MAGIC, STATIC_MESHES_VERSION, STATIC_RECORD_SIZE, STATIC_VERTEX_STRIDE, SUBSET_RECORD_SIZE,
    StaticMeshesFileHeader, StaticRecord, StaticType, SubsetRecord, UV_BOUND_PALETTE_CAP,
};

/// Returns the geometry vertex stride for a static's classification.
///
/// Both layouts are 20 bytes today, but the two constants stay separate because the C++ loader
/// declares them separately and `position.w` means different things in each. Used by every
/// serializer pass so the size, offset, and final byte-write computations stay consistent.
fn vertex_stride_for(static_type: StaticType) -> u64 {
    match static_type {
        StaticType::StaticGrass => GRASS_VERTEX_STRIDE as u64,
        _ => STATIC_VERTEX_STRIDE as u64,
    }
}

fn align_up(value: u64, alignment: u64) -> anyhow::Result<u64> {
    debug_assert!(alignment.is_power_of_two());
    value
        .checked_add(alignment - 1)
        .map(|v| v & !(alignment - 1))
        .ok_or_else(|| anyhow!("section offset overflow"))
}

fn pad_to(buf: &mut Vec<u8>, target: usize) {
    let current = buf.len();
    if current < target {
        buf.resize(target, 0);
    }
}

fn valid_component_classification(classification: u8) -> bool {
    matches!(classification, 0 | 1 | 2 | 3 | 5 | 6)
}

/// Validates that one subset's wire-format component records tile its triangle buffer.
///
/// This is the packed-side counterpart to the statics model's pre-packing partition check. It
/// additionally gates the two properties that only become expressible once packed: a
/// classification byte inside the valid range and zeroed reserved bytes.
///
/// An empty component list means the subset carries no provenance and renders at all tiers, so the
/// tiling rule does not apply to it.
///
/// # Errors
///
/// Returns an error identifying the first violated invariant.
fn validate_component_records(components: &[ComponentRecord], triangle_count: u32) -> anyhow::Result<()> {
    if components.is_empty() {
        return Ok(());
    }

    let mut expected_first_triangle = 0u32;
    for component in components {
        if component.triangle_count == 0 {
            bail!("component has zero triangles");
        }
        if component.first_triangle != expected_first_triangle {
            bail!("component ranges do not tile subset triangles");
        }
        expected_first_triangle = component
            .first_triangle
            .checked_add(component.triangle_count)
            .ok_or_else(|| anyhow!("component triangle range overflow"))?;
        if expected_first_triangle > triangle_count {
            bail!("component range exceeds subset triangle count");
        }
        if !component.radius.is_finite() || component.radius < 0.0 {
            bail!("component radius must be finite and non-negative");
        }
        if !valid_component_classification(component.classification) {
            bail!("component classification is invalid for merged static provenance");
        }
        if component.reserved != [0; 3] {
            bail!("component reserved bytes must be zero");
        }
    }
    if expected_first_triangle != triangle_count {
        bail!("component ranges do not cover all subset triangles");
    }
    Ok(())
}

/// Serializes distant statics into the v6 `static_meshes` POD binary format.
///
/// Uses a two-pass approach: first counts everything and validates sizes, then
/// writes header, tables, component records, palette records, texture paths, and geometry
/// blocks in order.
///
/// # Errors
///
/// Returns an error if any count or size exceeds the serialized field widths, or if a subset's
/// palette exceeds [`UV_BOUND_PALETTE_CAP`] — the shader's palette array is fixed at that size,
/// so an over-cap subset would index past it.
pub fn serialize_static_meshes(distant_statics: &PackedDistantStatics) -> anyhow::Result<Vec<u8>> {
    // --- PASS 1: count and validate ---
    let static_count: u32 = distant_statics
        .len()
        .try_into()
        .map_err(|_| anyhow!("static count exceeds u32"))?;
    let mut texture_blob_size: u64 = 0;
    let mut geometry_blob_size: u64 = 0;
    let mut component_count: u32 = 0;
    let mut palette_count: u32 = 0;
    let mut texture_paths: HashMap<&str, (u64, u32)> = HashMap::new();
    let mut texture_path_order: Vec<&str> = Vec::new();

    for ds in distant_statics.values() {
        for subset in &ds.subsets {
            let texture_path = subset.texture.as_ref();
            if texture_path.is_empty() {
                bail!("subset has empty texture path");
            }
            let texture_len: u32 = texture_path
                .len()
                .try_into()
                .map_err(|_| anyhow!("texture path length exceeds u32"))?;

            if !texture_paths.contains_key(texture_path) {
                let texture_offset = texture_blob_size;
                texture_blob_size = texture_blob_size
                    .checked_add(u64::from(texture_len) + 1) // +1 for NUL
                    .ok_or_else(|| anyhow!("texture blob size overflow"))?;
                texture_paths.insert(texture_path, (texture_offset, texture_len));
                texture_path_order.push(texture_path);
            }

            let vertex_count: u32 = subset
                .vertices
                .len()
                .try_into()
                .map_err(|_| anyhow!("subset vertex count exceeds u32"))?;

            if vertex_count > u16::MAX as u32 {
                bail!("subset vertex count exceeds 65535, incompatible with 16-bit index buffer",);
            }
            let triangle_count: u32 = subset
                .triangles
                .len()
                .try_into()
                .map_err(|_| anyhow!("subset triangle count exceeds u32"))?;
            let subset_component_count: u32 = subset
                .components
                .len()
                .try_into()
                .map_err(|_| anyhow!("subset component count exceeds u32"))?;
            component_count = component_count
                .checked_add(subset_component_count)
                .ok_or_else(|| anyhow!("component count exceeds u32"))?;

            validate_component_records(&subset.components, triangle_count)?;

            let subset_palette_count: u32 = subset
                .palette
                .len()
                .try_into()
                .map_err(|_| anyhow!("subset palette count exceeds u32"))?;
            if subset_palette_count > UV_BOUND_PALETTE_CAP {
                bail!(
                    "subset UV-bound palette has {subset_palette_count} entries, exceeding the cap of {UV_BOUND_PALETTE_CAP}"
                );
            }
            palette_count = palette_count
                .checked_add(subset_palette_count)
                .ok_or_else(|| anyhow!("palette count exceeds u32"))?;

            let vertex_bytes = u64::from(vertex_count) * vertex_stride_for(ds.static_type);
            let index_bytes = u64::from(triangle_count) * 3 * INDEX_ELEMENT_SIZE as u64;

            geometry_blob_size = geometry_blob_size
                .checked_add(vertex_bytes)
                .and_then(|v| v.checked_add(index_bytes))
                .ok_or_else(|| anyhow!("geometry blob size overflow"))?;
        }
    }

    let actual_subset_count: u32 = distant_statics
        .values()
        .map(|ds| ds.subsets.len())
        .sum::<usize>()
        .try_into()
        .map_err(|_| anyhow!("subset count exceeds u32"))?;

    let static_table_offset = HEADER_SIZE as u64;
    let static_table_size = u64::from(static_count)
        .checked_mul(STATIC_RECORD_SIZE as u64)
        .ok_or_else(|| anyhow!("static table size overflow"))?;
    let subset_table_offset = align_up(
        static_table_offset
            .checked_add(static_table_size)
            .ok_or_else(|| anyhow!("subset table offset overflow"))?,
        8,
    )?;
    let subset_table_size = u64::from(actual_subset_count)
        .checked_mul(SUBSET_RECORD_SIZE as u64)
        .ok_or_else(|| anyhow!("subset table size overflow"))?;
    let component_table_offset = align_up(
        subset_table_offset
            .checked_add(subset_table_size)
            .ok_or_else(|| anyhow!("component table offset overflow"))?,
        8,
    )?;
    let component_table_size = u64::from(component_count)
        .checked_mul(COMPONENT_RECORD_SIZE as u64)
        .ok_or_else(|| anyhow!("component table size overflow"))?;
    let palette_table_offset = align_up(
        component_table_offset
            .checked_add(component_table_size)
            .ok_or_else(|| anyhow!("palette table offset overflow"))?,
        8,
    )?;
    let palette_table_size = u64::from(palette_count)
        .checked_mul(PALETTE_RECORD_SIZE as u64)
        .ok_or_else(|| anyhow!("palette table size overflow"))?;
    let texture_blob_offset = palette_table_offset
        .checked_add(palette_table_size)
        .ok_or_else(|| anyhow!("texture blob offset overflow"))?;
    let geometry_blob_offset = align_up(
        texture_blob_offset
            .checked_add(texture_blob_size)
            .ok_or_else(|| anyhow!("geometry blob offset overflow"))?,
        8,
    )?;
    let total_size = geometry_blob_offset
        .checked_add(geometry_blob_size)
        .ok_or_else(|| anyhow!("total file size overflow"))?;
    let subset_table_offset_usize: usize = subset_table_offset
        .try_into()
        .map_err(|_| anyhow!("subset table offset exceeds usize"))?;
    let component_table_offset_usize: usize = component_table_offset
        .try_into()
        .map_err(|_| anyhow!("component table offset exceeds usize"))?;
    let palette_table_offset_usize: usize = palette_table_offset
        .try_into()
        .map_err(|_| anyhow!("palette table offset exceeds usize"))?;
    let texture_blob_offset_usize: usize = texture_blob_offset
        .try_into()
        .map_err(|_| anyhow!("texture blob offset exceeds usize"))?;
    let geometry_blob_offset_usize: usize = geometry_blob_offset
        .try_into()
        .map_err(|_| anyhow!("geometry blob offset exceeds usize"))?;
    let total_size_usize: usize = total_size.try_into().map_err(|_| anyhow!("total file size exceeds usize"))?;

    // --- PASS 2: write ---
    let mut buf = Vec::with_capacity(total_size_usize);

    // Header
    let header = StaticMeshesFileHeader {
        magic: *STATIC_MESHES_MAGIC,
        version: STATIC_MESHES_VERSION,
        header_size: HEADER_SIZE as u32,
        static_record_size: STATIC_RECORD_SIZE as u32,
        subset_record_size: SUBSET_RECORD_SIZE as u32,
        vertex_stride: STATIC_VERTEX_STRIDE as u32,
        index_element_size: INDEX_ELEMENT_SIZE as u32,
        static_count,
        subset_count: actual_subset_count,
        static_table_offset,
        static_table_size,
        subset_table_offset,
        subset_table_size,
        texture_blob_offset,
        texture_blob_size,
        geometry_blob_offset,
        geometry_blob_size,
        grass_vertex_stride: GRASS_VERTEX_STRIDE as u32,
        reserved: 0,
        component_table_offset,
        component_table_size,
        component_record_size: COMPONENT_RECORD_SIZE as u32,
        component_count,
        palette_table_offset,
        palette_table_size,
        palette_record_size: PALETTE_RECORD_SIZE as u32,
        palette_count,
    };
    buf.extend_from_slice(bytemuck::bytes_of(&header));

    // Static table
    let mut first_subset: u32 = 0;
    for ds in distant_statics.values() {
        let sc: u32 = ds.subsets.len().try_into().map_err(|_| anyhow!("subset count exceeds u32"))?;
        let rec = StaticRecord {
            static_type: ds.static_type as u32,
            sphere: ds.bounding_sphere,
            aabb: ds.bounding_box,
            first_subset_index: first_subset,
            subset_count: sc,
        };
        buf.extend_from_slice(bytemuck::bytes_of(&rec));
        first_subset = first_subset
            .checked_add(sc)
            .ok_or_else(|| anyhow!("static subset range overflow"))?;
    }

    // Padding to subset table
    pad_to(&mut buf, subset_table_offset_usize);

    // Subset table (with pre-computed offsets)
    let mut geom_cursor: u64 = 0;
    let mut first_component: u32 = 0;
    let mut first_palette: u32 = 0;
    for ds in distant_statics.values() {
        for subset in &ds.subsets {
            let (texture_relative_offset, texture_path_length) = *texture_paths
                .get(subset.texture.as_ref())
                .expect("validated texture path must have an offset");
            let vertex_count: u32 = subset
                .vertices
                .len()
                .try_into()
                .map_err(|_| anyhow!("subset vertex count exceeds u32"))?;
            let triangle_count: u32 = subset
                .triangles
                .len()
                .try_into()
                .map_err(|_| anyhow!("subset triangle count exceeds u32"))?;
            let vertex_bytes = u64::from(vertex_count) * vertex_stride_for(ds.static_type);
            let index_bytes = u64::from(triangle_count) * 3 * INDEX_ELEMENT_SIZE as u64;
            let texture_path_offset = texture_blob_offset
                .checked_add(texture_relative_offset)
                .ok_or_else(|| anyhow!("texture path offset overflow"))?;
            let vertex_offset = geometry_blob_offset
                .checked_add(geom_cursor)
                .ok_or_else(|| anyhow!("vertex offset overflow"))?;
            let index_offset = vertex_offset
                .checked_add(vertex_bytes)
                .ok_or_else(|| anyhow!("index offset overflow"))?;
            let subset_component_count: u32 = subset
                .components
                .len()
                .try_into()
                .map_err(|_| anyhow!("subset component count exceeds u32"))?;
            let subset_palette_count: u32 = subset
                .palette
                .len()
                .try_into()
                .map_err(|_| anyhow!("subset palette count exceeds u32"))?;

            let rec = SubsetRecord {
                sphere: subset.bounding_sphere,
                aabb: subset.bounding_box,
                texture_path_offset,
                vertex_offset,
                index_offset,
                vertex_count,
                triangle_count,
                flags: u32::from(subset.has_alpha != 0) | (u32::from(subset.has_uv_controller != 0) << 1),
                texture_path_length,
                horizon_footprint: subset.horizon_footprint,
                first_component_index: first_component,
                component_count: subset_component_count,
                first_palette_index: first_palette,
                palette_count: subset_palette_count,
            };
            buf.extend_from_slice(bytemuck::bytes_of(&rec));
            first_component = first_component
                .checked_add(subset_component_count)
                .ok_or_else(|| anyhow!("component range overflow"))?;
            first_palette = first_palette
                .checked_add(subset_palette_count)
                .ok_or_else(|| anyhow!("palette range overflow"))?;

            geom_cursor = geom_cursor
                .checked_add(vertex_bytes)
                .and_then(|offset| offset.checked_add(index_bytes))
                .ok_or_else(|| anyhow!("geometry blob cursor overflow"))?;
        }
    }

    // Padding to component table.
    pad_to(&mut buf, component_table_offset_usize);

    // Component table.
    let component_table_start = buf.len();
    for ds in distant_statics.values() {
        for subset in &ds.subsets {
            buf.extend_from_slice(bytemuck::cast_slice(&subset.components));
        }
    }
    debug_assert_eq!(
        (buf.len() - component_table_start) as u64,
        component_table_size,
        "component table size mismatch"
    );

    // Padding to palette table.
    pad_to(&mut buf, palette_table_offset_usize);

    // Palette table.
    let palette_table_start = buf.len();
    for ds in distant_statics.values() {
        for subset in &ds.subsets {
            buf.extend_from_slice(bytemuck::cast_slice(&subset.palette));
        }
    }
    debug_assert_eq!(
        (buf.len() - palette_table_start) as u64,
        palette_table_size,
        "palette table size mismatch"
    );

    // Padding to texture blob.
    pad_to(&mut buf, texture_blob_offset_usize);

    // Texture paths
    let texture_blob_start = buf.len();
    for texture_path in texture_path_order {
        buf.extend_from_slice(texture_path.as_bytes());
        buf.push(0); // NUL terminator
    }
    debug_assert_eq!(
        (buf.len() - texture_blob_start) as u64,
        texture_blob_size,
        "texture blob size mismatch"
    );

    // Padding to geometry blob
    pad_to(&mut buf, geometry_blob_offset_usize);

    // Geometry: vertices then indices per subset. Regular statics serialize as a bulk cast;
    // grass projects each vertex onto the `PackedGrassVertex` layout.
    for ds in distant_statics.values() {
        let is_grass = ds.static_type == StaticType::StaticGrass;
        for subset in &ds.subsets {
            if is_grass {
                for vertex in &subset.vertices {
                    buf.extend_from_slice(bytemuck::bytes_of(vertex.as_grass()));
                }
            } else {
                buf.extend_from_slice(bytemuck::cast_slice(&subset.vertices));
            }
            buf.extend_from_slice(bytemuck::cast_slice(&subset.triangles));
        }
    }

    debug_assert_eq!(geom_cursor, geometry_blob_size, "geometry blob size mismatch");
    debug_assert_eq!(buf.len(), total_size_usize, "serialized size mismatch");
    Ok(buf)
}

#[cfg(test)]
mod tests;
