use std::borrow::Cow;
use std::path::{Path, PathBuf};

use glam::{Affine3A, EulerRot, Quat, Vec2, Vec3};
use hashbrown::{HashMap, HashSet};
use tracing::{info, info_span, warn};
use uncased::{AsUncased, Uncased};

use rayon::prelude::*;

use crate::mge_xe::distant_statics::{BoundingBox, StaticType};
use crate::{IndexMap, Vfs};
use crate::{StaticOverrides, TerrainCell, TerrainCells, UsageWarning};

mod filter;
mod grass;
mod load;
mod script;
mod terrain;
mod types;

use types::{ReferenceSources, UString};

pub use grass::{classify_grass_plugins, is_grass_plugin};
pub use terrain::*;
pub use types::*;

use grass::load_grass_plugins;

/// Settings used while filtering parsed plugin usage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UsageFilterOptions {
    /// Includes activator records in distant-land usage.
    pub include_activators: bool,
    /// Includes supported miscellaneous object records in distant-land usage.
    pub include_misc: bool,
    /// Includes interior cells that contain water.
    pub include_interiors_with_water: bool,
    /// Includes interior cells flagged as behaving like exteriors.
    pub include_behaves_like_exterior: bool,
    /// Includes interior cells whose placed-reference span is at least 10,000 units.
    pub include_large_interiors: bool,
    /// Excludes persistent references to objects named as a `SomeId->Disable` target by any
    /// script in the load order.
    pub exclude_script_disable_targets: bool,
    /// Global density applied to grass without a mesh-specific density override.
    pub grass_density: f32,
}

#[cfg(test)]
mod tests;
