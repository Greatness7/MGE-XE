//! Distant-land projections captured from merged plugin data.
//!
//! Projections deliberately exclude source plugin bytes and run-local ordinals. They retain
//! normalized logical identities and raw float bit patterns, so unrelated records cannot dirty
//! distant-land work while every consumed input remains fingerprint-visible.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use itertools::Itertools;
use rayon::prelude::*;
use uncased::AsUncased;

use super::units::{CanonicalWrite, CanonicalWriter};
use crate::mge_xe::distant_statics::StaticType;
use crate::usage::ObjectKind;
use crate::vfs::normalize;
use crate::{
    DistantReference, DynamicVisKind, GenerationSettings, InteriorMetadata, ObjectDefinition, StaticOverride,
    StaticOverrides, UsageInfo,
};

/// Cell coordinates used to partition exterior reference and landscape projections.
pub(crate) type ProjectionCell = (i32, i32);

/// Complete snapshot used as the source for unit fingerprints.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Projections {
    /// Exterior references bucketed by their world-space cell.
    pub(crate) exterior_cells: BTreeMap<ProjectionCell, ReferenceCellProjection>,
    /// Interior reference sets keyed by normalized cell name.
    pub(crate) interiors: BTreeMap<String, InteriorCellProjection>,
    /// LAND projections keyed by absolute cell coordinates.
    pub(crate) landscapes: BTreeMap<ProjectionCell, LandscapeCellProjection>,
    /// Effective object definitions keyed by normalized object ID.
    pub(crate) objects: BTreeMap<String, ObjectDefinitionProjection>,
    /// Effective override and metadata state, partitioned by downstream ownership.
    pub(crate) overrides: OverrideStateProjection,
    /// Usage-filter settings whose effects are not all materialized at the pre-filter boundary.
    pub(crate) settings: UsageSettingsProjection,
    /// Actual door mesh set consumed by the current static extraction path.
    pub(crate) door_meshes: BTreeSet<String>,
    /// Dedicated grass placements, retained as statics-domain globals.
    pub(crate) grass_placements: Vec<GrassPlacementProjection>,
}

impl Projections {
    /// Captures the main load order before reference remapping or interior filtering.
    pub(crate) fn capture(usage: &UsageInfo<'_>, settings: &GenerationSettings, overrides: &StaticOverrides) -> Self {
        let mut exterior_groups: BTreeMap<_, Vec<_>> = BTreeMap::new();
        if let Some(references) = usage.cells.get("\0") {
            for (&key, reference) in references {
                exterior_groups
                    .entry(reference.cell_coords())
                    .or_default()
                    .push((key, reference));
            }
        }
        let exterior_cells = exterior_groups
            .into_iter()
            .collect_vec()
            .into_par_iter()
            .map(|(cell, references)| (cell, ReferenceCellProjection::capture(usage, references, settings, overrides)))
            .collect();

        let mut interior_groups: BTreeMap<String, (Option<InteriorMetadataProjection>, Vec<_>)> = BTreeMap::new();
        for (name, references) in &usage.cells {
            if name == "\0" {
                continue;
            }
            let entry = interior_groups.entry(name.to_ascii_lowercase()).or_default();
            entry.0 = usage
                .interior_metadata
                .get(name.as_uncased())
                .map(InteriorMetadataProjection::from);
            entry.1.extend(references.iter().map(|(&key, reference)| (key, reference)));
        }
        let interiors = interior_groups
            .into_iter()
            .collect_vec()
            .into_par_iter()
            .map(|(name, (metadata, references))| {
                let references = ReferenceCellProjection::capture(usage, references, settings, overrides);
                (
                    name,
                    InteriorCellProjection {
                        metadata,
                        reference_hash: references.reference_hash,
                        object_ids: references.object_ids,
                    },
                )
            })
            .collect();

        // Single LAND store: height_hash, terrain_hash, material_hash, and reverse texture
        // deps all derive from TerrainCell. Recipes stay independent (heights are not folded
        // into terrain_hash).
        let landscapes = usage
            .terrain_cells
            .iter()
            .collect_vec()
            .into_par_iter()
            .map(|(&cell, terrain)| {
                (
                    cell,
                    LandscapeCellProjection {
                        height_hash: Some(hash_heights(terrain.heights.as_ref())),
                        terrain_hash: Some(hash_terrain_cell(terrain)),
                        material_hash: Some(hash_terrain_material(terrain)),
                        referenced_textures: referenced_texture_keys(terrain),
                    },
                )
            })
            .collect();

        let objects = usage
            .objects
            .iter()
            .map(|(id, definition)| (id.to_ascii_lowercase(), ObjectDefinitionProjection::from(definition)))
            .collect();
        let door_meshes = usage.door_meshes.iter().map(|mesh| normalize(mesh).into_owned()).collect();

        Self {
            exterior_cells,
            interiors,
            landscapes,
            objects,
            overrides: OverrideStateProjection::from(overrides),
            settings: UsageSettingsProjection::from(settings),
            door_meshes,
            grass_placements: Vec::new(),
        }
    }

