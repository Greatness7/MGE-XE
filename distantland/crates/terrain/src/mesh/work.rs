//! Terrain mesh work identity and dependency enumeration.
//!
//! Declares *what* must be built from a terrain atlas layout: stable work keys,
//! per-item chunk dependencies, and the region-relative geometry coordinates the
//! parent builder consumes. Dense-grid construction and simplification live in
//! the parent module and depend on this one-way handoff.

use anyhow::{Result, bail};
use hashbrown::HashSet;

use super::MESH_CHUNK_CELLS_PER_SIDE;
use crate::layout::{TerrainAtlasLayout, TerrainAtlasRegion};
use distantland_foundation::units::{TerrainChunkUnitKey, overlapping_absolute_chunks};

use distantland_foundation::units::TerrainMeshWorkKey;

/// Inclusive absolute cell rectangle a work item owns, clipped to its region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct WorkCellRect {
    pub(super) min_x: i32,
    pub(super) min_y: i32,
    pub(super) max_x: i32,
    pub(super) max_y: i32,
}

impl WorkCellRect {
    /// Clips a work item's nominal square to the cells its region actually owns.
    pub(super) fn owned_by(region: &TerrainAtlasRegion, start_cell_x: i32, start_cell_y: i32) -> Self {
        let span = i32::try_from(MESH_CHUNK_CELLS_PER_SIDE).expect("mesh chunk cells per side must fit in i32");
        Self {
            min_x: start_cell_x,
            min_y: start_cell_y,
            max_x: (start_cell_x + span - 1).min(region.max_x),
            max_y: (start_cell_y + span - 1).min(region.max_y),
        }
    }
}

/// One enumerated mesh work item: its identity, its declared dependencies, and the region-relative
/// coordinates the existing builder consumes unchanged.
#[derive(Clone, Debug)]
pub struct TerrainMeshWorkItem {
    /// Stable logical identity of this item.
    pub key: TerrainMeshWorkKey,
    /// Every absolute chunk unit whose fingerprint covers something this item reads.
    pub dependencies: Vec<TerrainChunkUnitKey>,
    /// Atlas region that owns this work item's cell square.
    pub(super) region: TerrainAtlasRegion,
    /// Region-relative patch column index.
    pub(super) patch_x: usize,
    /// Region-relative patch row index.
    pub(super) patch_y: usize,
    /// Absolute cells this item owns after clipping to its region.
    pub(super) cell_rect: WorkCellRect,
}

impl TerrainMeshWorkItem {
    /// Whether any declared chunk dependency is in the supplied dirty set.
    pub fn depends_on_any(&self, dirty: &HashSet<TerrainChunkUnitKey>) -> bool {
        self.dependencies.iter().any(|chunk| dirty.contains(chunk))
    }
}

/// Enumerates every mesh work item implied by a layout, in no particular order.
///
/// # Errors
///
/// Returns an error when two items share a work key, which would make record identity ambiguous.
pub fn enumerate_terrain_mesh_work(layout: &TerrainAtlasLayout) -> Result<Vec<TerrainMeshWorkItem>> {
    let cells_per_side = u32::try_from(MESH_CHUNK_CELLS_PER_SIDE).expect("mesh chunk cells per side must fit in u32");
    let mut work = Vec::new();
    let mut seen = HashSet::new();
    for region in &layout.regions {
        let patches_across = usize::try_from(region.width())
            .expect("terrain region width must be non-negative")
            .div_ceil(MESH_CHUNK_CELLS_PER_SIDE);
        let patches_down = usize::try_from(region.height())
            .expect("terrain region height must be non-negative")
            .div_ceil(MESH_CHUNK_CELLS_PER_SIDE);
        // Reserving per region avoids re-deriving the total in a pre-pass: `work` holds sizeable
        // items, so growing from zero copies every one of them at each reallocation.
        work.reserve(patches_across * patches_down);
        seen.reserve(patches_across * patches_down);
        for patch_y in 0..patches_down {
            for patch_x in 0..patches_across {
                let start_cell_x = region.min_x + (patch_x * MESH_CHUNK_CELLS_PER_SIDE) as i32;
                let start_cell_y = region.min_y + (patch_y * MESH_CHUNK_CELLS_PER_SIDE) as i32;
                let key = TerrainMeshWorkKey {
                    start_cell_x,
                    start_cell_y,
                    cells_per_side,
                };
                if !seen.insert(key) {
                    bail!(
                        "terrain layout produced duplicate mesh work key at cell {start_cell_x},{start_cell_y} \
                         ({MESH_CHUNK_CELLS_PER_SIDE} cells per side)"
                    );
                }
                work.push(TerrainMeshWorkItem {
                    key,
                    dependencies: overlapping_absolute_chunks(start_cell_x, start_cell_y, MESH_CHUNK_CELLS_PER_SIDE),
                    region: *region,
                    patch_x,
                    patch_y,
                    cell_rect: WorkCellRect::owned_by(region, start_cell_x, start_cell_y),
                });
            }
        }
    }
    Ok(work)
}

#[cfg(test)]
mod tests;
