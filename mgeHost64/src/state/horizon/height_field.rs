use crate::abi::{D3dxVector2, OcclusionFormatError, TerrainFileHeader, TerrainOcclusionData};
#[cfg(test)]
use crate::abi::{TerrainFileLayout, TerrainFormatError, TerrainVertex};

pub(super) const EMPTY_HEIGHT: f32 = f32::MIN;

#[derive(Clone, Debug)]
pub(super) struct MaxHeightLevel {
    pub(super) spacing: f32,
    pub(super) nx: u32,
    pub(super) ny: u32,
    pub(super) max_z: Vec<f32>,
}

/// Static terrain max-height grid used as the occluder source for horizon culling.
#[derive(Clone, Debug)]
pub struct TerrainHeightField {
    pub origin: D3dxVector2,
    pub size: D3dxVector2,
    pub spacing: f32,
    pub nx: u32,
    pub ny: u32,
    pub(super) max_z: Vec<f32>,
    pub(super) covered_cells: usize,
    pub(super) global_max_z: f32,
    /// 2x2-reduced max-height levels used by the hierarchical builder.
    pub(super) levels: Vec<MaxHeightLevel>,
}

impl TerrainHeightField {
    /// Builds a max-height grid from decoded terrain vertices.
    ///
    /// Test-only oracle for the generated occlusion asset; production loads go through
    /// [`TerrainHeightField::from_occlusion`].
    #[cfg(test)]
    pub fn build_from_layout(layout: &TerrainFileLayout, bytes: &[u8], spacing: f32) -> Result<Self, TerrainFormatError> {
        if !spacing.is_finite() || spacing <= 0.0 {
            return Err(TerrainFormatError::InvalidRegion("horizon sample spacing must be positive"));
        }
        let size = D3dxVector2 {
            x: layout.header.world_size[0],
            y: layout.header.world_size[1],
        };
        if !size.x.is_finite() || !size.y.is_finite() || size.x <= 0.0 || size.y <= 0.0 {
            return Err(TerrainFormatError::InvalidRegion("horizon world size must be positive"));
        }

        let nx = (size.x / spacing).ceil() as u32 + 1;
        let ny = (size.y / spacing).ceil() as u32 + 1;
        let cell_count = (nx as usize)
            .checked_mul(ny as usize)
            .ok_or(TerrainFormatError::IntegerOverflow("terrain horizon height field cells"))?;
        let mut field = Self {
            origin: D3dxVector2 {
                x: layout.header.world_origin[0],
                y: layout.header.world_origin[1],
            },
            size,
            spacing,
            nx,
            ny,
            max_z: vec![EMPTY_HEIGHT; cell_count],
            covered_cells: 0,
            global_max_z: EMPTY_HEIGHT,
            levels: Vec::new(),
        };

        for mesh in &layout.meshes {
            for vertex in mesh.iter_vertices(bytes)? {
                field.add_vertex(vertex);
            }
        }
        field.build_pyramid();
        Ok(field)
    }

    /// Builds a height field from a parsed `terrain_occlusion.bin` asset paired with `terrain.bin`.
    pub fn from_occlusion(
        data: TerrainOcclusionData,
        terrain_header: &TerrainFileHeader,
    ) -> Result<Self, OcclusionFormatError> {
        let TerrainOcclusionData { header, max_z } = data;
        if header.origin_cell != terrain_header.origin_cell {
            return Err(OcclusionFormatError::TerrainMismatch("origin_cell"));
        }
        if header.cell_size_xy != terrain_header.cell_size_xy {
            return Err(OcclusionFormatError::TerrainMismatch("cell_size_xy"));
        }
        if header.world_origin != terrain_header.world_origin {
            return Err(OcclusionFormatError::TerrainMismatch("world_origin"));
        }
        if header.world_size != terrain_header.world_size {
            return Err(OcclusionFormatError::TerrainMismatch("world_size"));
        }

        let expected_base_len = (header.base_nx as usize)
            .checked_mul(header.base_ny as usize)
            .ok_or(OcclusionFormatError::IntegerOverflow("base level sample count"))?;
        if max_z.len() != expected_base_len {
            return Err(OcclusionFormatError::InvalidGrid("base level payload length mismatch"));
        }

        let mut covered_cells = 0usize;
        let mut global_max_z = EMPTY_HEIGHT;
        for (index, &height) in max_z.iter().enumerate() {
            if !height.is_finite() {
                return Err(OcclusionFormatError::NonFiniteHeight { index });
            }
            if height != EMPTY_HEIGHT {
                covered_cells += 1;
                global_max_z = global_max_z.max(height);
            }
        }

        let mut field = Self {
            origin: D3dxVector2 {
                x: header.world_origin[0],
                y: header.world_origin[1],
            },
            size: D3dxVector2 {
                x: header.world_size[0],
                y: header.world_size[1],
            },
            spacing: header.base_spacing,
            nx: header.base_nx,
            ny: header.base_ny,
            max_z,
            covered_cells,
            global_max_z,
            levels: Vec::new(),
        };
        field.build_pyramid();

        Ok(field)
    }

    pub fn mip_level_count(&self) -> usize {
        self.levels.len()
    }

    pub fn mip_byte_size(&self) -> usize {
        self.levels.iter().map(|level| level.max_z.len() * size_of::<f32>()).sum()
    }

