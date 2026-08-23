//! Shared override value types consumed while parsing plugin usage.

use hashbrown::HashMap;
use smallvec::SmallVec;
use uncased::Uncased;

use crate::mge_xe::distant_statics::StaticType;

/// Per-mesh override settings parsed from the default section.
#[derive(Debug, PartialEq)]
pub struct StaticOverride {
    /// Excludes the mesh from distant-land generation unless another rule forces it in.
    pub ignore: bool,
    pub static_type: StaticType,
    /// Grass density override. `-1.0` means use global setting.
    pub density: f32,
    /// Mesh simplification override. `None` means auto.
    pub simplify: Option<f32>,
    /// Treats the mesh as scriptless for classification purposes.
    pub no_script: bool,
}

impl Default for StaticOverride {
    fn default() -> Self {
        Self {
            ignore: false,
            static_type: StaticType::StaticAuto,
            density: -1.0,
            simplify: None,
            no_script: false,
        }
    }
}

/// Dynamic-visibility condition attached to a visibility group.
#[derive(Debug, PartialEq)]
pub enum DynamicVisKind {
    /// Group visibility controlled by a journal range.
    Journal {
        /// Lowercased journal ID.
        journal_id: String,
        /// Half-open journal ranges that enable the group.
        ranges: SmallVec<[(i32, i32); 8]>,
    },
    /// Group visibility controlled by a global-variable range.
    Global {
        /// Lowercased global ID.
        global_id: String,
        /// Half-open global ranges that enable the group.
        ranges: SmallVec<[(i32, i32); 8]>,
    },
    /// Group visibility controlled by a unique source object and linked objects.
    UniqueObject {
        /// Lowercased source object ID.
        source_id: String,
        /// Object IDs linked to the same dynamic-visibility group.
        linked_ids: Vec<String>,
    },
}

/// One parsed dynamic-visibility group.
#[derive(Debug, PartialEq)]
pub struct DynamicVisGroup {
    /// One-based group index written into `usage.data`.
    pub index: u16,
    /// Condition that activates the group.
    pub kind: DynamicVisKind,
}

/// Dynamic-visibility state parsed from override files.
#[derive(Debug, Default, PartialEq)]
pub struct DynamicVisData {
    /// Ordered groups written to `usage.data`.
    pub groups: Vec<DynamicVisGroup>,
    /// Script ID to dynamic-visibility group index mapping.
    pub scripts: HashMap<String, u16>,
    /// Unique object ID to dynamic-visibility group index mapping.
    pub unique_objects: HashMap<String, u16>,
}

/// Combined override data parsed from one or more override files.
#[derive(Debug, Default, PartialEq)]
pub struct StaticOverrides {
    /// Mesh-path overrides from the default section.
    pub mesh_overrides: HashMap<String, StaticOverride>,
    /// Object-name enable/disable overrides from `[names]`.
    pub names: HashMap<String, bool>,
    /// Interior-cell enable/disable overrides from `[interiors]`.
    pub interiors: HashMap<Uncased<'static>, bool>,
    /// Dynamic-visibility groups and lookup tables from `[dynamic_vis]`.
    pub dynamic_vis: DynamicVisData,
}
