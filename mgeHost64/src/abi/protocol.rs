use crate::abi::constants::{Dword, VecId};
use crate::abi::math::{D3dxVector4, ViewFrustum};
use bytemuck::{Pod, Zeroable};

/// Sort order requested for a visible mesh set.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VisibleSetSort {
    /// Preserve insertion order.
    #[default]
    None = 0,
    /// Sort by render state, primarily texture then vertex buffer.
    ByState = 1,
    /// Sort by texture only.
    ByTexture = 2,
}

impl VisibleSetSort {
    /// Decodes the ABI value, mapping unknown values to `None`.
    pub fn from_raw_lossy(value: u8) -> Self {
        match value {
            1 => Self::ByState,
            2 => Self::ByTexture,
            _ => Self::None,
        }
    }
}

/// RPC commands understood by the 64-bit host.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Command {
    /// No-op placeholder.
    #[default]
    None = 0,
    /// Allocate a shared vector.
    AllocVec = 1,
    /// Free a shared vector.
    FreeVec = 2,
    /// Exit the host process.
    Exit = 3,
    /// Apply dynamic-visibility updates.
    UpdateDynVis = 4,
    /// Initialize distant static quadtrees.
    InitDistantStatics = 5,
    /// Initialize landscape meshes.
    InitLandscape = 6,
    /// Select the active world space.
    SetWorldSpace = 7,
    /// Query coarse visible meshes.
    GetVisibleMeshesCoarse = 8,
    /// Query precise visible meshes.
    GetVisibleMeshes = 9,
    /// Sort an existing visible set in place.
    SortVisibleSet = 10,
    /// Apply live terrain horizon-culling tuning parameters.
    SetHorizonConfig = 11,
    /// Close the current render frame for the adaptive horizon gate: ticks the gate once with the
    /// frame's accumulated precise-static stats. Decoupled from `SortVisibleSet` so it can be sent
    /// at the true per-frame render boundary while horizon culling is enabled, regardless of whether
    /// the main distant-static pass or only a reflection pass ran this frame.
    FinishHorizonFrame = 12,
    /// Query startup-generation/output readiness.
    QueryOutputStatus = 13,
    /// Apply completed residency transitions and GPU buffer handles.
    UpdateResidency = 14,
    /// Advance the bounded merged-static residency planner.
    PlanResidency = 15,
}

impl Command {
    /// Decodes a raw command value from the shared parameter block.
    pub fn from_raw(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::None),
            1 => Some(Self::AllocVec),
            2 => Some(Self::FreeVec),
            3 => Some(Self::Exit),
            4 => Some(Self::UpdateDynVis),
            5 => Some(Self::InitDistantStatics),
            6 => Some(Self::InitLandscape),
            7 => Some(Self::SetWorldSpace),
            8 => Some(Self::GetVisibleMeshesCoarse),
            9 => Some(Self::GetVisibleMeshes),
            10 => Some(Self::SortVisibleSet),
            11 => Some(Self::SetHorizonConfig),
            12 => Some(Self::FinishHorizonFrame),
            13 => Some(Self::QueryOutputStatus),
            14 => Some(Self::UpdateResidency),
            15 => Some(Self::PlanResidency),
            _ => None,
        }
    }
}

/// Host-to-client residency request action.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResidencyPlanAction {
    #[default]
    Admit = 1,
    Evict = 2,
}

/// Client-to-host committed resource state.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResidencyCommitState {
    #[default]
    Unloaded = 0,
    Resident = 1,
    Unavailable = 2,
}

/// One bounded host-to-client residency request.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct ResidencyPlan {
    pub resource_id: u32,
    pub action: u32,
    pub plan_epoch: u32,
    pub reserved: u32,
}

/// One completed client-to-host residency transition.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct ResidencyCommit {
    pub resource_id: u32,
    pub state: u32,
    pub vbuffer: u32,
    pub ibuffer: u32,
}

/// Parameters for `Command::UpdateResidency`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct UpdateResidencyParameters {
    pub commits: VecId,
    pub success: u32,
}

