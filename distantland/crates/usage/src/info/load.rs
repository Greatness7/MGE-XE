use crate::default_land_texture_key;
use crate::vfs::make_normalized;

use super::*;

use distantland_foundation::identity::FileIdentity;

/// Usage-loading result with plugin identities, warnings, and a caller-defined pre-filter capture.
pub type GrassSetupCaptureResult<'a, T> = (UsageInfo<'a>, Vec<FileIdentity>, Vec<FileIdentity>, Vec<UsageWarning>, T);

/// Result of parsing one plugin while retaining identity for every successfully read file.
pub(crate) struct PluginUsage<'a> {
    /// Content identity captured before parsing.
    pub(crate) identity: Option<FileIdentity>,
    /// Distant-land data, absent when parsing or load-order remapping rejected the plugin.
    pub(crate) usage_info: Option<UsageInfo<'a>>,
}

impl<'a> UsageInfo<'a> {
    /// Bootstraps usage data while observing the merged main load order before filtering.
    pub fn setup_with_grass_plugins_and_capture<T>(
        vfs: &'a Vfs,
        grass_plugins: &[PathBuf],
        args: &UsageFilterOptions,
        overrides: &StaticOverrides,
        capture: impl FnOnce(&UsageInfo<'a>) -> T,
    ) -> anyhow::Result<GrassSetupCaptureResult<'a, T>> {
        let reference_sources = ReferenceSources::from_plugin_lists(vfs.active_plugins(), grass_plugins);
        let (mut usage_info, plugin_identities, capture) =
            UsageInfo::from_load_order_with_identities_and_sources_and_capture(
                vfs,
                vfs.active_plugins(),
                args,
                overrides,
                &reference_sources,
                capture,
            );
        let grass = load_grass_plugins(vfs, grass_plugins, &usage_info.objects, args, overrides, &reference_sources)?;
        usage_info.merge(grass.usage_info);
        usage_info.finalize();
        Ok((usage_info, plugin_identities, grass.identities, grass.warnings, capture))
    }