    /// Adds the already-classified dedicated grass placements from the final usage view.
    ///
    /// Interior placements are captured here too, carrying their cell name: the interior projections
    /// above were taken before the grass list merged, so this is the only place they are seen.
    pub(crate) fn capture_dedicated_grass(
        &mut self,
        usage: &UsageInfo<'_>,
        grass_plugins: &[PathBuf],
        settings: &GenerationSettings,
        overrides: &StaticOverrides,
    ) {
        let sources: BTreeSet<_> = grass_plugins.iter().filter_map(|path| normalized_filename(path)).collect();
        self.grass_placements = usage
            .cells
            .iter()
            .flat_map(|(cell_name, references)| {
                let interior_cell = (cell_name != "\0").then(|| cell_name.clone());
                references
                    .iter()
                    .map(move |(&key, reference)| (interior_cell.clone(), key, reference))
            })
            .filter_map(|(interior_cell, key, reference)| {
                let source = usage.reference_source_name(key.source())?;
                sources.contains(source).then(|| GrassPlacementProjection {
                    source: source.to_owned(),
                    index: key.index(),
                    interior_cell,
                    mesh_key: normalize(reference.id.as_ref()).into_owned(),
                    translation: vec3_bits(reference.translation),
                    rotation: vec3_bits(reference.rotation),
                    scale: reference.scale.to_bits(),
                    vis_index: reference.vis_index,
                    effective_density: effective_grass_density(reference.id.as_ref(), settings, overrides).to_bits(),
                })
            })
            .collect();
        self.grass_placements.sort_unstable();
    }

    pub(crate) fn reference_bucket_count(&self) -> usize {
        self.exterior_cells.len() + self.interiors.len()
    }
}

/// Projection of all references assigned to one exterior cell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReferenceCellProjection {
    /// Hash of the sorted full reference records and resolved views.
    pub(crate) reference_hash: [u8; 32],
    /// Referenced object IDs retained for reverse-dependency construction.
    pub(crate) object_ids: BTreeSet<String>,
}

impl ReferenceCellProjection {
    fn capture(
        usage: &UsageInfo<'_>,
        mut references: Vec<(crate::StableRefKey, &DistantReference<'_>)>,
        settings: &GenerationSettings,
        overrides: &StaticOverrides,
    ) -> Self {
        references.sort_unstable_by_key(|(key, _)| *key);
        let object_ids = references
            .iter()
            .map(|(_, reference)| reference.id.as_ref())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let mut writer = CanonicalWriter::new();
        writer.write_str("reference_cell_v1");
        writer.write_u64(references.len() as u64);
        for (key, reference) in references {
            write_reference(&mut writer, usage, key, reference, settings, overrides);
        }
        Self {
            reference_hash: writer.finish(),
            object_ids,
        }
    }
}

/// Projection of one interior cell and its inclusion metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InteriorCellProjection {
    /// Metadata used by interior inclusion filtering, if the merged cell supplied it.
    pub(crate) metadata: Option<InteriorMetadataProjection>,
    /// Hash of the sorted full reference records and resolved views.
    pub(crate) reference_hash: [u8; 32],
    /// Referenced object IDs retained for reverse-dependency construction.
    pub(crate) object_ids: BTreeSet<String>,
}

impl CanonicalWrite for InteriorCellProjection {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        write_option(writer, self.metadata.as_ref());
        writer.write_bytes(&self.reference_hash);
        writer.write_u64(self.object_ids.len() as u64);
        for object_id in &self.object_ids {
            writer.write_str(object_id);
        }
    }
}

