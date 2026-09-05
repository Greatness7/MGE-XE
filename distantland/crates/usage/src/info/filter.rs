use std::collections::BTreeSet;

use super::*;

impl<'a> UsageInfo<'a> {
    /// Clips the terrain cell bounding rectangle to fit within the control-texture limits.
    ///
    /// Uses a greedy boundary trim: while either the dimension limit or the byte limit is
    /// exceeded, drop whichever of the four boundary rows/columns holds the fewest populated
    /// cells. Ties are broken in a fixed axis order (min_x, max_x, min_y, max_y) to keep
    /// output deterministic.
    ///
    /// When cells are clipped, exterior references in the dropped region are also removed.
    /// The retained region is stored on `self` and read back through
    /// [`UsageInfo::terrain_control_clip`], both by the grass merge step and by the caller
    /// that reports the clip. Nothing is stored when no cells were removed.
    pub(crate) fn clip_terrain_control_region(&mut self, limits: &TerrainControlClipLimits) {
        if self.terrain_cells.is_empty() {
            return;
        }

        let (orig_min_x, orig_min_y, orig_max_x, orig_max_y) = terrain_cell_bounds(&self.terrain_cells);

        if !region_exceeds_limits(orig_min_x, orig_min_y, orig_max_x, orig_max_y, limits) {
            return;
        }

        let mut min_x = orig_min_x;
        let mut min_y = orig_min_y;
        let mut max_x = orig_max_x;
        let mut max_y = orig_max_y;

        // Build distinct populated coordinate lookups for O(1) boundary counting
        // and O(log N) coordinate snapping.
        let mut cells_by_x: hashbrown::HashMap<i32, Vec<i32>> = hashbrown::HashMap::new();
        let mut cells_by_y: hashbrown::HashMap<i32, Vec<i32>> = hashbrown::HashMap::new();
        for &(cx, cy) in self.terrain_cells.keys() {
            cells_by_x.entry(cx).or_default().push(cy);
            cells_by_y.entry(cy).or_default().push(cx);
        }
        let mut xs: Vec<i32> = cells_by_x.keys().copied().collect();
        xs.sort_unstable();
        let mut ys: Vec<i32> = cells_by_y.keys().copied().collect();
        ys.sort_unstable();

        while region_exceeds_limits(min_x, min_y, max_x, max_y, limits) {
            // Snap each boundary to the nearest populated coordinate before counting.
            // Draining empties one coordinate at a time converges to this state; snapping
            // avoids O(gap × extent) scans across empty space.
            let min_x_idx = xs.partition_point(|&x| x < min_x);
            let max_x_idx = xs.partition_point(|&x| x <= max_x);
            let min_y_idx = ys.partition_point(|&y| y < min_y);
            let max_y_idx = ys.partition_point(|&y| y <= max_y);

            if min_x_idx >= xs.len() || max_x_idx == 0 || min_y_idx >= ys.len() || max_y_idx == 0 {
                break;
            }

            min_x = xs[min_x_idx];
            max_x = xs[max_x_idx - 1];
            min_y = ys[min_y_idx];
            max_y = ys[max_y_idx - 1];

            // Reachable when both boundaries on an axis have trimmed past every populated
            // coordinate between them, which a very small dimension cap can force.
            if max_x < min_x || max_y < min_y {
                break;
            }

            if !region_exceeds_limits(min_x, min_y, max_x, max_y, limits) {
                break;
            }

            // Count populated cells on each boundary within current bounds.
            let count_min_x = count_boundary_x(&cells_by_x, min_x, min_y, max_y);
            let count_max_x = count_boundary_x(&cells_by_x, max_x, min_y, max_y);
            let count_min_y = count_boundary_y(&cells_by_y, min_y, min_x, max_x);
            let count_max_y = count_boundary_y(&cells_by_y, max_y, min_x, max_x);

            // Pick the boundary with fewest populated cells.
            // Tie-break: min_x, max_x, min_y, max_y (fixed order for determinism).
            let candidates = [
                (count_min_x, BoundarySide::MinX),
                (count_max_x, BoundarySide::MaxX),
                (count_min_y, BoundarySide::MinY),
                (count_max_y, BoundarySide::MaxY),
            ];
            let (_, side) = candidates.iter().min_by_key(|(count, _)| *count).unwrap();

            match side {
                BoundarySide::MinX => min_x += 1,
                BoundarySide::MaxX => max_x -= 1,
                BoundarySide::MinY => min_y += 1,
                BoundarySide::MaxY => max_y -= 1,
            }
        }

        // A region that retains nothing is not a salvage. Leave the map untouched so
        // `validate_control_texture_region` fails with its full diagnostics rather than
        // publishing a world with no terrain at all.
        let retains_any = self
            .terrain_cells
            .keys()
            .any(|&(cx, cy)| cx >= min_x && cx <= max_x && cy >= min_y && cy <= max_y);
        if !retains_any {
            return;
        }

        let mut dropped_cells: Vec<(i32, i32)> = Vec::new();
        self.terrain_cells.retain(|&(cx, cy), _| {
            let keep = cx >= min_x && cx <= max_x && cy >= min_y && cy <= max_y;
            if !keep {
                dropped_cells.push((cx, cy));
            }
            keep
        });

        if dropped_cells.is_empty() {
            return;
        }

        // Sort dropped cells for deterministic reporting.
        dropped_cells.sort_unstable();

        // Tighten to the surviving cells so the reported region and the reference clip match
        // the terrain that actually remains.
        let (min_x, min_y, max_x, max_y) = terrain_cell_bounds(&self.terrain_cells);
        self.clip_exterior_references_to_region(min_x, min_y, max_x, max_y);

        self.terrain_control_clip = Some(TerrainControlClip {
            retained_min: [min_x, min_y],
            retained_max: [max_x, max_y],
            dropped_cells,
        });
    }