    /// # Panics
    ///
    /// Panics if no valid plugins could be successfully loaded from the provided load order.
    pub fn from_load_order(
        vfs: &'a Vfs,
        load_order: &[PathBuf],
        args: &UsageFilterOptions,
        overrides: &StaticOverrides,
    ) -> UsageInfo<'a> {
        let reference_sources = ReferenceSources::from_load_order(load_order);
        let (mut usage_info, _identities, ()) = Self::from_load_order_with_identities_and_sources_and_capture(
            vfs,
            load_order,
            args,
            overrides,
            &reference_sources,
            |_| (),
        );
        usage_info.finalize();
        usage_info
    }

    /// Parses, merges, and filters a load order, returning **unfinalized** usage.
    ///
    /// The returned state has no derived mesh maps and no deterministic ordering; every caller must
    /// end with [`UsageInfo::finalize`], after any further merge of its own. Deferring that lets the
    /// grass path merge its dedicated list before the single finalization rather than after a first
    /// one the merge would immediately invalidate.
    fn from_load_order_with_identities_and_sources_and_capture<T>(
        vfs: &'a Vfs,
        load_order: &[PathBuf],
        args: &UsageFilterOptions,
        overrides: &StaticOverrides,
        reference_sources: &ReferenceSources,
        capture: impl FnOnce(&UsageInfo<'a>) -> T,
    ) -> (UsageInfo<'a>, Vec<FileIdentity>, T) {
        let mut plugin_identities = Vec::with_capacity(load_order.len());
        let mut this = {
            let span = info_span!(
                "usage.parse_and_merge_plugins",
                report = true,
                plugin_count = load_order.len() as u64,
                object_count = tracing::field::Empty,
                reference_count = tracing::field::Empty,
                exterior_reference_count = tracing::field::Empty,
                landscape_cell_count = tracing::field::Empty,
                terrain_cell_count = tracing::field::Empty,
            );
            let _guard = span.enter();

            let plugin_results: Vec<_> = load_order
                .par_iter()
                .map(|path| UsageInfo::from_path_with_identity_and_sources(path, vfs, args, overrides, reference_sources))
                .collect();

            let mut usage_infos = plugin_results //
                .into_iter()
                .filter_map(|result| {
                    if let Some(identity) = result.identity {
                        plugin_identities.push(identity);
                    }
                    result.usage_info
                });

            let mut this = usage_infos //
                .next()
                .expect("No valid plugins in load order");

            for other in usage_infos {
                this.merge(other);
            }

            // Script classification is resolved here, not in `from_plugin`: the script set is only
            // complete once every plugin has been merged. This must stay ahead of `capture`, which
            // projects `ObjectDefinition` into the statics domain digest.
            this.classify_script_disables();

            span.record("object_count", this.objects.len() as u64);
            span.record("reference_count", this.total_references_count() as u64);
            span.record("exterior_reference_count", this.exterior_references_count() as u64);
            // Both counters report the sole LAND store so existing observability fields stay populated.
            span.record("landscape_cell_count", this.terrain_cells.len() as u64);
            span.record("terrain_cell_count", this.terrain_cells.len() as u64);

            this
        };

        let capture = capture(&this);

        {
            let span = info_span!(
                "usage.remap_references",
                report = true,
                object_count = tracing::field::Empty,
                reference_count = tracing::field::Empty,
                exterior_reference_count = tracing::field::Empty
            );
            let _guard = span.enter();

            this.remap_references(args, overrides);
            this.cells.retain(|_, references| !references.is_empty());
            span.record("object_count", this.objects.len() as u64);
            span.record("reference_count", this.total_references_count() as u64);
            span.record("exterior_reference_count", this.exterior_references_count() as u64);
        }

        {
            let span = info_span!(
                "usage.filter_interiors",
                report = true,
                reference_count = tracing::field::Empty,
                exterior_reference_count = tracing::field::Empty,
                terrain_cell_count = tracing::field::Empty
            );
            let _guard = span.enter();

            this.filter_interiors(args, overrides);
            span.record("reference_count", this.total_references_count() as u64);
            span.record("exterior_reference_count", this.exterior_references_count() as u64);
            span.record("terrain_cell_count", this.terrain_cells.len() as u64);
        }

        (this, plugin_identities, capture)
    }

    /// Runs once per load, after the last merge: [`Self::calc_mesh_scale_maximums`] clears and
    /// rebuilds `mesh_scale_maximums`/`forced_meshes` from the final reference set, and the sort
    /// is what keeps parallel traversal order out of the output.
    fn finalize(&mut self) {
        {
            let span = info_span!(
                "usage.calc_mesh_scale_maximums",
                report = true,
                mesh_scale_count = tracing::field::Empty
            );
            let _guard = span.enter();
            self.calc_mesh_scale_maximums();
            span.record("mesh_scale_count", self.mesh_scale_maximums.len() as u64);
        }

        self.sort_for_deterministic_output();
    }

    /// Calculates the maximum scale for each mesh so references below the minimum size threshold
    /// can be omitted even at their largest scale.
    ///
    /// This is also the sole writer of `forced_meshes`, and deliberately so: it runs from
    /// [`Self::finalize`], after the grass list has been merged, whereas `remap_references` runs
    /// before that merge and so cannot see grass references at all. Both fields are therefore
    /// derived from the same final reference set, and neither survives an earlier pass.
    fn calc_mesh_scale_maximums(&mut self) {
        debug_assert!(!self.released, "mesh scale recalculation after post-fingerprint release");
        self.mesh_scale_maximums.clear();
        self.forced_meshes.clear();
        self.mesh_scale_maximums.reserve(self.objects.len());
        self.forced_meshes.reserve(self.objects.len());

        for reference in self.cells.values().flat_map(|references| references.values()) {
            let Cow::Borrowed(mesh_key) = &reference.id else {
                continue;
            };
            let max_scale = self.mesh_scale_maximums.entry(*mesh_key).or_default();
            *max_scale = reference.scale.max(*max_scale);
        }

        for object in self.objects.values() {
            if object.force_mesh_generation && self.mesh_scale_maximums.contains_key(object.mesh) {
                self.forced_meshes.insert(object.mesh);
            }
        }
    }

    /// Parses one plugin and captures the bytes' identity before decoding them.
    pub(super) fn from_path_with_identity_and_sources(
        path: &Path,
        vfs: &'a Vfs,
        args: &UsageFilterOptions,
        overrides: &StaticOverrides,
        reference_sources: &ReferenceSources,
    ) -> PluginUsage<'a> {
        use tes3::esp::*;

        let filter = |tag| {
            // Records parsed for distant land: base object geometry + placement cells.
            // LAND/LTEX drive terrain; SCPT detects "Disable" calls to filter dynamic objects.
            // (OpenCS occasionally emits a "GCVR" record which is intentionally excluded.)
            matches!(
                &tag,
                b"TES3" | b"STAT" | b"ACTI" | b"DOOR" | b"CONT" | b"LIGH" | b"CELL" | b"SCPT" | b"LAND" | b"LTEX"
            )
        };

        let Ok(bytes) = std::fs::read(path) else {
            info!("Failed to load plugin: {}", path.display());
            return PluginUsage {
                identity: None,
                usage_info: None,
            };
        };
        let identity = FileIdentity::from_bytes(path, &bytes);
        let mut plugin = Plugin::new();
        if plugin.load_bytes_filtered(&bytes, filter).is_err() {
            info!("Failed to load plugin: {}", path.display());
            return PluginUsage {
                identity: Some(identity),
                usage_info: None,
            };
        }

        let usage_info = Self::from_plugin(path, vfs, args, overrides, reference_sources, plugin);

        PluginUsage {
            identity: Some(identity),
            usage_info,
        }
    }

    fn from_plugin(
        path: &Path,
        vfs: &'a Vfs,
        args: &UsageFilterOptions,
        overrides: &StaticOverrides,
        reference_sources: &ReferenceSources,
        mut plugin: tes3::esp::Plugin,
    ) -> Option<UsageInfo<'a>> {
        use tes3::esp::*;

        let Some(header) = plugin.header_mut() else {
            info!("Plugin missing header: {}", path.display());
            return None;
        };

        // `index_remap[i]` maps plugin-relative master index i to the stable source filename.
        // Index 0 is this plugin itself; indices 1..=N are its declared masters.
        let mut index_remap = Vec::with_capacity(header.masters.len() + 1);
        index_remap.push(reference_sources.source_id(path)?);
        for (path, _) in &header.masters {
            index_remap.push(reference_sources.source_id(path)?);
        }

        // Collected per plugin but consumed load-order-wide: `merge` unions these, and
        // `classify_script_disables` applies them once every plugin has been folded in.
        let mut disable_scripts: HashSet<UString> = HashSet::new();
        let mut disable_targets: HashSet<UString> = HashSet::new();
        for script in plugin.extract_objects_of_type::<Script>() {
            let facts = super::script::scan_script(script.text);
            if facts.disables_self {
                disable_scripts.insert(script.id.into());
            }
            disable_targets.extend(facts.disable_targets.into_iter().map(UString::new));
        }

        let mut info = UsageInfo {
            reference_sources: reference_sources.clone(),
            disable_scripts,
            disable_targets,
            ..UsageInfo::default()
        };

        for object in plugin.objects.iter_mut() {
            #[rustfmt::skip]
            let (kind, id, mesh, script) = match object {
                TES3Object::Static(object) => {
                    (ObjectKind::Static, &mut object.id, &mut object.mesh, None)
                },
                TES3Object::Activator(object) => {
                    (ObjectKind::Activator, &mut object.id, &mut object.mesh, Some(&mut object.script))
                },
                TES3Object::Door(object) => {
                    (ObjectKind::Door, &mut object.id, &mut object.mesh, Some(&mut object.script))
                }
                TES3Object::Container(object) => {
                    (ObjectKind::Misc, &mut object.id, &mut object.mesh, Some(&mut object.script))
                }
                TES3Object::Light(object) => {
                    (ObjectKind::Misc, &mut object.id, &mut object.mesh, Some(&mut object.script))
                }
                _ => continue,
            };

            // Normalize this now so we never have to allocate anywhere else across the pipeline.
            make_normalized(mesh);

            let Some(mesh_key) = vfs.resolve_model_mesh_key(mesh) else {
                continue;
            };

            if matches!(kind, ObjectKind::Door) {
                info.door_meshes.insert(mesh_key);
            }

            let script_id = script.map_or("", |script| {
                script.make_ascii_lowercase();
                script.as_str()
            });

            let mut id = std::mem::take(id);
            id.make_ascii_lowercase();

            if let Some(definition) = Self::build_object_definition(&id, script_id, mesh_key, kind, args, overrides) {
                info.objects.insert(id, definition);
            }
        }

        // Build LTEX table for this plugin. LTEX index n is stored as key n (raw INTV value).
        // Lookup from VTEX raw index r uses key r-1.
        let mut texture_table: IndexMap<_, _> = plugin
            .objects_of_type_mut::<LandscapeTexture>()
            .map(|tex| {
                // Normalize this now so we never have to allocate anywhere else across the pipeline.
                make_normalized(&mut tex.file_name);
                let path = vfs
                    .resolve_texture_key(&tex.file_name)
                    .unwrap_or(default_land_texture_key(vfs));
                (tex.index as u16, path)
            })
            .collect();
        texture_table.sort_unstable_keys();

        let texture_table = std::sync::Arc::new(texture_table);

        for landscape in plugin.objects_of_type::<Landscape>() {
            let grid = landscape.grid;
            // Match MGE-XE: skip if DELETED, or if flags are explicitly set but
            // the vertex-heights bit is not. When `landscape_flags` is empty we
            // treat it as "DATA subrecord absent" and keep the record (MGE-XE
            // initializes its `usesVertexHeights` flag to true).
            let flags = landscape.landscape_flags;
            if landscape.flags.contains(ObjectFlags::DELETED)
                || (!flags.is_empty() && !flags.contains(LandscapeFlags::USES_VERTEX_HEIGHTS_AND_NORMALS))
            {
                info.terrain_cells.shift_remove(&grid);
            } else {
                // Heights are decoded once inside `from_landscape` and stored only on TerrainCell.
                info.terrain_cells.insert(
                    grid,
                    TerrainCell::from_landscape(landscape, std::sync::Arc::clone(&texture_table)),
                );
            }
        }

        // TODO: Depending on the order we load cells, we may need to handle
        // this differently. Cell water height is optional, if absent the engine
        // uses the height from the previous definition in load order.
        // Counts new exterior references whose `refr_index` was already used by an earlier cell of
        // this same plugin. A well-formed plugin numbers references uniquely across its own cells,
        // so any collision means the file is malformed and placements are being lost.
        let mut colliding_reference_count: u64 = 0;
        for cell in plugin.into_objects_of_type::<Cell>() {
            if cell.is_interior() {
                info.interior_metadata.insert(
                    cell.name.clone().into(),
                    InteriorMetadata {
                        behaves_like_exterior: cell.data.flags.contains(CellFlags::BEHAVES_LIKE_EXTERIOR),
                        has_water: cell.data.flags.contains(CellFlags::HAS_WATER),
                        water_height: cell.water_height.unwrap_or(0.0),
                    },
                );
            }

            if cell.references.is_empty() {
                continue;
            }

            // All exterior references share a "cell". Interiors are separate world spaces.
            let references = if cell.is_exterior() {
                info.exterior_references_mut()
            } else {
                info.cells.entry(cell.name).or_default() // Casing sensitive as per MGE-XE.
            };

            // `cell.references` is keyed by `(mast_index, refr_index)`, so a collision can only come
            // from another cell of this plugin. Iteration order is therefore irrelevant to the
            // resulting key set, and `sort_for_deterministic_output` orders the map before output.
            for ((mast_index, refr_index), mut reference) in cell.references {
                let Some(&source) = index_remap.get(mast_index as usize) else {
                    continue;
                };
                reference.id.make_ascii_lowercase();

                let key = StableRefKey::new(source, refr_index);
                if mast_index == 0 && references.contains_key(&key) {
                    colliding_reference_count += 1;
                }

                references.insert(
                    key,
                    DistantReference {
                        // The accessor, not the raw `temporary` field: moved references and
                        // teleport doors are persistent regardless of which block they sit in.
                        persistent: reference.persistent(),
                        id: Cow::Owned(reference.id),
                        deleted: reference.deleted.is_some(),
                        translation: reference.translation.into(),
                        rotation: reference.rotation.into(),
                        scale: reference.scale.unwrap_or(1.0),
                        vis_index: 0,
                    },
                );
            }
        }

        if colliding_reference_count != 0 {
            warn!(
                "Discarded {colliding_reference_count} placement(s) in {}: the plugin reuses reference \
                 indices across its cells, which is malformed. Groundcover plugins commonly do this. \
                 List it under grass_plugins instead of the load order to keep every placement.",
                path.display()
            );
        }

        Some(info)
    }

    /// Constructs metadata rules for an object, factoring in overrides.
    ///
    /// Script classification is deliberately *not* resolved here. The effective script id is only
    /// recorded, and [`UsageInfo::classify_script_disables`] applies it once the whole load order
    /// has been merged.
    pub(crate) fn build_object_definition(
        object_id: &str,
        mut script_id: &str,
        mesh: &'a str,
        kind: ObjectKind,
        args: &UsageFilterOptions,
        overrides: &StaticOverrides,
    ) -> Option<ObjectDefinition<'a>> {
        if object_id.is_empty() {
            return None;
        }

        let mesh_override = overrides.mesh_overrides.get(mesh);
        let vis_index = overrides
            .dynamic_vis
            .scripts
            .get(script_id)
            .copied()
            .or_else(|| overrides.dynamic_vis.unique_objects.get(object_id).copied())
            .unwrap_or(0);

        if mesh_override.is_some_and(|ovr| ovr.no_script) {
            script_id = "";
        }
        // Empty ids borrow a `'static` literal, so the scriptless majority costs no allocation.
        let script_id = if script_id.is_empty() {
            UString::new("")
        } else {
            UString::new(script_id.to_owned())
        };

        if vis_index != 0 {
            return Some(ObjectDefinition {
                mesh,
                ignore_by_default: false,
                force_mesh_generation: true,
                vis_index,
                kind,
                script_id,
                script_disabled: false,
                disable_target: false,
            });
        }

        let ignored_by_kind = match kind {
            ObjectKind::Static => false,
            ObjectKind::Activator => !args.include_activators,
            // Doors are always included in distant land, like statics, regardless of `include_misc`.
            ObjectKind::Door => false,
            ObjectKind::Misc => !args.include_misc,
        };

        // An override entry replaces the kind check rather than adding to it, matching the legacy
        // generator: listing a mesh in an `.ovr` is the author stating whether it belongs in distant
        // land, so `l\Light_torch_01.nif=far` keeps lights even with `include_misc` off. A script
        // disable still wins, as it did in legacy. `classify_script_disables` folds it in later.
        let ignored = match mesh_override {
            Some(ovr) => ovr.ignore,
            None => ignored_by_kind,
        };

        let mut definition = ObjectDefinition {
            mesh,
            ignore_by_default: ignored,
            force_mesh_generation: false,
            vis_index: 0,
            kind,
            script_id,
            script_disabled: false,
            disable_target: false,
        };

        if let Some(true) = overrides.names.get(object_id) {
            definition.ignore_by_default = false;
            definition.force_mesh_generation = true;
        }

        Some(definition)
    }

    /// Applies the load-order-wide script classification to every merged object definition.
    ///
    /// Must run after the last [`UsageInfo::merge`] of the load order and before anything reads
    /// `ignore_by_default` or projects the definitions, because neither script set is complete
    /// until then.
    ///
    /// `force_mesh_generation` is the force-include escape hatch. It is set by a `dynamic_visibility` /
    /// `unique_object` group or an `include_objects` entry, so a script disable never overrides
    /// it. `script_disabled` is still recorded on those objects, matching the pre-merge behaviour
    /// this replaces.
    pub(crate) fn classify_script_disables(&mut self) {
        debug_assert!(!self.released, "script disable classification after post-fingerprint release");
        self.script_disable.self_disabling_scripts = self.disable_scripts.len();
        self.script_disable.disable_targets_named = self.disable_targets.len();
        if self.disable_scripts.is_empty() && self.disable_targets.is_empty() {
            return;
        }

        let (mut excluded_by_script, mut targets_resolved) = (0_usize, 0_usize);
        // Each definition is decided independently of the others, so the arbitrary `HashMap`
        // iteration order cannot leak into the result.
        for (object_id, definition) in &mut self.objects {
            // Rule A: the attached script disables whatever it is attached to, so every instance
            // goes. Folded into `ignore_by_default` here, as an object-level exclusion.
            if !definition.script_id.is_empty() && self.disable_scripts.contains(&definition.script_id) {
                definition.script_disabled = true;
                if !definition.force_mesh_generation {
                    definition.ignore_by_default = true;
                    excluded_by_script += 1;
                }
            }
            // Rule B: recorded only. `remap_references` applies it per reference, because one
            // `X->Disable` call reaches exactly one of them.
            definition.disable_target = self.disable_targets.contains(object_id.as_uncased());
            targets_resolved += usize::from(definition.disable_target);
        }

        self.script_disable.objects_excluded_by_script = excluded_by_script;
        self.script_disable.disable_targets_resolved = targets_resolved;
    }
}
