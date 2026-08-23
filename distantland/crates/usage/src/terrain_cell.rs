//! Decoded LAND-cell values owned by plugin usage scanning.

use std::sync::Arc;

use glam::{Vec3, Vec4};

use crate::{IndexMap, Vfs};

/// Morrowind's fallback landscape texture.
pub const DEFAULT_LAND_TEXTURE: &str = "_land_default.tga";

/// Mapping from decoded LTEX indices to normalized terrain texture keys.
pub type TerrainTextureTable<'a> = Arc<IndexMap<u16, &'a str>>;

/// Decoded terrain data for one landscape cell.
#[derive(Clone, Debug)]
pub struct TerrainCell<'a> {
    /// Cell grid coordinates.
    pub grid: (i32, i32),
    /// Vertex heights for the 65x65 landscape grid.
    pub heights: Box<[[f32; 65]; 65]>,
    /// Vertex normals for the 65x65 landscape grid.
    pub normals: Vec<Vec3>,
    /// Vertex colors for the 65x65 landscape grid.
    pub colors: Vec<Vec4>,
    /// Raw texture indices for the 16x16 cell patches.
    pub texture_indices: Box<[[u16; 16]; 16]>,
    /// Resolved LTEX table for the cell's originating plugin.
    pub texture_table: TerrainTextureTable<'a>,
}

/// Terrain cells keyed by `(cell_x, cell_y)`.
pub type TerrainCells<'a> = IndexMap<(i32, i32), TerrainCell<'a>>;

impl<'a> TerrainCell<'a> {
    /// Decodes a `Landscape` record into a terrain cell with sensible fallbacks for missing data.
    pub fn from_landscape(landscape: &tes3::esp::Landscape, texture_table: TerrainTextureTable<'a>) -> Self {
        use tes3::esp::LandscapeFlags;

        let grid = landscape.grid;
        let flags = landscape.landscape_flags;
        let heights = landscape.decode_vertex_heights();
        let normals = if flags.contains(LandscapeFlags::USES_VERTEX_HEIGHTS_AND_NORMALS) {
            landscape.decode_vertex_normals()
        } else {
            vec![Vec3::Z; 65 * 65]
        };
        let colors = if flags.contains(LandscapeFlags::USES_VERTEX_COLORS) {
            landscape.decode_vertex_colors()
        } else {
            vec![Vec4::new(1.0, 1.0, 1.0, 0.0); 65 * 65]
        };
        let texture_indices = if flags.contains(LandscapeFlags::USES_TEXTURES) {
            landscape.texture_indices.data.clone()
        } else {
            Box::new([[0; 16]; 16])
        };

        Self {
            grid,
            heights,
            normals,
            colors,
            texture_indices,
            texture_table,
        }
    }

    /// Returns whether this cell contains only default/trivial terrain data.
    pub fn is_default(&self) -> bool {
        const DEFAULT_NORMAL: Vec3 = Vec3::Z;
        const DEFAULT_COLOR: Vec4 = Vec4::new(1.0, 1.0, 1.0, 0.0);
        let uniform_height = self.heights[0][0];
        self.heights.as_flattened().iter().all(|&height| height == uniform_height)
            && self.normals.iter().all(|&normal| normal == DEFAULT_NORMAL)
            && self.colors.iter().all(|&color| color == DEFAULT_COLOR)
            && self.texture_indices.as_flattened().iter().all(|&index| index == 0)
    }
}

/// Resolves the default land texture key through the VFS when possible.
pub fn default_land_texture_key(vfs: &Vfs) -> &str {
    vfs.resolve_texture_key(DEFAULT_LAND_TEXTURE).unwrap_or(DEFAULT_LAND_TEXTURE)
}