    /// Re-applies a previously computed terrain control clip to exterior references only.
    ///
    /// Called after the grass merge to remove grass placements in the clipped region.
    pub(crate) fn reapply_terrain_control_clip(&mut self) {
        let Some(clip) = &self.terrain_control_clip else {
            return;
        };
        let ([min_x, min_y], [max_x, max_y]) = (clip.retained_min, clip.retained_max);
        self.clip_exterior_references_to_region(min_x, min_y, max_x, max_y);
    }

    /// Removes exterior references whose cell coordinates fall outside the given bounds.
    fn clip_exterior_references_to_region(&mut self, min_x: i32, min_y: i32, max_x: i32, max_y: i32) {
        if let Some(references) = self.cells.get_mut("\0") {
            references.retain(|_, reference| {
                let (cx, cy) = reference.cell_coords();
                cx >= min_x && cx <= max_x && cy >= min_y && cy <= max_y
            });
        }
    }

    /// Discard interior cells that do not meet MGE-XE's inclusion criteria.
    ///
    /// This is done after the merge phase to ensure all references from all plugins
    /// are considered when calculating the cell's spatial span.
    ///
    pub(crate) fn filter_interiors(&mut self, args: &UsageFilterOptions, overrides: &StaticOverrides) {
        self.cells.retain(|name, references| {
            if name == "\0" {
                return true;
            }

            if let Some(&enabled) = overrides.interiors.get(name.as_uncased()) {
                return enabled;
            }

            let Some(metadata) = self.interior_metadata.get(name.as_uncased()) else {
                return false;
            };

            if args.include_interiors_with_water && metadata.has_water {
                return true;
            }

            if args.include_behaves_like_exterior && metadata.behaves_like_exterior {
                return true;
            }

            if args.include_large_interiors && is_large_interior_refs(references) {
                return true;
            }

            false
        });
    }

