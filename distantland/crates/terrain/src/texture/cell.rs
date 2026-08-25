use super::*;

/// Result of sampling a render vertex's terrain texture, including cross-cell lookups.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampledTerrainTexture<'a> {
    /// Cell that ultimately supplied the texture.
    pub cell: (i32, i32),
    /// Patch coordinates inside the sampled cell.
    pub patch: (usize, usize),
    /// Raw VTEX index stored in the landscape patch.
    pub raw_vtex_index: u16,
    /// Resolved normalized texture key.
    pub path: &'a str,
}

/// Wraps a patch coordinate that has gone out of [0,16) into the adjacent cell.
pub fn mod_cell(cell_coord: &mut i32, patch_coord: &mut i32) {
    if *patch_coord < 0 {
        *cell_coord -= 1;
        *patch_coord += 16;
    } else if *patch_coord >= 16 {
        *cell_coord += 1;
        *patch_coord -= 16;
    }
}

/// Convert a render vertex (0..64) to a texture patch coordinate (0..15).
pub fn vertex_to_patch(vertex_x: i32, vertex_y: i32) -> (i32, i32) {
    let patch_x = ((vertex_x - 2) as f32 / 4.0).floor() as i32;
    let patch_y = ((vertex_y - 2) as f32 / 4.0).ceil() as i32;
    (patch_x, patch_y)
}

/// Read the raw VTEX index stored in a cell's texture_indices array.
pub fn raw_vtex_index_at_patch(cell: &TerrainCell<'_>, patch_x: usize, patch_y: usize) -> u16 {
    let parent_index = (patch_x / 4) + 4 * (patch_y / 4);
    let shape_index = (patch_x % 4) + 4 * (patch_y % 4);
    cell.texture_indices[parent_index][shape_index]
}

/// Resolve a raw VTEX index to a texture path string.
pub fn resolve_raw_vtex<'a>(cell: &TerrainCell<'a>, raw_vtex_index: u16, default: &'a str) -> &'a str {
    if raw_vtex_index == 0 {
        return default;
    }
    cell.texture_table.get(&(raw_vtex_index - 1)).copied().unwrap_or(default)
}

/// Full cross-cell texture lookup for a render vertex.
pub fn sample_vertex_texture<'a>(
    terrain_cells: &TerrainCells<'a>,
    cell_x: i32,
    cell_y: i32,
    vertex_x: i32,
    vertex_y: i32,
    default_texture: &'a str,
) -> SampledTerrainTexture<'a> {
    let (mut px, mut py) = vertex_to_patch(vertex_x, vertex_y);

    let mut cx = cell_x;
    let mut cy = cell_y;

    mod_cell(&mut cx, &mut px);
    mod_cell(&mut cy, &mut py);

    // Clamp to valid patch range
    let px = px.clamp(0, 15) as usize;
    let py = py.clamp(0, 15) as usize;

    let Some(cell) = terrain_cells.get(&(cx, cy)) else {
        return SampledTerrainTexture {
            cell: (cx, cy),
            patch: (px, py),
            raw_vtex_index: 0,
            path: default_texture,
        };
    };

    let raw_vtex_index = raw_vtex_index_at_patch(cell, px, py);
    let path = resolve_raw_vtex(cell, raw_vtex_index, default_texture);

    SampledTerrainTexture {
        cell: (cx, cy),
        patch: (px, py),
        raw_vtex_index,
        path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_default_cell() -> TerrainCell<'static> {
        TerrainCell {
            grid: (0, 0),
            heights: Box::new([[0.0; 65]; 65]),
            normals: TerrainNormals::Encoded(Box::new([[[0, 0, 1]; 65]; 65])),
            colors: TerrainColors::Encoded(Box::new([[[255; 3]; 65]; 65])),
            texture_indices: Box::new([[0; 16]; 16]),
            texture_table: Arc::new(IndexMap::default()),
        }
    }

    #[test]
    fn default_cell_is_detected_as_default() {
        assert!(make_default_cell().is_default());
    }

    #[test]
    fn cell_with_nonuniform_heights_is_not_default() {
        let mut cell = make_default_cell();
        cell.heights[0][1] = 1.0;

        assert!(!cell.is_default());
    }

    #[test]
    fn cell_with_custom_normals_is_not_default() {
        let mut cell = make_default_cell();
        let TerrainNormals::Encoded(normals) = &mut cell.normals else {
            unreachable!();
        };
        normals.as_flattened_mut()[1] = [1, 0, 0];

        assert!(!cell.is_default());
    }

    #[test]
    fn cell_with_custom_colors_is_not_default() {
        let mut cell = make_default_cell();
        let TerrainColors::Encoded(colors) = &mut cell.colors else {
            unreachable!();
        };
        colors.as_flattened_mut()[2] = [128, 255, 255];

        assert!(!cell.is_default());
    }

    #[test]
    fn cell_with_vtex_assignment_is_not_default() {
        let mut cell = make_default_cell();
        cell.texture_indices[0][0] = 1;

        assert!(!cell.is_default());
    }
}
