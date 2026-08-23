//! Terrain atlas region growth and packing helpers.

use hashbrown::HashSet;

/// One rectangular region inside the packed terrain atlas.
///
/// Field order is part of the world-mesh cache fingerprint
/// (hashed as raw bytes via `bytemuck::cast_slice`). Reordering,
/// adding, or removing fields will silently invalidate every
/// existing on-disk cache entry.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, bytemuck::Zeroable, bytemuck::Pod)]
pub struct TerrainAtlasRegion {
    /// Minimum covered cell X coordinate.
    pub min_x: i32,
    /// Maximum covered cell X coordinate.
    pub max_x: i32,
    /// Minimum covered cell Y coordinate.
    pub min_y: i32,
    /// Maximum covered cell Y coordinate.
    pub max_y: i32,
    /// Packed atlas offset, in cells, from the atlas origin on X.
    pub offset_x: i32,
    /// Packed atlas offset, in cells, from the atlas origin on Y.
    pub offset_y: i32,
}

impl TerrainAtlasRegion {
    /// Returns the region width in cells.
    pub fn width(&self) -> i32 {
        self.max_x - self.min_x + 1
    }

    /// Returns the region height in cells.
    pub fn height(&self) -> i32 {
        self.max_y - self.min_y + 1
    }
}

/// Packed layout of all terrain atlas regions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerrainAtlasLayout {
    /// Packed regions, each with atlas-cell offsets.
    pub regions: Vec<TerrainAtlasRegion>,
    /// Atlas width in cells.
    pub span_x: i32,
    /// Atlas height in cells.
    pub span_y: i32,
}