    /// Normalizes reference IDs to their underlying mesh paths and applies visibility overrides.
    pub(crate) fn remap_references(&mut self, args: &UsageFilterOptions, overrides: &StaticOverrides) {
        debug_assert!(!self.released, "reference remap after post-fingerprint release");
        let objects = &self.objects;
        let name_overrides = &overrides.names;
        let reference_sources = self.reference_sources.clone();
        // Collected for the generation report. A `BTreeSet` so the listed ids are sorted and
        // deduplicated regardless of cell traversal order.
        let mut excluded_targets: BTreeSet<String> = BTreeSet::new();
        let mut excluded_target_references = 0_usize;

        for (cell_name, references) in &mut self.cells {
            references.retain(|reference_key, reference| {
                if reference.deleted {
                    return false;
                }

                let Some(object) = objects.get(reference.id.as_ref()) else {
                    return false;
                };

                if object.ignore_by_default && !object.force_mesh_generation {
                    return false;
                }

                // Rule B. `X->Disable` resolves through Morrowind's records handler, which only
                // holds references loaded from a plugin's persistent block, and
                // `findFirstReferenceById` returns one match. Therefore this reaches persistent
                // references only, and never the base object.
                if args.exclude_script_disable_targets
                    && object.disable_target
                    && reference.persistent
                    && !object.force_mesh_generation
                {
                    // Still the object id here; the mesh remap only happens on the way out.
                    excluded_targets.insert(reference.id.as_ref().to_owned());
                    excluded_target_references += 1;
                    return false;
                }

                if object.vis_index == 0 && name_overrides.get(reference.id.as_ref()).copied() == Some(false) {
                    return false;
                }

                if grass_density_should_cull(
                    cell_name,
                    &reference_sources,
                    *reference_key,
                    reference,
                    object,
                    args,
                    overrides,
                ) {
                    return false;
                }

                reference.id = Cow::Borrowed(object.mesh);
                reference.vis_index = object.vis_index;
                true
            });
        }

        self.script_disable.references_excluded_as_disable_targets = excluded_target_references;
        self.script_disable.excluded_disable_targets = excluded_targets.into_iter().collect();
    }

    /// Discard references to objects that do not exist in the given distant statics list.
    ///
    /// Generally this means those objects that were too small, or had no geometry, etc.
    ///
    #[tracing::instrument(skip_all)]
    pub fn discard_unused_references(&mut self, contains_static: impl Fn(&str) -> bool) {
        let span = info_span!(
            "usage.discard_unused_references",
            report = true,
            reference_count = tracing::field::Empty,
            exterior_reference_count = tracing::field::Empty
        );
        let _guard = span.enter();
        let count = self.total_references_count();
        info!("Discarding references to objects that do not exist in distant statics list...");
        for references in self.cells.values_mut() {
            references.retain(|_, reference| contains_static(reference.id.as_ref()));
        }
        let num_removed = count - self.total_references_count();
        info!("  Removed {num_removed} references");
        span.record("reference_count", self.total_references_count() as u64);
        span.record("exterior_reference_count", self.exterior_references_count() as u64);
    }