/// Parameters for `Command::PlanResidency`.
#[repr(C, packed(4))]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct PlanResidencyParameters {
    pub plan: VecId,
    pub plan_epoch: u32,
    pub center_x: f32,
    pub center_y: f32,
    pub center_z: f32,
    pub admission_radius: f32,
    pub retain_radius: f32,
    pub max_cells: u32,
    pub max_resources: u32,
    pub view_heading_bin: u32,
    pub cap_bytes: u64,
    pub available_bytes: u64,
    pub cap_debt_bytes: u64,
}

/// Parameters for `Command::AllocVec`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct AllocVecParameters {
    /// Maximum number of elements the vector may ever hold.
    pub max_capacity_in_elements: u32,
    /// Preferred commit granularity expressed in elements.
    pub window_size_in_elements: u32,
    /// Size of each element in bytes.
    pub element_size: u32,
    /// Number of elements to commit immediately after allocation.
    pub initial_capacity: u32,
    /// Total bytes reserved in the file mapping.
    pub reserved_bytes: u32,
    /// Bytes committed per growth window.
    pub window_bytes: u32,
    /// Bytes reserved for the shared header before element storage.
    pub header_bytes: u32,
    /// Duplicated mapping handle for the 32-bit client.
    pub shared_mem32: u32,
    /// Vector identifier assigned by the host.
    pub id: VecId,
}

/// Parameters for `Command::FreeVec`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct FreeVecParameters {
    /// Vector identifier to free.
    pub id: VecId,
    /// Set to non-zero by the host when the vector was actually freed.
    pub was_freed: u8,
    /// Reserved for ABI alignment.
    pub _padding0: [u8; 3],
}

/// Parameters for `Command::UpdateDynVis`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct DynVisParameters {
    /// Shared vector containing `DynVisFlag` records.
    pub id: VecId,
}

/// Parameters for `Command::InitDistantStatics`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct DistantStaticParameters {
    /// Shared vector containing `DistantStatic` records.
    pub distant_statics: VecId,
    /// Shared vector containing `DistantSubset` records.
    pub distant_subsets: VecId,
    /// Client-side far static min-size threshold used for quadtree classification.
    pub far_static_min_size: f32,
    /// Client-side very-far static min-size threshold used for quadtree classification.
    pub very_far_static_min_size: f32,
    /// Set to non-zero when initialization completed successfully.
    pub success: u32,
}

/// Parameters for `Command::InitLandscape`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct InitLandscapeParameters {
    /// Shared vector containing `LandscapeBuffers` records.
    pub buffers: VecId,
    /// Stable terrain sort token used by the host quadtree; terrain textures are globally bound.
    pub terrain_sort_token: u32,
    /// Set to non-zero when initialization completed successfully.
    pub success: u32,
}

/// Parameters for `Command::QueryOutputStatus`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct QueryOutputStatusParameters {
    /// 0 pending, 1 ready, 2 failed.
    pub status: u32,
}

/// Parameters for `Command::SetWorldSpace`.
#[repr(C, packed(4))]
#[derive(Clone, Copy, Debug, Zeroable, Pod)]
pub struct SetWorldSpaceParameters {
    /// Fixed-size cell or world-space name buffer.
    pub cellname: [u8; 64],
    /// Set to non-zero by the host when the name resolved to a known world space.
    pub cell_found: u8,
    /// Reserved for ABI alignment.
    pub _padding0: [u8; 3],
}

impl Default for SetWorldSpaceParameters {
    fn default() -> Self {
        Self {
            cellname: [0; 64],
            cell_found: 0,
            _padding0: [0; 3],
        }
    }
}

/// Parameters for the visible-mesh query and sort commands.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct GetMeshesParameters {
    /// Shared vector that receives `RenderMesh` results.
    pub visible_set: VecId,
    /// Raw `VisibleSetSort` value.
    pub sort: u8,
    /// Reserved for ABI alignment.
    pub _padding0: [u8; 3],
    /// View frustum used for culling.
    pub view_frustum: ViewFrustum,
    /// Visibility mask composed from `VIS_*` constants.
    pub set_flags: Dword,
    /// XYZ eye position plus W radius limit for precise queries.
    pub view_sphere: D3dxVector4,
    /// Raw near-static band end in world units.
    pub near_static_end: f32,
    /// Raw far-static band end in world units.
    pub far_static_end: f32,
}