/// Builds packed terrain atlas regions from the set of landscape cell coordinates.
///
/// The algorithm has three phases:
///
/// 1. **Region growing** – starting from the top-left unvisited cell, each seed
///    expands outward (up, down, left, right) in a loop until no further neighbours
///    are found, greedily absorbing adjacent cells into one rectangular bounding box.
///
/// 2. **Wide-region packing** – regions are sorted by descending width.  The widest
///    region becomes row 0 of the atlas.  Any subsequent region whose width is at
///    least 75 % of the first region is stacked directly below it in a single column.
///    This preserves stable UV alignment with the legacy layout.
///
/// 3. **Remaining-region packing** – leftover regions are sorted by descending height
///    and tiled left-to-right.  When a new region would overflow the established atlas
///    width, a new row is started.
pub fn build_terrain_atlas_layout<I>(land_keys: I) -> TerrainAtlasLayout
where
    I: IntoIterator<Item = (i32, i32)>,
{
    let keys: Vec<(i32, i32)> = land_keys.into_iter().collect();
    if keys.is_empty() {
        return TerrainAtlasLayout::default();
    }

    let mut unvisited: HashSet<(i32, i32)> = keys.iter().copied().collect();

    let mut map_min_x = i32::MAX;
    let mut map_max_x = i32::MIN;
    let mut map_min_y = i32::MAX;
    let mut map_max_y = i32::MIN;

    for &(x, y) in &keys {
        map_min_x = map_min_x.min(x);
        map_max_x = map_max_x.max(x);
        map_min_y = map_min_y.min(y);
        map_max_y = map_max_y.max(y);
    }

    let mut regions = Vec::new();

    for y in map_min_y..=map_max_y {
        for x in map_min_x..=map_max_x {
            if !unvisited.contains(&(x, y)) {
                continue;
            }

            unvisited.remove(&(x, y));

            let mut r_min_x = x;
            let mut r_max_x = x;
            let mut r_min_y = y;
            let mut r_max_y = y;

            // Extend on each axis separately so the region bounding box stays rectangular.
            // Cells on the orthogonal edges of the current rectangle are also considered,
            // which gives the greedily-absorbed "one cell thick border" expansion semantics.
            let mut continue_search = true;
            while continue_search {
                continue_search = false;

                let mut extend = false;
                for search_x in (r_min_x - 1)..=(r_max_x + 1) {
                    if unvisited.contains(&(search_x, r_min_y - 1)) {
                        unvisited.remove(&(search_x, r_min_y - 1));
                        extend = true;
                    }
                }
                if extend {
                    r_min_y -= 1;
                    continue_search = true;
                }

                let mut extend = false;
                for search_x in (r_min_x - 1)..=(r_max_x + 1) {
                    if unvisited.contains(&(search_x, r_max_y + 1)) {
                        unvisited.remove(&(search_x, r_max_y + 1));
                        extend = true;
                    }
                }
                if extend {
                    r_max_y += 1;
                    continue_search = true;
                }

                let mut extend = false;
                for search_y in (r_min_y - 1)..=(r_max_y + 1) {
                    if unvisited.contains(&(r_min_x - 1, search_y)) {
                        unvisited.remove(&(r_min_x - 1, search_y));
                        extend = true;
                    }
                }
                if extend {
                    r_min_x -= 1;
                    continue_search = true;
                }

                let mut extend = false;
                for search_y in (r_min_y - 1)..=(r_max_y + 1) {
                    if unvisited.contains(&(r_max_x + 1, search_y)) {
                        unvisited.remove(&(r_max_x + 1, search_y));
                        extend = true;
                    }
                }
                if extend {
                    r_max_x += 1;
                    continue_search = true;
                }
            }

            regions.push(TerrainAtlasRegion {
                min_x: r_min_x,
                max_x: r_max_x,
                min_y: r_min_y,
                max_y: r_max_y,
                offset_x: 0,
                offset_y: 0,
            });
        }
    }

    // Keep the packing behavior aligned with the legacy layout logic so generated UVs
    // remain stable across old and new code paths.
    // Descending width ensures the largest continuous mainland block is placed first.
    regions.sort_by(|a, b| (b.max_x - b.min_x).cmp(&(a.max_x - a.min_x)));

    let mut packed = Vec::new();
    let mut span_x = 0i32;
    let mut span_y = 0i32;

    if !regions.is_empty() {
        let mut first = regions.remove(0);
        first.offset_x = 0;
        first.offset_y = 0;

        span_x = first.max_x - first.min_x + 1;
        span_y = first.max_y - first.min_y + 1;
        let mut current_offset_y = span_y;

        packed.push(first);

        // Regions within 75% of the widest region's width are stacked in a single
        // column beneath it, keeping the primary landmass contiguous in the atlas.
        let width_limit = (span_x * 3) / 4;

        while !regions.is_empty() {
            let width = regions[0].max_x - regions[0].min_x + 1;
            let height = regions[0].max_y - regions[0].min_y + 1;
            if width < width_limit {
                break;
            }

            let mut r = regions.remove(0);
            r.offset_x = 0;
            r.offset_y = current_offset_y;
            current_offset_y += height;
            span_y += height;

            packed.push(r);
        }

        regions.sort_by(|a, b| (b.max_y - b.min_y).cmp(&(a.max_y - a.min_y)));

        let mut current_offset_x = 0;
        while !regions.is_empty() {
            let mut r = regions.remove(0);
            let width = r.max_x - r.min_x + 1;
            let height = r.max_y - r.min_y + 1;

            if current_offset_x + width > span_x {
                current_offset_x = 0;
                current_offset_y = span_y;
            }
            if current_offset_x == 0 {
                span_y += height;
            }

            r.offset_x = current_offset_x;
            r.offset_y = current_offset_y;
            current_offset_x += width;

            packed.push(r);
        }
    }

    TerrainAtlasLayout {
        regions: packed,
        span_x,
        span_y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input() {
        let layout = build_terrain_atlas_layout(std::iter::empty());
        assert_eq!(layout.span_x, 0);
        assert_eq!(layout.span_y, 0);
        assert!(layout.regions.is_empty());
    }

    #[test]
    fn single_cell() {
        let layout = build_terrain_atlas_layout([(0, 0)]);
        assert_eq!(layout.span_x, 1);
        assert_eq!(layout.span_y, 1);
        assert_eq!(layout.regions.len(), 1);
        let r = &layout.regions[0];
        assert_eq!(r.width(), 1);
        assert_eq!(r.height(), 1);
    }

    #[test]
    fn two_adjacent_cells() {
        let layout = build_terrain_atlas_layout([(0, 0), (1, 0)]);
        assert_eq!(layout.regions.len(), 1, "adjacent cells should merge into one region");
    }

    #[test]
    fn two_disconnected_cells() {
        let layout = build_terrain_atlas_layout([(0, 0), (10, 10)]);
        assert_eq!(layout.regions.len(), 2, "disconnected cells should produce two regions");
    }

    #[test]
    fn l_shaped_group() {
        // (0,0),(1,0),(0,1): L-shape
        let layout = build_terrain_atlas_layout([(0, 0), (1, 0), (0, 1)]);
        assert_eq!(layout.regions.len(), 1, "connected L-shape should be one region");
        let r = &layout.regions[0];
        assert_eq!(r.min_x, 0);
        assert_eq!(r.max_x, 1);
        assert_eq!(r.min_y, 0);
        assert_eq!(r.max_y, 1);
    }
}
