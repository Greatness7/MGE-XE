//! Decoded LAND-cell values owned by plugin usage scanning.

use std::sync::Arc;

use glam::{Vec3, Vec4};

use crate::{IndexMap, Vfs};

/// Morrowind's fallback landscape texture.
pub const DEFAULT_LAND_TEXTURE: &str = "_land_default.tga";

/// Mapping from decoded LTEX indices to normalized terrain texture keys.
pub type TerrainTextureTable<'a> = Arc<IndexMap<u16, &'a str>>;

/// Compact vertex-normal payload for a 65x65 landscape grid.
#[derive(Clone, Debug, Default)]
pub enum TerrainNormals {
    /// The VNML record is absent; every vertex decodes to +Z.
    #[default]
    Default,
    /// Raw VNML bytes retained in TES3 row-major layout.
    Encoded(Box<[[[i8; 3]; 65]; 65]>),
}

impl TerrainNormals {
    /// Decodes one row-major vertex normal using the pinned `tes3` semantics.
    ///
    /// # Panics
    ///
    /// Panics when `index` is outside the 65x65 vertex grid.
    #[inline]
    pub fn get(&self, index: usize) -> Vec3 {
        match self {
            Self::Default => {
                assert!(index < 65 * 65, "terrain normal index out of bounds");
                Vec3::Z
            }
            Self::Encoded(data) => {
                let [x, y, z] = data.as_flattened()[index];
                let mut normal = Vec3::ZERO;
                normal.x = x as f32;
                normal.y = y as f32;
                normal.z = z as f32;
                normal.try_normalize().unwrap_or(Vec3::Z)
            }
        }
    }

    /// Iterates decoded normals in TES3 row-major order.
    #[inline]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = Vec3> + '_ {
        (0..65 * 65).map(|index| self.get(index))
    }
}

/// Compact vertex-color payload for a 65x65 landscape grid.
#[derive(Clone, Debug, Default)]
pub enum TerrainColors {
    /// The VCLR record is absent; every vertex decodes to opaque-white RGB with alpha zero.
    #[default]
    Default,
    /// Raw VCLR bytes retained in TES3 row-major layout.
    Encoded(Box<[[[u8; 3]; 65]; 65]>),
}

impl TerrainColors {
    /// Decodes one row-major vertex color using the pinned `tes3` semantics.
    ///
    /// # Panics
    ///
    /// Panics when `index` is outside the 65x65 vertex grid.
    #[inline]
    pub fn get(&self, index: usize) -> Vec4 {
        match self {
            Self::Default => {
                assert!(index < 65 * 65, "terrain color index out of bounds");
                Vec4::new(1.0, 1.0, 1.0, 0.0)
            }
            Self::Encoded(data) => {
                let [r, g, b] = data.as_flattened()[index];
                let mut color = Vec4::ZERO;
                color.x = r as f32;
                color.y = g as f32;
                color.z = b as f32;
                color /= 255.0;
                color
            }
        }
    }

    /// Iterates decoded colors in TES3 row-major order.
    #[inline]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = Vec4> + '_ {
        (0..65 * 65).map(|index| self.get(index))
    }
}

/// Decoded terrain data for one landscape cell.
#[derive(Clone, Debug)]
pub struct TerrainCell<'a> {
    /// Cell grid coordinates.
    pub grid: (i32, i32),
    /// Vertex heights for the 65x65 landscape grid.
    pub heights: Box<[[f32; 65]; 65]>,
    /// Vertex normals for the 65x65 landscape grid.
    pub normals: TerrainNormals,
    /// Vertex colors for the 65x65 landscape grid.
    pub colors: TerrainColors,
    /// Raw texture indices for the 16x16 cell patches.
    pub texture_indices: Box<[[u16; 16]; 16]>,
    /// Resolved LTEX table for the cell's originating plugin.
    pub texture_table: TerrainTextureTable<'a>,
}

/// Terrain cells keyed by `(cell_x, cell_y)`.
pub type TerrainCells<'a> = IndexMap<(i32, i32), TerrainCell<'a>>;