/// Parameters for `Command::SetHorizonConfig`.
///
/// Carries the live terrain horizon-culling tuning values pushed from the 32-bit
/// runtime. Mirrors `SetHorizonConfigParameters` in `d3d8/cpp/ipc/bridge.h`; the host
/// clamps every field to the `config.rs` ranges before applying.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct SetHorizonConfigParameters {
    /// Non-zero enables host-side terrain horizon culling.
    pub enabled: u32,
    /// Terrain height bias subtracted from the occluder horizon.
    pub bias_z: f32,
    /// Object-top bias added before testing against the horizon.
    pub object_bias_z: f32,
    /// Near terrain radius excluded from horizon construction.
    pub near_units: f32,
    /// Distance covered by each horizon prefix ring.
    pub ring_step: f32,
    /// Maximum terrain range used while building the per-frame horizon.
    pub max_range: f32,
    /// Number of azimuth bins in the horizon table.
    pub bins: u32,
    /// Radial sample step used while building the horizon table. The terrain grid spacing is a
    /// separate load-time host setting and is not part of this live payload.
    pub sample_spacing: f32,
    /// Non-zero enables the adaptive self-suspension gate.
    pub adaptive_gate: u32,
}

/// Untagged ABI union containing the active command-specific parameter block.
#[repr(C)]
#[derive(Clone, Copy)]
pub union ParameterUnion {
    /// `Command::AllocVec` payload.
    pub alloc_vec_params: AllocVecParameters,
    /// `Command::FreeVec` payload.
    pub free_vec_params: FreeVecParameters,
    /// `Command::UpdateDynVis` payload.
    pub dyn_vis_params: DynVisParameters,
    /// `Command::InitDistantStatics` payload.
    pub distant_static_params: DistantStaticParameters,
    /// `Command::InitLandscape` payload.
    pub init_landscape_params: InitLandscapeParameters,
    /// `Command::SetWorldSpace` payload.
    pub world_space_params: SetWorldSpaceParameters,
    /// Visible-set query payload.
    pub mesh_params: GetMeshesParameters,
    /// `Command::SetHorizonConfig` payload.
    pub horizon_config_params: SetHorizonConfigParameters,
    /// `Command::QueryOutputStatus` payload.
    pub output_status_params: QueryOutputStatusParameters,
    /// `Command::UpdateResidency` payload.
    pub update_residency_params: UpdateResidencyParameters,
    /// `Command::PlanResidency` payload.
    pub plan_residency_params: PlanResidencyParameters,
}

impl Default for ParameterUnion {
    fn default() -> Self {
        Self {
            alloc_vec_params: AllocVecParameters::default(),
        }
    }
}

/// Shared parameter block written by the client before signaling the host.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Parameters {
    /// Raw `Command` discriminant.
    pub command: u32,
    /// Command-specific parameter payload.
    pub params: ParameterUnion,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_decoder_preserves_known_values_and_falls_back_to_none() {
        assert_eq!(Command::from_raw(0), Some(Command::None));
        assert_eq!(Command::from_raw(1), Some(Command::AllocVec));
        assert_eq!(Command::from_raw(10), Some(Command::SortVisibleSet));
        assert_eq!(Command::from_raw(11), Some(Command::SetHorizonConfig));
        assert_eq!(Command::from_raw(12), Some(Command::FinishHorizonFrame));
        assert_eq!(Command::from_raw(13), Some(Command::QueryOutputStatus));
        assert_eq!(Command::from_raw(14), Some(Command::UpdateResidency));
        assert_eq!(Command::from_raw(15), Some(Command::PlanResidency));
        assert_eq!(Command::from_raw(16), None);
        assert_eq!(Command::from_raw(u32::MAX), None);
    }

    #[test]
    fn visible_set_sort_decoder_preserves_known_values_and_falls_back_to_none() {
        assert_eq!(VisibleSetSort::from_raw_lossy(0), VisibleSetSort::None);
        assert_eq!(VisibleSetSort::from_raw_lossy(1), VisibleSetSort::ByState);
        assert_eq!(VisibleSetSort::from_raw_lossy(2), VisibleSetSort::ByTexture);
        assert_eq!(VisibleSetSort::from_raw_lossy(3), VisibleSetSort::None);
        assert_eq!(VisibleSetSort::from_raw_lossy(u8::MAX), VisibleSetSort::None);
    }
}