    /// Builds the immutable max-height pyramid.
    pub(super) fn build_pyramid(&mut self) {
        let mut levels: Vec<MaxHeightLevel> = Vec::new();
        let mut prev_nx = self.nx;
        let mut prev_ny = self.ny;
        let mut prev_spacing = self.spacing;

        while prev_nx > 1 || prev_ny > 1 {
            let nx = prev_nx.div_ceil(2);
            let ny = prev_ny.div_ceil(2);
            let mut max_z = vec![EMPTY_HEIGHT; nx as usize * ny as usize];
            for y in 0..ny {
                for x in 0..nx {
                    let mut m = EMPTY_HEIGHT;
                    for dy in 0..2u32 {
                        for dx in 0..2u32 {
                            let cx = x * 2 + dx;
                            let cy = y * 2 + dy;
                            if cx >= prev_nx || cy >= prev_ny {
                                continue;
                            }
                            let h = match levels.last() {
                                Some(previous) => previous.max_z[(cy * prev_nx + cx) as usize],
                                None => self.max_z[(cy * prev_nx + cx) as usize],
                            };
                            if h != EMPTY_HEIGHT {
                                m = m.max(h);
                            }
                        }
                    }
                    max_z[(y * nx + x) as usize] = m;
                }
            }
            let spacing = prev_spacing * 2.0;
            levels.push(MaxHeightLevel { spacing, nx, ny, max_z });
            prev_nx = nx;
            prev_ny = ny;
            prev_spacing = spacing;
        }
        self.levels = levels;
    }

    /// Returns a conservative max height over the requested AABB and pyramid level.
    pub(super) fn max_over_aabb(&self, level: usize, min_x: f32, min_y: f32, max_x: f32, max_y: f32) -> f32 {
        let (spacing, nx, ny, max_z): (f32, u32, u32, &[f32]) = if level == 0 {
            (self.spacing, self.nx, self.ny, &self.max_z)
        } else {
            let level = &self.levels[(level - 1).min(self.levels.len().saturating_sub(1))];
            (level.spacing, level.nx, level.ny, &level.max_z)
        };

        let clamped_min_x = min_x.max(self.origin.x);
        let clamped_min_y = min_y.max(self.origin.y);
        let clamped_max_x = max_x.min(self.origin.x + self.size.x);
        let clamped_max_y = max_y.min(self.origin.y + self.size.y);
        if clamped_min_x > clamped_max_x || clamped_min_y > clamped_max_y {
            return EMPTY_HEIGHT;
        }

        let ix0 = ((clamped_min_x - self.origin.x) / spacing)
            .floor()
            .clamp(0.0, (nx - 1) as f32) as u32;
        let iy0 = ((clamped_min_y - self.origin.y) / spacing)
            .floor()
            .clamp(0.0, (ny - 1) as f32) as u32;
        let ix1 = ((clamped_max_x - self.origin.x) / spacing)
            .floor()
            .clamp(0.0, (nx - 1) as f32) as u32;
        let iy1 = ((clamped_max_y - self.origin.y) / spacing)
            .floor()
            .clamp(0.0, (ny - 1) as f32) as u32;

        let mut result = EMPTY_HEIGHT;
        for iy in iy0..=iy1 {
            for ix in ix0..=ix1 {
                let h = max_z[(iy * nx + ix) as usize];
                if h != EMPTY_HEIGHT {
                    result = result.max(h);
                }
            }
        }
        result
    }

    /// Picks a pyramid level that resolves `extent` to at most a 2x2 cell block.
    pub(super) fn level_for_extent(&self, extent: f32) -> usize {
        if self.levels.is_empty() || extent <= self.spacing {
            return 0;
        }
        let level = (extent / self.spacing).log2().ceil();
        let level = if level.is_finite() { level.max(0.0) as usize } else { 0 };
        level.min(self.levels.len())
    }

    pub fn covered_cell_count(&self) -> usize {
        self.covered_cells
    }

    pub fn global_max_z(&self) -> Option<f32> {
        (self.global_max_z != EMPTY_HEIGHT).then_some(self.global_max_z)
    }

    pub fn contains_xy(&self, x: f32, y: f32) -> bool {
        x.is_finite()
            && y.is_finite()
            && x >= self.origin.x
            && y >= self.origin.y
            && x <= self.origin.x + self.size.x
            && y <= self.origin.y + self.size.y
    }

    /// Samples the conservative max height near `x,y`.
    pub fn sample_max_z(&self, x: f32, y: f32) -> Option<f32> {
        if !self.contains_xy(x, y) {
            return None;
        }
        let (base_x, base_y) = self.base_cell(x, y)?;

        let mut max_z = EMPTY_HEIGHT;
        for y_offset in 0..=1 {
            for x_offset in 0..=1 {
                let ix = base_x + x_offset;
                let iy = base_y + y_offset;
                if ix >= self.nx || iy >= self.ny {
                    continue;
                }
                let height = self.max_z[self.index(ix, iy)];
                if height != EMPTY_HEIGHT {
                    max_z = max_z.max(height);
                }
            }
        }
        (max_z != EMPTY_HEIGHT).then_some(max_z)
    }

    #[cfg(test)]
    fn add_vertex(&mut self, vertex: TerrainVertex) {
        if let Some((ix, iy)) = self.base_cell(vertex.position.x, vertex.position.y) {
            let index = self.index(ix, iy);
            if self.max_z[index] == EMPTY_HEIGHT {
                self.covered_cells += 1;
            }
            self.max_z[index] = self.max_z[index].max(vertex.position.z);
            self.global_max_z = self.global_max_z.max(vertex.position.z);
        }
    }

    fn base_cell(&self, x: f32, y: f32) -> Option<(u32, u32)> {
        if !self.contains_xy(x, y) {
            return None;
        }
        let ix = ((x - self.origin.x) / self.spacing).floor().clamp(0.0, (self.nx - 1) as f32) as u32;
        let iy = ((y - self.origin.y) / self.spacing).floor().clamp(0.0, (self.ny - 1) as f32) as u32;
        Some((ix, iy))
    }

    fn index(&self, ix: u32, iy: u32) -> usize {
        iy as usize * self.nx as usize + ix as usize
    }
}