/// Inclusion metadata attached to one interior cell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InteriorMetadataProjection {
    pub(crate) behaves_like_exterior: bool,
    pub(crate) has_water: bool,
    /// Raw water-height bits.
    pub(crate) water_height: u32,
}

impl From<&InteriorMetadata> for InteriorMetadataProjection {
    fn from(value: &InteriorMetadata) -> Self {
        Self {
            behaves_like_exterior: value.behaves_like_exterior,
            has_water: value.has_water,
            water_height: value.water_height.to_bits(),
        }
    }
}

impl CanonicalWrite for InteriorMetadataProjection {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.write_bool(self.behaves_like_exterior);
        writer.write_bool(self.has_water);
        writer.write_u32(self.water_height);
    }
}

/// Compact identity of one cell's decoded LAND inputs.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct LandscapeCellProjection {
    /// Hash of the height grid consumed by terrain mesh generation.
    pub(crate) height_hash: Option<[u8; 32]>,
    /// Hash of normals, colors, indices, and the full resolved LTEX table.
    pub(crate) terrain_hash: Option<[u8; 32]>,
    /// Hash of texture indices and the resolved LTEX table only.
    pub(crate) material_hash: Option<[u8; 32]>,
    /// Assigned normalized texture keys retained for reverse-dependency construction.
    pub(crate) referenced_textures: BTreeSet<String>,
}

/// Object-definition projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ObjectDefinitionProjection {
    pub(crate) mesh_key: String,
    pub(crate) kind: ObjectKindProjection,
    pub(crate) ignore_by_default: bool,
    pub(crate) force_mesh_generation: bool,
    pub(crate) vis_index: u16,
    /// Effective script-disable classification.
    pub(crate) script_disabled: bool,
}

impl From<&ObjectDefinition<'_>> for ObjectDefinitionProjection {
    fn from(value: &ObjectDefinition<'_>) -> Self {
        Self {
            mesh_key: normalize(value.mesh).into_owned(),
            kind: value.kind.into(),
            ignore_by_default: value.ignore_by_default,
            force_mesh_generation: value.force_mesh_generation,
            vis_index: value.vis_index,
            script_disabled: value.script_disabled,
        }
    }
}

impl CanonicalWrite for ObjectDefinitionProjection {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.write_str(&self.mesh_key);
        self.kind.write_canonical(writer);
        writer.write_bool(self.ignore_by_default);
        writer.write_bool(self.force_mesh_generation);
        writer.write_u16(self.vis_index);
        writer.write_bool(self.script_disabled);
    }
}

/// Stable tag for the broad source record kind of an object definition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ObjectKindProjection {
    /// STAT record.
    Static,
    /// ACTI record.
    Activator,
    /// DOOR record.
    Door,
    /// CONT or LIGH record.
    Misc,
}

impl From<ObjectKind> for ObjectKindProjection {
    fn from(value: ObjectKind) -> Self {
        match value {
            ObjectKind::Static => Self::Static,
            ObjectKind::Activator => Self::Activator,
            ObjectKind::Door => Self::Door,
            ObjectKind::Misc => Self::Misc,
        }
    }
}

impl CanonicalWrite for ObjectKindProjection {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.write_u8(match self {
            Self::Static => 0,
            Self::Activator => 1,
            Self::Door => 2,
            Self::Misc => 3,
        });
    }
}

/// Effective override state separated into per-mesh and statics-global partitions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OverrideStateProjection {
    /// Per-mesh override directives.
    pub(crate) meshes: BTreeMap<String, MeshOverrideProjection>,
    /// Object-name inclusion overrides.
    pub(crate) names: BTreeMap<String, bool>,
    /// Interior inclusion overrides.
    pub(crate) interiors: BTreeMap<String, bool>,
    /// Dynamic-visibility groups and lookup tables.
    pub(crate) dynamic_visibility: DynamicVisibilityProjection,
}