impl<'a> TerrainCell<'a> {
    /// Retains a `Landscape` record as a compact terrain cell with sensible fallbacks for missing data.
    pub fn from_landscape(landscape: &tes3::esp::Landscape, texture_table: TerrainTextureTable<'a>) -> Self {
        use tes3::esp::LandscapeFlags;

        let grid = landscape.grid;
        let flags = landscape.landscape_flags;
        let heights = landscape.decode_vertex_heights();
        let normals = if flags.contains(LandscapeFlags::USES_VERTEX_HEIGHTS_AND_NORMALS) {
            TerrainNormals::Encoded(landscape.vertex_normals.data.clone())
        } else {
            TerrainNormals::Default
        };
        let colors = if flags.contains(LandscapeFlags::USES_VERTEX_COLORS) {
            TerrainColors::Encoded(landscape.vertex_colors.data.clone())
        } else {
            TerrainColors::Default
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
            && self.normals.iter().all(|normal| normal == DEFAULT_NORMAL)
            && self.colors.iter().all(|color| color == DEFAULT_COLOR)
            && self.texture_indices.as_flattened().iter().all(|&index| index == 0)
    }
}

/// Resolves the default land texture key through the VFS when possible.
pub fn default_land_texture_key(vfs: &Vfs) -> &str {
    vfs.resolve_texture_key(DEFAULT_LAND_TEXTURE).unwrap_or(DEFAULT_LAND_TEXTURE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tes3::esp::{Landscape, LandscapeFlags};

    fn bits3(value: Vec3) -> [u32; 3] {
        value.to_array().map(f32::to_bits)
    }

    fn bits4(value: Vec4) -> [u32; 4] {
        value.to_array().map(f32::to_bits)
    }

    #[test]
    fn compact_accessors_match_pinned_tes3_decoders() {
        let mut landscape = Landscape::default();
        landscape.landscape_flags = LandscapeFlags::USES_VERTEX_HEIGHTS_AND_NORMALS | LandscapeFlags::USES_VERTEX_COLORS;

        let normal_samples = [[-128, -1, 0], [0, 0, 0], [0, 0, 127], [127, 127, 127]];
        let color_samples = [[0, 1, 255], [0, 0, 0], [255, 255, 255], [127, 128, 129]];
        for (index, sample) in normal_samples.into_iter().enumerate() {
            landscape.vertex_normals.data.as_flattened_mut()[index] = sample;
        }
        for (index, sample) in color_samples.into_iter().enumerate() {
            landscape.vertex_colors.data.as_flattened_mut()[index] = sample;
        }

        let expected_normals = landscape.decode_vertex_normals();
        let expected_colors = landscape.decode_vertex_colors();
        let cell = TerrainCell::from_landscape(&landscape, Arc::default());

        assert!(matches!(&cell.normals, TerrainNormals::Encoded(_)));
        assert!(matches!(&cell.colors, TerrainColors::Encoded(_)));
        assert_eq!(
            cell.normals.iter().map(bits3).collect::<Vec<_>>(),
            expected_normals.into_iter().map(bits3).collect::<Vec<_>>()
        );
        assert_eq!(
            cell.colors.iter().map(bits4).collect::<Vec<_>>(),
            expected_colors.into_iter().map(bits4).collect::<Vec<_>>()
        );
    }

    #[test]
    fn absent_payloads_remain_distinct_from_encoded_zeroes() {
        let absent = TerrainCell::from_landscape(&Landscape::default(), Arc::default());
        assert!(matches!(&absent.normals, TerrainNormals::Default));
        assert!(matches!(&absent.colors, TerrainColors::Default));
        assert_eq!(absent.normals.get(0), Vec3::Z);
        assert_eq!(absent.colors.get(0), Vec4::new(1.0, 1.0, 1.0, 0.0));

        let mut encoded = Landscape::default();
        encoded.landscape_flags = LandscapeFlags::USES_VERTEX_HEIGHTS_AND_NORMALS | LandscapeFlags::USES_VERTEX_COLORS;
        let encoded = TerrainCell::from_landscape(&encoded, Arc::default());
        assert!(matches!(&encoded.normals, TerrainNormals::Encoded(_)));
        assert!(matches!(&encoded.colors, TerrainColors::Encoded(_)));
        assert_eq!(encoded.normals.get(0), Vec3::Z);
        assert_eq!(encoded.colors.get(0), Vec4::ZERO);
    }

    #[test]
    fn retained_payload_sizes_match_land_records() {
        assert_eq!(std::mem::size_of::<[[[i8; 3]; 65]; 65]>(), 12_675);
        assert_eq!(std::mem::size_of::<[[[u8; 3]; 65]; 65]>(), 12_675);
        assert!(std::mem::size_of::<TerrainNormals>() <= 2 * std::mem::size_of::<usize>());
        assert!(std::mem::size_of::<TerrainColors>() <= 2 * std::mem::size_of::<usize>());
    }
}
