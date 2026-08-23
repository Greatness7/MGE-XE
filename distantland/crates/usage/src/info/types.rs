use super::*;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use itertools::Itertools;

/// A case-insensitive string, typically used for object IDs.
pub(crate) type UString = Uncased<'static>;
/// A run-local handle for the normalized filename that owns a placed reference.
///
/// The numeric value is an implementation detail and must never be hashed or persisted.
/// Use [`UsageInfo::reference_source_name`] at identity boundaries instead.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceId(u32);

impl SourceId {
    /// Reserved source for synthetic references created by merge batching.
    const SYNTHETIC: Self = Self(u32::MAX);

    fn from_rank(rank: usize) -> Self {
        Self(rank.try_into().expect("active plugin count fits in u32"))
    }

    /// Returns the reserved source for run-local synthetic references.
    pub const fn synthetic() -> Self {
        Self::SYNTHETIC
    }

    pub fn is_synthetic(self) -> bool {
        self == Self::SYNTHETIC
    }
}

/// Source-stable identity for one placed reference.
///
/// `source` resolves to a normalized plugin filename through the owning [`UsageInfo`].
/// Its run-local ordinal is used only for compact storage and ordering; persisted identity
/// uses the filename string and `index`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StableRefKey {
    source: SourceId,
    index: u32,
}

impl StableRefKey {
    pub const fn new(source: SourceId, index: u32) -> Self {
        Self { source, index }
    }

    /// Creates a run-local key for a synthetic merged reference.
    pub const fn synthetic(index: u32) -> Self {
        Self::new(SourceId::SYNTHETIC, index)
    }

    /// Returns the run-local source handle.
    pub const fn source(self) -> SourceId {
        self.source
    }

    /// Returns the source-local reference index.
    pub const fn index(self) -> u32 {
        self.index
    }

    /// Creates a deterministic key backed by the crate's synthetic test source table.
    #[doc(hidden)]
    pub const fn test(index: u32) -> Self {
        Self::new(SourceId(0), index)
    }
}

/// Immutable run-scoped interning table for reference source filenames.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct ReferenceSources(Arc<[String]>);

impl ReferenceSources {
    pub(super) fn from_load_order(load_order: &[PathBuf]) -> Self {
        Self::from_plugin_lists(load_order, &[])
    }

    pub(super) fn from_plugin_lists(load_order: &[PathBuf], grass_plugins: &[PathBuf]) -> Self {
        let names = load_order
            .iter()
            .chain(grass_plugins)
            .filter_map(|path| normalized_filename(path))
            .sorted_unstable()
            .dedup()
            .collect_vec();
        assert!(
            names.len() < u32::MAX as usize,
            "active plugin count fits below the synthetic source sentinel"
        );
        Self(names.into())
    }

    pub(super) fn source_id(&self, path: impl AsRef<Path>) -> Option<SourceId> {
        let name = normalized_filename(path.as_ref())?;
        self.0.binary_search(&name).ok().map(SourceId::from_rank)
    }

    pub(super) fn name(&self, source: SourceId) -> Option<&str> {
        if source.is_synthetic() {
            None
        } else {
            self.0.get(source.0 as usize).map(String::as_str)
        }
    }

    fn is_same_table(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self == other
    }

    fn test() -> Self {
        Self(Arc::from(["test.esp".to_owned()]))
    }
}

fn normalized_filename(path: &Path) -> Option<String> {
    Some(path.file_name()?.to_string_lossy().to_ascii_lowercase())
}

/// A mapping of stable source identity and source-local index to a distant reference.
pub type References<'a> = IndexMap<StableRefKey, DistantReference<'a>>;
pub use distantland_formats::world::LAND_CELL_SIZE;
/// The maximum height a triangle can protrude above the terrain to be considered buried.
pub const MAX_EXPOSED_HEIGHT_THRESHOLD: f32 = 96.0;
/// The minimum ratio of exposed triangle area to total area to be considered visible.
pub const EXPOSED_AREA_RATIO_THRESHOLD: f32 = 0.15;
/// Epsilon for height comparisons to account for terrain precision.
pub const EXPOSED_HEIGHT_EPSILON: f32 = 8.0;
/// Minimum number of triangles required to reliably determine if an object is buried.
pub const MIN_VALID_TRIANGLES: usize = 8;
/// Minimum ratio of triangles that must be evaluated to trust the buried heuristic.
pub const MIN_VALID_TRIANGLE_RATIO: f32 = 0.25;