impl From<&StaticOverrides> for OverrideStateProjection {
    fn from(value: &StaticOverrides) -> Self {
        Self {
            meshes: value
                .mesh_overrides
                .iter()
                .map(|(key, override_)| (normalize(key).into_owned(), MeshOverrideProjection::from(override_)))
                .collect(),
            names: value
                .names
                .iter()
                .map(|(key, enabled)| (key.to_ascii_lowercase(), *enabled))
                .collect(),
            interiors: value
                .interiors
                .iter()
                .map(|(key, enabled)| (key.as_str().to_ascii_lowercase(), *enabled))
                .collect(),
            dynamic_visibility: DynamicVisibilityProjection::from(value),
        }
    }
}

/// One per-mesh override.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MeshOverrideProjection {
    /// Excludes the mesh by default.
    pub(crate) ignore: bool,
    /// Explicit MGE-XE static type tag.
    pub(crate) static_type: u8,
    /// Raw density bits.
    pub(crate) density: u32,
    /// Raw simplification override bits.
    pub(crate) simplify: Option<u32>,
    /// Treats the mesh as scriptless.
    pub(crate) no_script: bool,
}

impl From<&StaticOverride> for MeshOverrideProjection {
    fn from(value: &StaticOverride) -> Self {
        Self {
            ignore: value.ignore,
            static_type: value.static_type as u8,
            density: value.density.to_bits(),
            simplify: value.simplify.map(f32::to_bits),
            no_script: value.no_script,
        }
    }
}

impl CanonicalWrite for MeshOverrideProjection {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.write_bool(self.ignore);
        writer.write_u8(self.static_type);
        writer.write_u32(self.density);
        match self.simplify {
            Some(bits) => {
                writer.write_bool(true);
                writer.write_u32(bits);
            }
            None => writer.write_bool(false),
        }
        writer.write_bool(self.no_script);
    }
}

/// Dynamic-visibility state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DynamicVisibilityProjection {
    /// Ordered visibility groups; indices are serialized explicitly.
    pub(crate) groups: Vec<DynamicVisibilityGroupProjection>,
    /// Script-to-group lookup.
    pub(crate) scripts: BTreeMap<String, u16>,
    /// Unique-object-to-group lookup.
    pub(crate) unique_objects: BTreeMap<String, u16>,
}

impl From<&StaticOverrides> for DynamicVisibilityProjection {
    fn from(value: &StaticOverrides) -> Self {
        Self {
            groups: value
                .dynamic_vis
                .groups
                .iter()
                .map(|group| DynamicVisibilityGroupProjection {
                    index: group.index,
                    kind: DynamicVisibilityKindProjection::from(&group.kind),
                })
                .collect(),
            scripts: value
                .dynamic_vis
                .scripts
                .iter()
                .map(|(id, index)| (id.to_ascii_lowercase(), *index))
                .collect(),
            unique_objects: value
                .dynamic_vis
                .unique_objects
                .iter()
                .map(|(id, index)| (id.to_ascii_lowercase(), *index))
                .collect(),
        }
    }
}

impl CanonicalWrite for DynamicVisibilityProjection {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        write_sequence(writer, &self.groups);
        write_u16_map(writer, &self.scripts);
        write_u16_map(writer, &self.unique_objects);
    }
}

/// Dynamic-visibility group.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DynamicVisibilityGroupProjection {
    /// One-based group index.
    pub(crate) index: u16,
    /// Visibility condition.
    pub(crate) kind: DynamicVisibilityKindProjection,
}

impl CanonicalWrite for DynamicVisibilityGroupProjection {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.write_u16(self.index);
        self.kind.write_canonical(writer);
    }
}

/// Dynamic-visibility condition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DynamicVisibilityKindProjection {
    /// Journal index range condition.
    Journal {
        /// Lowercased journal ID.
        journal_id: String,
        /// Inclusive enabled ranges.
        ranges: Vec<(i32, i32)>,
    },
    /// Global value range condition.
    Global {
        /// Lowercased global ID.
        global_id: String,
        /// Inclusive enabled ranges.
        ranges: Vec<(i32, i32)>,
    },
    /// Unique-object linkage condition.
    UniqueObject {
        /// Lowercased source object ID.
        source_id: String,
        /// Ordered linked object IDs.
        linked_ids: Vec<String>,
    },
}

