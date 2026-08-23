use anyhow::bail;
use itertools::Itertools;

use super::{area, bottom, contains, intersects, right, usable_rect};
use crate::atlas::types::CachedAtlasRect;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct FitScore {
    area_waste: u64,
    short_side_waste: u32,
    page_id: usize,
    top: u32,
    x: u32,
}

impl FitScore {
    pub(super) fn with_page(self, page_id: usize) -> Self {
        Self { page_id, ..self }
    }
}

/// Guillotine free-rectangle partitioner over one atlas page, seeded from prior occupied slots.
///
/// Placements use best-fit scoring and subtract the placed reservation from every overlapping free
/// rectangle; subtraction is exact guillotine splitting, so retained regions stay rectangular.
pub(super) struct SeededFreeList {
    pub(super) free: Vec<CachedAtlasRect>,
}

impl SeededFreeList {
    pub(super) fn new(
        page_width: u32,
        page_height: u32,
        border_padding: u32,
        occupied: impl IntoIterator<Item = (u64, CachedAtlasRect)>,
    ) -> anyhow::Result<Self> {
        let occupied = occupied.into_iter().sorted_unstable_by_key(|(slot_id, _)| *slot_id);
        let usable = usable_rect(page_width, page_height, border_padding)?;
        let mut result = Self { free: vec![usable] };
        for (_, rect) in occupied {
            if !contains(&usable, &rect) {
                bail!("atlas occupied reservation is outside the page usable interior");
            }
            result.subtract(rect)?;
        }
        Ok(result)
    }

    pub(super) fn best_fit(&self, width: u32, height: u32) -> Option<FitScore> {
        self.free
            .iter()
            .filter(|rect| rect.width >= width && rect.height >= height)
            .map(|rect| FitScore {
                area_waste: area(*rect) - u64::from(width) * u64::from(height),
                short_side_waste: (rect.width - width).min(rect.height - height),
                page_id: 0,
                top: rect.y + height,
                x: rect.x,
            })
            .min()
    }

    pub(super) fn insert(&mut self, width: u32, height: u32) -> Option<CachedAtlasRect> {
        let score = self.best_fit(width, height)?;
        let rect = CachedAtlasRect {
            x: score.x,
            y: score.top - height,
            width,
            height,
        };
        self.subtract(rect).ok()?;
        Some(rect)
    }

    fn subtract(&mut self, node: CachedAtlasRect) -> anyhow::Result<()> {
        let mut next = Vec::new();
        for free in self.free.drain(..) {
            if !intersects(&free, &node) {
                next.push(free);
                continue;
            }
            let free_right = right(free)?;
            let free_bottom = bottom(free)?;
            let node_right = right(node)?;
            let node_bottom = bottom(node)?;
            let ix1 = free.x.max(node.x);
            let iy1 = free.y.max(node.y);
            let ix2 = free_right.min(node_right);
            let iy2 = free_bottom.min(node_bottom);
            if iy1 > free.y {
                next.push(CachedAtlasRect {
                    x: free.x,
                    y: free.y,
                    width: free.width,
                    height: iy1 - free.y,
                });
            }
            if iy2 < free_bottom {
                next.push(CachedAtlasRect {
                    x: free.x,
                    y: iy2,
                    width: free.width,
                    height: free_bottom - iy2,
                });
            }
            if ix1 > free.x && iy2 > iy1 {
                next.push(CachedAtlasRect {
                    x: free.x,
                    y: iy1,
                    width: ix1 - free.x,
                    height: iy2 - iy1,
                });
            }
            if ix2 < free_right && iy2 > iy1 {
                next.push(CachedAtlasRect {
                    x: ix2,
                    y: iy1,
                    width: free_right - ix2,
                    height: iy2 - iy1,
                });
            }
        }
        next.retain(|rect| rect.width > 0 && rect.height > 0);
        next.sort_unstable_by_key(|rect| (rect.x, rect.y, rect.width, rect.height));
        next.dedup();
        let snapshot = next.clone();
        next.retain(|rect| !snapshot.iter().any(|other| rect != other && contains(other, rect)));
        self.free = next;
        Ok(())
    }
}
