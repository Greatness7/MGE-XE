use std::collections::BTreeSet;

use super::*;

impl<'a> UsageInfo<'a> {
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
