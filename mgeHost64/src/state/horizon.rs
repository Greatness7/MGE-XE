mod bounds;
mod cull;
mod gate;
mod height_field;
mod runtime;
mod table;

#[cfg(test)]
pub(crate) use gate::GATE_EVAL_WINDOW;
pub(crate) use runtime::HorizonRuntime;
#[cfg(test)]
pub(crate) use runtime::{MAX_PENDING_FRAMES, MAX_STALE_DISTANCE};

pub use bounds::HorizonMeshBounds;
pub use cull::{
    HorizonCullStats, horizon_culled_bounds, horizon_culled_capped_xy, horizon_culled_rect, horizon_visible_capped_xy,
};
#[cfg(test)]
pub use cull::{horizon_culled, horizon_culled_capped, horizon_visible_capped};
pub use height_field::TerrainHeightField;
#[cfg(test)]
pub use table::MAX_HORIZON_RINGS;
pub use table::{HorizonParams, HorizonTable};

#[cfg(test)]
use crate::abi::{
    BoundingBox, BoundingSphere, D3dxVector2, D3dxVector3, HorizonFootprint, OcclusionFormatError, TerrainFileLayout,
    TerrainVertex,
};
#[cfg(test)]
use bounds::generated_footprint_signed_area;
#[cfg(test)]
use cull::min_distance_sq_to_polygon_edges;
#[cfg(test)]
use height_field::EMPTY_HEIGHT;
#[cfg(test)]
use table::EMPTY_SLOPE;

#[cfg(test)]
mod tests;