/// Represents an instance of an object placed in the world.
#[derive(Clone)]
pub struct DistantReference<'a> {
    /// The ID of the object, mapped to the mesh name after resolution.
    pub id: Cow<'a, str>,
    pub deleted: bool,
    pub translation: Vec3,
    pub rotation: Vec3,
    pub scale: f32,
    /// Dynamic visibility group index, where 0 means standard distant static.
    pub vis_index: u16,
    /// Whether Morrowind loads this placement into its records handler, from
    /// [`tes3::esp::Reference::persistent`].
    ///
    /// This is the reference's own `NAM0` block placement plus the moved-reference and
    /// teleport-door overrides. This fact is frozen into each plugin at its own serialize time, not the
    /// base object's CS "persistent" checkbox. Only these can be reached by `SomeId->Disable`.
    pub persistent: bool,
}

/// Metadata about how a base object type is handled during distant land generation.
///
/// Deliberately not `Copy`: `script_id` owns its text so the object's effective script survives
/// the load-order merge, which is what lets script classification run once, afterwards.
#[derive(Clone)]
pub struct ObjectDefinition<'a> {
    /// The NIF mesh file associated with this object.
    pub mesh: &'a str,
    /// If true, instances of this object are excluded from distant land.
    pub ignore_by_default: bool,
    /// If true, the mesh is required for distant statics even if the object is disabled or dynamic.
    pub force_mesh_generation: bool,
    /// The visibility group index assigned to instances of this object.
    pub vis_index: u16,
    /// Broad record kind used by downstream unit identity and door handling.
    pub kind: ObjectKind,
    /// The object's effective attached script id, empty when it has none or a `no_script` mesh
    /// override blanked it. Held so `UsageInfo::classify_script_disables` can resolve the object
    /// against the load-order-wide script set after the merge.
    pub script_id: UString,
    /// Whether the object's effective script is classified as disabling the object.
    ///
    /// Always false until `classify_script_disables` runs.
    pub script_disabled: bool,
    /// Whether this object's id appears as a `SomeId->Disable` target anywhere in the load order.
    ///
    /// Object-level because that is how the target is named, but it excludes *references* rather
    /// than the object: one such call reaches one reference. Always false until
    /// `classify_script_disables` runs.
    pub disable_target: bool,
}

/// Broad categorizations of objects used for inclusion filtering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectKind {
    /// A static (STAT record) with no scripted or interactive behavior.
    Static,
    /// A scripted or interactive world object (ACTI record).
    Activator,
    /// A door (DOOR record). Always included in distant land, like statics.
    Door,
    /// Miscellaneous object type included in distant land under specific settings.
    Misc,
}

/// Metadata for an interior cell used for determining distant land inclusion.
#[derive(Default, Clone)]
pub struct InteriorMetadata {
    /// Whether the cell was flagged as behaving like an exterior in Morrowind's engine.
    pub behaves_like_exterior: bool,
    /// Whether the cell has a visible water plane.
    pub has_water: bool,
    /// Height of the water plane in engine units, meaningful only when `has_water` is true.
    pub water_height: f32,
}

/// Outcome counts for the two mwscript disable rules, surfaced in `generation_report.toml`.
///
/// Advisory only. Nothing reads these back. They exist because the rules remove content and
/// there is otherwise no way to review what they did without a game session.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ScriptDisableReport {
    /// Scripts across the load order containing a bare whole-word `Disable` (Rule A).
    pub self_disabling_scripts: usize,
    /// Objects excluded because their attached script self-disables (Rule A).
    pub objects_excluded_by_script: usize,
    /// Distinct ids named as a `SomeId->Disable` target by any script (Rule B).
    pub disable_targets_named: usize,
    /// Those that resolve to a distant-land base object, and so can exclude anything.
    pub disable_targets_resolved: usize,
    /// Persistent references excluded by Rule B.
    pub references_excluded_as_disable_targets: usize,
    /// Ids of the objects whose persistent references Rule B excluded, sorted.
    ///
    /// The exclusion list the rule is reviewable by; small by construction, since a resolved
    /// target has only as many persistent references as plugins chose to serialize that way.
    pub excluded_disable_targets: Vec<String>,
}