impl From<&DynamicVisKind> for DynamicVisibilityKindProjection {
    fn from(value: &DynamicVisKind) -> Self {
        match value {
            DynamicVisKind::Journal { journal_id, ranges } => Self::Journal {
                journal_id: journal_id.to_ascii_lowercase(),
                ranges: ranges.to_vec(),
            },
            DynamicVisKind::Global { global_id, ranges } => Self::Global {
                global_id: global_id.to_ascii_lowercase(),
                ranges: ranges.to_vec(),
            },
            DynamicVisKind::UniqueObject { source_id, linked_ids } => Self::UniqueObject {
                source_id: source_id.to_ascii_lowercase(),
                linked_ids: linked_ids.iter().map(|id| id.to_ascii_lowercase()).collect(),
            },
        }
    }
}

impl CanonicalWrite for DynamicVisibilityKindProjection {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        match self {
            Self::Journal { journal_id, ranges } => {
                writer.write_u8(0);
                writer.write_str(journal_id);
                write_ranges(writer, ranges);
            }
            Self::Global { global_id, ranges } => {
                writer.write_u8(1);
                writer.write_str(global_id);
                write_ranges(writer, ranges);
            }
            Self::UniqueObject { source_id, linked_ids } => {
                writer.write_u8(2);
                writer.write_str(source_id);
                writer.write_u64(linked_ids.len() as u64);
                for id in linked_ids {
                    writer.write_str(id);
                }
            }
        }
    }
}

/// Usage-filter settings retained at the pre-filter projection boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct UsageSettingsProjection {
    /// Global grass density raw bits.
    pub(crate) grass_density: u32,
    /// Whether activators participate.
    pub(crate) include_activators: bool,
    /// Whether miscellaneous objects participate.
    pub(crate) include_misc: bool,
    /// Whether behaves-like-exterior interiors participate.
    pub(crate) include_behaves_like_exterior: bool,
    /// Whether watery interiors participate.
    pub(crate) include_interiors_with_water: bool,
    /// Whether spatially large interiors participate.
    pub(crate) include_large_interiors: bool,
    /// Whether persistent `SomeId->Disable` targets are excluded.
    pub(crate) exclude_script_disable_targets: bool,
    /// Non-grass static cull depth below the applicable water level, as raw bits.
    pub(crate) deep_water_static_cull_depth: u32,
}

impl From<&GenerationSettings> for UsageSettingsProjection {
    /// Projects the usage-filter settings whose effects are not all materialized by the sibling
    /// projections.
    ///
    /// The exhaustive destructure is this projection's enforcement point, mirroring
    /// [`UnitSettingsPartitions::from_settings`](crate::generation::units::UnitSettingsPartitions::from_settings):
    /// this struct is the only channel by which a setting itself reaches `statics_global`, and so
    /// the `statics_domain_digest` whole-bundle carry gate. `settings_identity` does not feed that
    /// gate, so a setting missing here would stop invalidating the static cache. Any field added to
    /// [`GenerationSettings`] now fails to compile until it is classified below.
    fn from(value: &GenerationSettings) -> Self {
        let GenerationSettings {
            // --- Retained: effects are not all materialized at the pre-filter boundary ---
            grass_density,
            include_activators,
            include_misc,
            include_behaves_like_exterior,
            include_interiors_with_water,
            include_large_interiors,
            exclude_script_disable_targets,
            deep_water_static_cull_depth,
            // --- Materialized by a sibling projection ---
            // These only select which override sources are parsed; their entire effect is already
            // present in the resolved `StaticOverrides` captured by `OverrideStateProjection`,
            // which `fingerprint_statics_global` folds in alongside this struct.
            use_override_list: _,
            override_files: _,
            use_plugin_metadata: _,
            // --- Owned by a unit fingerprint partition (`UnitSettingsPartitions`) ---
            min_static_size: _,
            door_size_multiplier: _,
            static_mesh_target_error: _,
            static_mesh_normal_weight: _,
            static_mesh_color_weight: _,
            static_mesh_merge_error_multiplier: _,
            merge_group_radius: _,
            max_static_texture_long_axis: _,
            max_static_texture_short_axis: _,
            max_static_atlas_size: _,
            texture_dedupe_mode: _,
            static_texture_sizing: _,
            max_terrain_texture_size: _,
            max_terrain_atlas_size: _,
            terrain_detail: _,
            terrain_mesh_smoothed_normal_weight: _,
            terrain_mesh_color_weight: _,
            // --- Terrain-domain globals and control-map guards (not statics) ---
            generate_terrain: _,
            max_terrain_control_texture_size: _,
            max_terrain_control_texture_bytes: _,
            // --- Execution policy (excluded from `settings_identity`) ---
            force_rebuild: _,
        } = value;

        Self {
            grass_density: grass_density.to_bits(),
            include_activators: *include_activators,
            include_misc: *include_misc,
            include_behaves_like_exterior: *include_behaves_like_exterior,
            include_interiors_with_water: *include_interiors_with_water,
            include_large_interiors: *include_large_interiors,
            exclude_script_disable_targets: *exclude_script_disable_targets,
            deep_water_static_cull_depth: deep_water_static_cull_depth.to_bits(),
        }
    }
}