    /// Discard non-grass references that are deep under water. Checks if the
    /// (transformed) bounding box maximum Z is more than `deep_water_cull_depth` units below the applicable
    /// water level. Grass remains visible for underwater rendering.
    ///
    /// Exterior uses Morrowind's sea level (0.0). Interior water cells use the
    /// per-cell water height stored during plugin parsing.
    ///
    #[tracing::instrument(skip_all)]
    pub fn discard_deep_water_references<'b>(
        &mut self,
        deep_water_cull_depth: f32,
        static_info: impl Fn(&str) -> Option<(StaticType, &'b BoundingBox)>,
    ) {
        let span = info_span!(
            "usage.discard_deep_water_references",
            report = true,
            reference_count = tracing::field::Empty,
            exterior_reference_count = tracing::field::Empty
        );
        let _guard = span.enter();
        let count = self.total_references_count();

        info!("Discarding references that are deep under water...");

        // Exterior water level is 0.0 (Morrowind sea level)
        let exterior_threshold = -deep_water_cull_depth;
        self.exterior_references_mut().retain(|_, reference| {
            static_info(reference.id.as_ref()).is_some_and(|(static_type, bounds)| {
                static_type == StaticType::StaticGrass || reference.world_max_z(bounds) >= exterior_threshold
            })
        });

        // Interior cells with water each has its own stored water height
        for (name, references) in self.cells.iter_mut() {
            let Some(metadata) = self.interior_metadata.get(name.as_uncased()) else {
                continue;
            };
            if !metadata.has_water {
                continue;
            }
            let threshold = metadata.water_height - deep_water_cull_depth;
            references.retain(|_, reference| {
                static_info(reference.id.as_ref()).is_some_and(|(static_type, bounds)| {
                    static_type == StaticType::StaticGrass || reference.world_max_z(bounds) >= threshold
                })
            });
        }

        let num_removed = count - self.total_references_count();

        info!("  Removed {num_removed} references");
        span.record("reference_count", self.total_references_count() as u64);
        span.record("exterior_reference_count", self.exterior_references_count() as u64);
    }

    /// Discards exterior references whose geometry is mostly buried by surrounding terrain.
    ///
    /// Returns the aggregate outcome and work tally for the references considered.
    #[tracing::instrument(skip_all)]
    pub fn discard_low_visibility_references(
        &mut self,
        static_type: impl Sync + Fn(&str) -> Option<StaticType>,
        is_buried: impl Sync + Fn(&TerrainCells<'_>, &DistantReference<'_>, &mut BurialStats) -> bool,
    ) -> BurialStats {
        let span = info_span!(
            "usage.discard_low_visibility_references",
            report = true,
            reference_count = tracing::field::Empty,
            exterior_reference_count = tracing::field::Empty,
            burial_refs_considered = tracing::field::Empty,
            burial_keep_clearance_shortcut = tracing::field::Empty,
            burial_keep_height_early = tracing::field::Empty,
            burial_keep_insufficient = tracing::field::Empty,
            burial_keep_exposed = tracing::field::Empty,
            burial_buried = tracing::field::Empty,
            burial_tris_visited = tracing::field::Empty,
            burial_centroid_height_samples = tracing::field::Empty
        );
        let _guard = span.enter();
        let count = self.exterior_references_count();

        info!("Discarding references that are mostly buried in terrain...");

        let terrain_cells = &self.terrain_cells;

        let Some(exterior) = self.cells.get("\0") else {
            return BurialStats::default();
        };

        // Run the buried heuristic once per exterior reference in parallel, collecting both the
        // keys to remove and the corresponding `BurialStats` tally. Stats are folded per worker and
        // reduced into one total so the instrumentation adds no atomics to the hot path.
        let (remove_vec, stats) = exterior
            .par_iter()
            .map(|(key, reference)| {
                let mut stats = BurialStats::default();
                // Grass sits flush on the terrain and is short by nature, so the buried-geometry
                // heuristic would wrongly cull almost all of it. Exempt grass, matching the
                // min-radius (see `passes_min_radius`), merge, and atlas exemptions elsewhere.
                let removed = match static_type(reference.id.as_ref()) {
                    Some(static_type) if static_type != StaticType::StaticGrass => {
                        is_buried(terrain_cells, reference, &mut stats).then_some(*key)
                    }
                    _ => None,
                };
                (removed, stats)
            })
            .fold(
                || (Vec::<StableRefKey>::new(), BurialStats::default()),
                |(mut keys, mut acc), (removed, stats)| {
                    if let Some(key) = removed {
                        keys.push(key);
                    }
                    acc.merge(stats);
                    (keys, acc)
                },
            )
            .reduce(
                || (Vec::new(), BurialStats::default()),
                |(mut keys, mut acc), (mut other_keys, other)| {
                    keys.append(&mut other_keys);
                    acc.merge(other);
                    (keys, acc)
                },
            );
        let remove_keys: HashSet<StableRefKey> = remove_vec.into_iter().collect();

        if !remove_keys.is_empty() {
            self.exterior_references_mut().retain(|key, _| !remove_keys.contains(key));
        }

        let num_removed = count - self.exterior_references_count();
        info!("  Removed {num_removed} references");
        span.record("reference_count", self.total_references_count() as u64);
        span.record("exterior_reference_count", self.exterior_references_count() as u64);
        span.record("burial_refs_considered", stats.refs_considered);
        span.record("burial_keep_clearance_shortcut", stats.keep_clearance_shortcut);
        span.record("burial_keep_height_early", stats.keep_height_early);
        span.record("burial_keep_insufficient", stats.keep_insufficient);
        span.record("burial_keep_exposed", stats.keep_exposed);
        span.record("burial_buried", stats.buried);
        span.record("burial_tris_visited", stats.tris_visited);
        span.record("burial_centroid_height_samples", stats.centroid_height_samples);
        stats
    }
}