/// Global repository of collected object and placement data from plugins.
#[derive(Default, Clone)]
pub struct UsageInfo<'a> {
    /// Interned normalized source filenames shared by every reference key in this run.
    pub(super) reference_sources: ReferenceSources,
    /// Base object definitions, keyed by their lowercased IDs.
    pub objects: HashMap<String, ObjectDefinition<'a>>,
    /// Meshes that must be generated for distant land due to overrides.
    pub forced_meshes: HashSet<&'a str>,
    /// Meshes belonging to door (DOOR) records. Door statics get the configured door-size
    /// multiplier applied to their cull radius and written bucket radius.
    pub door_meshes: HashSet<&'a str>,
    /// The maximum scale at which each mesh is found in the world.
    pub mesh_scale_maximums: HashMap<&'a str, f32>,
    /// Placed references mapped by their cell identifier. Exterior references are under `"\\0"`.
    pub cells: IndexMap<String, References<'a>>,
    /// Decoded exterior LAND cells (heights, normals, colors, texture assignments).
    ///
    /// Heights live only here; there is no parallel height-only map.
    pub terrain_cells: TerrainCells<'a>,
    /// Inclusion metadata for interior cells.
    pub interior_metadata: HashMap<UString, InteriorMetadata>,
    /// Ids of scripts anywhere in the load order that are classified as disabling their object.
    ///
    /// Accumulated across every plugin by [`UsageInfo::merge`], never cleared. Per-plugin scope
    /// would let a later plugin re-declaring an object silently drop the classification.
    pub(super) disable_scripts: HashSet<UString>,
    /// Object ids named as `SomeId->Disable` targets by any script anywhere in the load order.
    ///
    /// Load-order-wide for the same reason as `disable_scripts`, and more so: the arrow form is
    /// inherently cross-plugin.
    pub(super) disable_targets: HashSet<UString>,
    /// What the two disable rules actually did, for the generation report.
    pub script_disable: ScriptDisableReport,
}

impl<'a> UsageInfo<'a> {
    /// Installs the deterministic single-source table used by cross-crate unit tests.
    #[doc(hidden)]
    pub fn use_test_reference_source(&mut self) {
        self.reference_sources = ReferenceSources::test();
    }

    /// Determines the next available reference index for `source` across all world spaces.
    pub fn next_reference_index(&self, source: SourceId) -> u32 {
        self.cells
            .values()
            .flat_map(|instances| instances.keys())
            .filter_map(|key| (key.source == source).then_some(key.index))
            .max()
            .map_or(1, |i| i + 1)
    }

    /// Resolves a run-local source handle to its normalized plugin filename.
    pub fn reference_source_name(&self, source: SourceId) -> Option<&str> {
        self.reference_sources.name(source)
    }

    /// Incorporates data from another plugin's `UsageInfo`.
    ///
    /// Native references are namespaced by their owning plugin filename. References to masters
    /// share the master's source and native index, allowing later plugins to overwrite or delete
    /// them without depending on global load-order positions.
    ///
    /// # Panics
    ///
    /// Panics if `other` was parsed against a different reference-source table.
    pub fn merge(&mut self, other: UsageInfo<'a>) {
        assert!(
            self.reference_sources.is_same_table(&other.reference_sources),
            "cannot merge usage information from different reference-source tables"
        );
        self.objects.extend(other.objects);
        self.door_meshes.extend(other.door_meshes);
        self.terrain_cells.extend(other.terrain_cells);
        self.interior_metadata.extend(other.interior_metadata);
        // A union, not a last-wins overwrite: `X->Disable` is inherently cross-plugin, and a
        // script declared in one plugin can be attached to an object declared in another.
        self.disable_scripts.extend(other.disable_scripts);
        self.disable_targets.extend(other.disable_targets);
        // `script_disable` is deliberately not merged. It is filled by `classify_script_disables`
        // and `remap_references`, both of which run after the load order has been folded into one
        // `UsageInfo`; every `other` reaching here still holds the default. The one later merge,
        // the dedicated grass list, must not overwrite the finished report.

        for (name, references) in other.cells {
            let merged = self.cells.entry(name).or_default();
            for (key, reference) in references {
                merged.insert(key, reference);
            }
        }
    }

    /// Sorts maps into a stable order so serialized outputs are deterministic.
    pub fn sort_for_deterministic_output(&mut self) {
        self.cells.sort_unstable_keys();
        for references in self.cells.values_mut() {
            references.sort_unstable_keys();
        }
        self.terrain_cells.sort_unstable_keys();
    }

    pub fn exterior_references(&self) -> Option<&References<'a>> {
        self.cells.get("\0")
    }

    pub fn exterior_references_mut(&mut self) -> &mut References<'a> {
        if !self.cells.contains_key("\0") {
            self.cells.insert("\0".to_string(), References::default());
        }
        self.cells.get_mut("\0").expect("exterior references")
    }

    pub fn interior_cells(&self) -> impl Iterator<Item = (&String, &References<'a>)> {
        self.cells.iter().filter(|(name, _)| *name != "\0")
    }

    pub fn total_references_count(&self) -> usize {
        self.cells.values().map(|references| references.len()).sum()
    }

    pub fn exterior_references_count(&self) -> usize {
        self.cells.get("\0").map_or(0, |references| references.len())
    }
}