impl CanonicalWrite for UsageSettingsProjection {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.write_u32(self.grass_density);
        writer.write_bool(self.include_activators);
        writer.write_bool(self.include_misc);
        writer.write_bool(self.include_behaves_like_exterior);
        writer.write_bool(self.include_interiors_with_water);
        writer.write_bool(self.include_large_interiors);
        writer.write_bool(self.exclude_script_disable_targets);
        writer.write_u32(self.deep_water_static_cull_depth);
    }
}

/// Dedicated-grass placement retained outside per-cell merge ownership.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct GrassPlacementProjection {
    /// Normalized owning plugin filename.
    pub(crate) source: String,
    /// Occurrence-stable source-local index.
    pub(crate) index: u32,
    /// Exact name of the interior cell the placement sits in; `None` for the exterior.
    pub(crate) interior_cell: Option<String>,
    pub(crate) mesh_key: String,
    /// Raw translation component bits.
    pub(crate) translation: [u32; 3],
    /// Raw rotation component bits.
    pub(crate) rotation: [u32; 3],
    /// Raw scale bits.
    pub(crate) scale: u32,
    pub(crate) vis_index: u16,
    /// Raw effective grass-density bits.
    pub(crate) effective_density: u32,
}

impl CanonicalWrite for GrassPlacementProjection {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.write_str(&self.source);
        writer.write_u32(self.index);
        writer.write_bool(self.interior_cell.is_some());
        if let Some(interior_cell) = &self.interior_cell {
            writer.write_str(interior_cell);
        }
        writer.write_str(&self.mesh_key);
        write_vec3_bits(writer, self.translation);
        write_vec3_bits(writer, self.rotation);
        writer.write_u32(self.scale);
        writer.write_u16(self.vis_index);
        writer.write_u32(self.effective_density);
    }
}

fn write_reference(
    writer: &mut CanonicalWriter,
    usage: &UsageInfo<'_>,
    key: crate::StableRefKey,
    reference: &DistantReference<'_>,
    settings: &GenerationSettings,
    overrides: &StaticOverrides,
) {
    let source = usage
        .reference_source_name(key.source())
        .expect("pre-filter plugin references have stable source filenames");
    let object_id = reference.id.as_ref();
    writer.write_str(source);
    writer.write_u32(key.index());
    writer.write_str(object_id);
    write_vec3_bits(writer, vec3_bits(reference.translation));
    write_vec3_bits(writer, vec3_bits(reference.rotation));
    writer.write_f32(reference.scale);
    writer.write_bool(reference.deleted);

    let resolved = usage.objects.get(object_id);
    writer.write_bool(resolved.is_some());
    if let Some(object) = resolved {
        let density = grass_density(object.mesh, settings, overrides);
        writer.write_str(object.mesh);
        writer.write_u16(object.vis_index);
        writer.write_bool(density.is_some());
        match density {
            Some(density) => {
                writer.write_bool(true);
                writer.write_f32(density);
            }
            None => writer.write_bool(false),
        }
    }
}

fn grass_density(mesh: &str, settings: &GenerationSettings, overrides: &StaticOverrides) -> Option<f32> {
    let mesh_override = overrides.mesh_overrides.get(mesh);
    let is_grass = mesh.starts_with("grass\\")
        || mesh_override.is_some_and(|override_| override_.static_type == StaticType::StaticGrass);
    is_grass.then(|| {
        mesh_override
            .and_then(|override_| (override_.density >= 0.0).then_some(override_.density))
            .unwrap_or(settings.grass_density)
    })
}