/// Returns `true` if `reference` should be culled based on the effective grass density.
///
/// A density of 0.0 culls the object unconditionally; 1.0 keeps it unconditionally.
/// Intermediate values use a deterministic pseudo-random sample derived from the
/// reference identity to produce stable, order-independent thinning across runs.
fn grass_density_should_cull(
    cell_name: &str,
    reference_sources: &ReferenceSources,
    reference_key: StableRefKey,
    reference: &DistantReference<'_>,
    object: &ObjectDefinition<'_>,
    args: &UsageFilterOptions,
    overrides: &StaticOverrides,
) -> bool {
    let Some(density) = grass_density_for_object(object, args, overrides) else {
        return false;
    };

    if density <= 0.0 {
        return true;
    }
    if density >= 1.0 {
        return false;
    }

    let source_name = reference_sources
        .name(reference_key.source())
        .expect("parsed grass references have an interned source filename");
    grass_density_sample(cell_name, source_name, reference_key, reference) >= density
}

/// Returns the effective grass density for `object`, or `None` if the object is not grass.
///
/// An object is considered grass when its mesh path begins with `"grass\\"` or when
/// the static override for that mesh specifies `StaticType::StaticGrass`.  The density
/// comes from the mesh-level override when present (and non-negative), otherwise from
/// the global `args.grass_density` setting.
pub(super) fn grass_density_for_object(
    object: &ObjectDefinition<'_>,
    args: &UsageFilterOptions,
    overrides: &StaticOverrides,
) -> Option<f32> {
    let mesh_override = overrides.mesh_overrides.get(object.mesh);
    let is_grass = object.mesh.starts_with("grass\\")
        || mesh_override
            .map(|mesh_override| matches!(mesh_override.static_type, StaticType::StaticGrass))
            .unwrap_or(false);

    if !is_grass {
        return None;
    }

    Some(
        mesh_override
            .and_then(|mesh_override| (mesh_override.density >= 0.0).then_some(mesh_override.density))
            .unwrap_or(args.grass_density),
    )
}

/// Derives a stable pseudo-random sample in [0, 1) from the reference's identity.
///
/// The Blake3 hash of `cell_name` + normalized source filename + source-local index +
/// translation XYZ bits is used so the result is deterministic across runs and independent of
/// global load-order position or iteration order.
/// Grass thinning culls the reference when the sample is ≥ the target density.
fn grass_density_sample(
    cell_name: &str,
    source_name: &str,
    reference_key: StableRefKey,
    reference: &DistantReference<'_>,
) -> f32 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(cell_name.as_bytes());
    hasher.update(source_name.as_bytes());
    hasher.update(&reference_key.index().to_le_bytes());
    hasher.update(&reference.translation.x.to_bits().to_le_bytes());
    hasher.update(&reference.translation.y.to_bits().to_le_bytes());
    hasher.update(&reference.translation.z.to_bits().to_le_bytes());

    let hash = hasher.finalize();
    let sample = u64::from_le_bytes(hash.as_bytes()[..8].try_into().expect("slice with exact length"));
    ((sample as f64) / (u64::MAX as f64)) as f32
}