fn effective_grass_density(mesh: &str, settings: &GenerationSettings, overrides: &StaticOverrides) -> f32 {
    grass_density(mesh, settings, overrides).unwrap_or(settings.grass_density)
}

fn hash_heights(heights: &[[f32; 65]; 65]) -> [u8; 32] {
    let mut writer = CanonicalWriter::new();
    writer.write_str("land_heights_v1");
    for row in heights {
        for &height in row {
            writer.write_f32(height);
        }
    }
    writer.finish()
}

/// Writes a cell's per-vertex texture assignment grid followed by its index-to-path table.
///
/// Shared by [`hash_terrain_cell`] and [`hash_terrain_material`] so the two hashes cannot drift
/// apart. The table is sorted by index here rather than assumed sorted: the production loader
/// calls `sort_unstable_keys` before publishing the table, but test fixtures build `texture_table`
/// directly, so a hash that inherited `IndexMap` insertion order would make the fingerprint
/// depend on traversal order and break determinism. The sort is O(n) on already-ordered input
/// for a table that holds at most a handful of entries.
fn write_canonical_texture_assignment(writer: &mut CanonicalWriter, cell: &crate::TerrainCell<'_>) {
    for row in cell.texture_indices.iter() {
        for &index in row {
            writer.write_u16(index);
        }
    }
    writer.write_u64(cell.texture_table.len() as u64);
    for (&index, texture) in cell.texture_table.iter().sorted_unstable_by_key(|(index, _)| **index) {
        writer.write_u16(index);
        writer.write_str(&normalize(texture));
    }
}

fn hash_terrain_cell(cell: &crate::TerrainCell<'_>) -> [u8; 32] {
    let mut writer = CanonicalWriter::new();
    writer.write_str("land_terrain_v1");
    writer.write_u64(cell.normals.iter().len() as u64);
    for normal in cell.normals.iter() {
        write_vec3_bits(&mut writer, vec3_bits(normal));
    }
    writer.write_u64(cell.colors.iter().len() as u64);
    for color in cell.colors.iter() {
        writer.write_f32(color.x);
        writer.write_f32(color.y);
        writer.write_f32(color.z);
        writer.write_f32(color.w);
    }
    write_canonical_texture_assignment(&mut writer, cell);
    writer.finish()
}

fn hash_terrain_material(cell: &crate::TerrainCell<'_>) -> [u8; 32] {
    let mut writer = CanonicalWriter::new();
    writer.write_str("land_material_v1");
    write_canonical_texture_assignment(&mut writer, cell);
    writer.finish()
}

fn referenced_texture_keys(cell: &crate::TerrainCell<'_>) -> BTreeSet<String> {
    cell.texture_indices
        .iter()
        .flatten()
        .filter_map(|raw| raw.checked_sub(1).and_then(|index| cell.texture_table.get(&index)))
        .map(|key| normalize(key).into_owned())
        .collect()
}

fn normalized_filename(path: &Path) -> Option<String> {
    Some(path.file_name()?.to_string_lossy().to_ascii_lowercase())
}

fn vec3_bits(value: glam::Vec3) -> [u32; 3] {
    [value.x.to_bits(), value.y.to_bits(), value.z.to_bits()]
}

fn write_vec3_bits(writer: &mut CanonicalWriter, value: [u32; 3]) {
    for component in value {
        writer.write_u32(component);
    }
}

fn write_u16_map(writer: &mut CanonicalWriter, values: &BTreeMap<String, u16>) {
    writer.write_u64(values.len() as u64);
    for (key, &value) in values {
        writer.write_str(key);
        writer.write_u16(value);
    }
}

fn write_sequence<T: CanonicalWrite>(writer: &mut CanonicalWriter, values: &[T]) {
    writer.write_u64(values.len() as u64);
    for value in values {
        value.write_canonical(writer);
    }
}

fn write_option<T: CanonicalWrite>(writer: &mut CanonicalWriter, value: Option<&T>) {
    writer.write_bool(value.is_some());
    if let Some(value) = value {
        value.write_canonical(writer);
    }
}

fn write_ranges(writer: &mut CanonicalWriter, ranges: &[(i32, i32)]) {
    writer.write_u64(ranges.len() as u64);
    for &(start, end) in ranges {
        writer.write_i32(start);
        writer.write_i32(end);
    }
}

#[cfg(test)]
mod tests;