/// Returns true if the bounding span of all references in an interior cell is >= 10,000 units.
fn is_large_interior_refs(references: &References<'_>) -> bool {
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    for reference in references.values() {
        let pos = reference.translation;
        min = min.min(pos);
        max = max.max(pos);
    }
    (max - min).max_element() >= 10_000.0
}

/// Boundary side used as tie-break discriminant in the greedy shrink.
#[derive(Clone, Copy)]
enum BoundarySide {
    MinX,
    MaxX,
    MinY,
    MaxY,
}

/// Returns `(min_x, min_y, max_x, max_y)` for the terrain cell bounding rectangle.
fn terrain_cell_bounds(terrain_cells: &TerrainCells<'_>) -> (i32, i32, i32, i32) {
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    for &(cx, cy) in terrain_cells.keys() {
        min_x = min_x.min(cx);
        min_y = min_y.min(cy);
        max_x = max_x.max(cx);
        max_y = max_y.max(cy);
    }
    (min_x, min_y, max_x, max_y)
}

/// Checks whether the bounding rectangle exceeds either the dimension or byte limit.
fn region_exceeds_limits(min_x: i32, min_y: i32, max_x: i32, max_y: i32, limits: &TerrainControlClipLimits) -> bool {
    if max_x < min_x || max_y < min_y {
        return false;
    }
    let width = (max_x - min_x + 1) as u32;
    let height = (max_y - min_y + 1) as u32;
    if width > limits.max_dimension_cells || height > limits.max_dimension_cells {
        return true;
    }
    let material_w = u64::from(width) * u64::from(MATERIAL_PATCHES_PER_CELL);
    let material_h = u64::from(height) * u64::from(MATERIAL_PATCHES_PER_CELL);
    estimated_control_texture_bytes(material_w, material_h) > limits.max_bytes
}

/// Estimates the control-texture byte footprint for a given material size.
///
/// Mirrors the terrain crate's `estimated_control_texture_bytes`: two RGBA8 control maps
/// plus a BC1 patch-albedo mip chain.
fn estimated_control_texture_bytes(width: u64, height: u64) -> u64 {
    let area = width * height;
    // Two RGBA8 maps = area * 4 * 2 = area * 8
    let rgba_bytes = area * 8;
    // BC1 mip chain: sum of ceil(w/4)*ceil(h/4)*8 over all levels
    let bc1_bytes = bc1_mip_chain_size(width as u32, height as u32);
    rgba_bytes + bc1_bytes
}

/// Computes the total byte size of a BC1 mip chain down to 1×1 using `distantland_texture` primitives.
fn bc1_mip_chain_size(mut width: u32, mut height: u32) -> u64 {
    let mut total = 0_u64;
    loop {
        total += distantland_texture::dds::bcn_level_bytes(width, height, 8) as u64;
        if width == 1 && height == 1 {
            break;
        }
        (width, height) = distantland_texture::dds::next_mip_dimensions(width, height);
    }
    total
}

/// Counts populated terrain cells on a column boundary (fixed x, varying y).
fn count_boundary_x(cells_by_x: &hashbrown::HashMap<i32, Vec<i32>>, x: i32, min_y: i32, max_y: i32) -> usize {
    cells_by_x
        .get(&x)
        .map_or(0, |ys| ys.iter().filter(|&&y| y >= min_y && y <= max_y).count())
}

/// Counts populated terrain cells on a row boundary (fixed y, varying x).
fn count_boundary_y(cells_by_y: &hashbrown::HashMap<i32, Vec<i32>>, y: i32, min_x: i32, max_x: i32) -> usize {
    cells_by_y
        .get(&y)
        .map_or(0, |xs| xs.iter().filter(|&&x| x >= min_x && x <= max_x).count())
}
