use std::collections::VecDeque;
use std::mem::size_of;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

use bytemuck::Pod;
use tracing::{error, info};
use windows_sys::Win32::Foundation::WAIT_OBJECT_0;

use crate::abi::{
    Command, DynVisFlag, INVALID_VECTOR, Parameters, RenderMesh, VIS_STATIC, VecId, VisibleSetSort, bytes_from_fixed,
};
use crate::config::Configuration;
use crate::error::HostError;
use crate::ipc::shared_vec::SharedVec;
use crate::state::distant_land::DistantLandState;
use crate::state::quadtree::TierBands;
use crate::win::{MappedView, OwnedHandle, StartupHandles, map_view, set_event, wait_multiple};
use distantland::output_index::OutputSnapshot;

#[derive(Default)]
pub struct OutputState {
    pub snapshot: Option<OutputSnapshot>,
    pub configuration: Option<Configuration>,
    pub failed: bool,
}

pub type SharedOutputState = Arc<Mutex<OutputState>>;

pub struct Server {
    shared_mem: OwnedHandle,
    client_process: OwnedHandle,
    rpc_start_event: OwnedHandle,
    rpc_complete_event: OwnedHandle,
    parameters: Option<MappedParams>,
    vecs: Vec<Option<SharedVec>>,
    free_vecs: VecDeque<VecId>,
    distant_land: DistantLandState,
    sort_scratch: Vec<RenderMesh>,
    output_state: SharedOutputState,
}

struct MappedParams {
    _view: MappedView,
    ptr: NonNull<Parameters>,
}

impl MappedParams {
    #[allow(clippy::cast_ptr_alignment)]
    fn map(handle: windows_sys::Win32::Foundation::HANDLE) -> Result<Self, HostError> {
        let mapped = map_view(handle, 0, size_of::<Parameters>())?;
        let mut view = MappedView::new(mapped, size_of::<Parameters>())?;
        let ptr = NonNull::new(view.as_bytes_mut().as_mut_ptr().cast::<Parameters>())
            .expect("mapped parameter block pointer is non-null");
        Ok(Self { _view: view, ptr })
    }

    fn get(&self) -> &Parameters {
        unsafe {
            // Safety: ptr always points into the live parameter mapping owned by _view.
            self.ptr.as_ref()
        }
    }

    fn get_mut(&mut self) -> &mut Parameters {
        unsafe {
            // Safety: ptr always points into the live parameter mapping owned by _view, and
            // mutable access is only exposed through &mut self.
            self.ptr.as_mut()
        }
    }
}

fn get_vec_ref_from<T: Pod>(vecs: &[Option<SharedVec>], id: VecId) -> Result<&SharedVec, HostError> {
    let Some(Some(vec)) = vecs.get(id as usize) else {
        return Err(HostError::listen(format!("Vector {id} not found")));
    };
    if !vec.is_type::<T>() {
        return Err(HostError::listen(format!("Vector {id} element size mismatch")));
    }
    Ok(vec)
}

fn get_vec_mut_from<T: Pod>(vecs: &mut [Option<SharedVec>], id: VecId) -> Result<&mut SharedVec, HostError> {
    let Some(Some(vec)) = vecs.get_mut(id as usize) else {
        return Err(HostError::listen(format!("Vector {id} not found")));
    };
    if !vec.is_type::<T>() {
        return Err(HostError::listen(format!("Vector {id} element size mismatch")));
    }
    Ok(vec)
}

impl Server {
    pub fn new(handles: StartupHandles, configuration: Configuration, output_state: SharedOutputState) -> Self {
        Self {
            shared_mem: OwnedHandle(handles.shared_mem),
            client_process: OwnedHandle(handles.client_process),
            rpc_start_event: OwnedHandle(handles.rpc_start_event),
            rpc_complete_event: OwnedHandle(handles.rpc_complete_event),
            parameters: None,
            vecs: Vec::new(),
            free_vecs: VecDeque::new(),
            distant_land: DistantLandState::new(configuration),
            sort_scratch: Vec::new(),
            output_state,
        }
    }

    pub fn init(&mut self) -> Result<(), HostError> {
        self.parameters = Some(MappedParams::map(self.shared_mem.raw())?);
        Ok(())
    }

    /// Unknown commands are logged and ignored. Returns when the client exits or sends
    /// `Command::Exit`.
    pub fn listen(&mut self) -> Result<(), HostError> {
        loop {
            if let Err(error) = set_event(self.rpc_complete_event.raw()) {
                error!("Failed to signal RPC completion: {error}");
            }
            let wait_handles = [self.client_process.raw(), self.rpc_start_event.raw()];
            let wait_result = wait_multiple(&wait_handles, u32::MAX)?;

            // Waiting on the client process exits promptly when Morrowind shuts down.
            if wait_result == WAIT_OBJECT_0 {
                info!("Morrowind process exited; exiting 64-bit host");
                return Ok(());
            }

            let command_value = self.params().command;
            let Some(command) = Command::from_raw(command_value) else {
                error!("Received unknown command value {}", command_value);
                continue;
            };

            match command {
                Command::None => {}
                Command::AllocVec => Self::log_nonfatal_rpc_error("AllocVec", self.alloc_vec()),
                Command::FreeVec => {
                    self.free_vec();
                    Self::log_nonfatal_rpc_error("FreeVec", Ok(()));
                }
                Command::Exit => {
                    info!("Host process received exit command");
                    return Ok(());
                }
                Command::UpdateDynVis => Self::log_nonfatal_rpc_error("UpdateDynVis", self.update_dyn_vis()),
                Command::InitDistantStatics => {
                    Self::log_nonfatal_rpc_error("InitDistantStatics", self.init_distant_statics());
                }
                Command::InitLandscape => Self::log_nonfatal_rpc_error("InitLandscape", self.init_landscape()),
                Command::SetWorldSpace => {
                    self.set_world_space();
                    Self::log_nonfatal_rpc_error("SetWorldSpace", Ok(()));
                }
                Command::GetVisibleMeshesCoarse => {
                    Self::log_nonfatal_rpc_error("GetVisibleMeshesCoarse", self.get_visible_meshes_coarse());
                }
                Command::GetVisibleMeshes => Self::log_nonfatal_rpc_error("GetVisibleMeshes", self.get_visible_meshes()),
                Command::SortVisibleSet => Self::log_nonfatal_rpc_error("SortVisibleSet", self.sort_visible_set()),
                Command::SetHorizonConfig => {
                    Self::log_nonfatal_rpc_error("SetHorizonConfig", self.set_horizon_config());
                }
                Command::FinishHorizonFrame => {
                    Self::log_nonfatal_rpc_error("FinishHorizonFrame", self.finish_horizon_frame());
                }
                Command::QueryOutputStatus => self.query_output_status(),
                Command::UpdateResidency => {
                    Self::log_nonfatal_rpc_error("UpdateResidency", self.update_residency());
                }
                Command::PlanResidency => Self::log_nonfatal_rpc_error("PlanResidency", self.plan_residency()),
            }
        }
    }

    /// Logs command failures without tearing down the whole host process.
    fn log_nonfatal_rpc_error(command: &str, result: Result<(), HostError>) {
        if let Err(error) = result {
            error!("RPC {command} failed: {error}");
        }
    }

    fn params(&self) -> &Parameters {
        self.parameters.as_ref().expect("server initialized").get()
    }

    fn params_mut(&mut self) -> &mut Parameters {
        self.parameters.as_mut().expect("server initialized").get_mut()
    }

    /// Handles `Command::AllocVec`.
    fn alloc_vec(&mut self) -> Result<(), HostError> {
        self.alloc_vec_with(SharedVec::create)
    }

    /// Shared implementation for vector allocation, with an injectable constructor for tests.
    fn alloc_vec_with<F>(&mut self, create: F) -> Result<(), HostError>
    where
        F: FnOnce(
            VecId,
            windows_sys::Win32::Foundation::HANDLE,
            &mut crate::abi::AllocVecParameters,
        ) -> Result<SharedVec, HostError>,
    {
        let mut request = unsafe { self.params().params.alloc_vec_params };
        request.id = INVALID_VECTOR;
        request.shared_mem32 = 0;
        request.reserved_bytes = 0;
        request.window_bytes = 0;
        request.header_bytes = 0;

        info!(
            "AllocVec request: elementSize={} windowElements={} maxElements={} initialCapacity={}",
            request.element_size,
            request.window_size_in_elements,
            request.max_capacity_in_elements,
            request.initial_capacity
        );

        let recycled_id = self.free_vecs.pop_front();
        let id = recycled_id.unwrap_or(self.vecs.len() as VecId);
        let vec = match create(id, self.client_process.raw(), &mut request) {
            Ok(vec) => vec,
            Err(error) => {
                if recycled_id.is_some() {
                    self.free_vecs.push_back(id);
                }
                self.params_mut().params.alloc_vec_params = request;
                return Err(error);
            }
        };
        if id as usize == self.vecs.len() {
            self.vecs.push(Some(vec));
        } else {
            self.vecs[id as usize] = Some(vec);
        }
        self.params_mut().params.alloc_vec_params = request;
        Ok(())
    }

    /// Handles `Command::FreeVec`.
    ///
    /// Vectors are only recycled after both sides have released their shared ownership.
    fn free_vec(&mut self) {
        let mut params = unsafe { self.params().params.free_vec_params };
        params.was_freed = 0;
        if params.id == INVALID_VECTOR {
            self.params_mut().params.free_vec_params = params;
            return;
        }
        let Some(entry) = self.vecs.get_mut(params.id as usize) else {
            self.params_mut().params.free_vec_params = params;
            return;
        };
        if let Some(vec) = entry.as_ref()
            && !vec.can_free()
        {
            self.params_mut().params.free_vec_params = params;
            return;
        }
        *entry = None;
        self.free_vecs.push_back(params.id);
        params.was_freed = 1;
        self.params_mut().params.free_vec_params = params;
    }

    /// Applies dynamic-visibility flags from the shared update vector.
    fn update_dyn_vis(&mut self) -> Result<(), HostError> {
        let params = unsafe { self.params().params.dyn_vis_params };
        let vecs = &mut self.vecs;
        let distant_land = &mut self.distant_land;
        let vec = get_vec_mut_from::<DynVisFlag>(vecs, params.id)?;
        vec.for_each(|update| distant_land.update_dyn_vis_one(update))
    }

    /// Builds distant-static quadtrees from the shared static and subset vectors.
    fn init_distant_statics(&mut self) -> Result<(), HostError> {
        let mut params = unsafe { self.params().params.distant_static_params };
        let result = {
            let vecs = &self.vecs;
            let distant_statics = get_vec_ref_from::<crate::abi::DistantStatic>(vecs, params.distant_statics)?;
            let distant_subsets = get_vec_ref_from::<crate::abi::DistantSubset>(vecs, params.distant_subsets)?;
            self.distant_land.init_distant_statics(
                distant_statics,
                distant_subsets,
                params.far_static_min_size,
                params.very_far_static_min_size,
            )
        };
        params.success = result.is_ok() as u32;
        self.params_mut().params.distant_static_params = params;
        result
    }

    /// Applies client-acknowledged residency state transitions.
    fn update_residency(&mut self) -> Result<(), HostError> {
        let mut params = unsafe { self.params().params.update_residency_params };
        let result = {
            let vecs = &mut self.vecs;
            let distant_land = &mut self.distant_land;
            let commits = get_vec_mut_from::<crate::abi::ResidencyCommit>(vecs, params.commits)?;
            let mut result = Ok(());
            for index in 0..commits.size() {
                if result.is_ok() {
                    result = distant_land.apply_residency_commit(commits.get(index));
                }
            }
            result
        };
        params.success = result.is_ok() as u32;
        self.params_mut().params.update_residency_params = params;
        result
    }

    /// Advances the bounded radial residency planner.
    fn plan_residency(&mut self) -> Result<(), HostError> {
        let params = unsafe { self.params().params.plan_residency_params };
        let vecs = &mut self.vecs;
        let distant_land = &mut self.distant_land;
        let plan = get_vec_mut_from::<crate::abi::ResidencyPlan>(vecs, params.plan)?;
        distant_land.plan_residency(plan, params)
    }

    /// Builds the landscape quadtree from `terrain.bin` and the shared buffer handles.
    fn init_landscape(&mut self) -> Result<(), HostError> {
        let mut params = unsafe { self.params().params.init_landscape_params };
        let terrain_path = {
            let output = self
                .output_state
                .lock()
                .map_err(|_| HostError::init("Output state lock poisoned"))?;
            output
                .snapshot
                .as_ref()
                .filter(|snapshot| snapshot.terrain_available())
                .map(|snapshot| snapshot.paths().terrain_path.clone())
                .ok_or_else(|| HostError::init("Pinned terrain output is unavailable"))?
        };
        let result = {
            let vecs = &mut self.vecs;
            let distant_land = &mut self.distant_land;
            let vec = get_vec_mut_from::<crate::abi::LandscapeBuffers>(vecs, params.buffers)?;
            distant_land.init_landscape(vec, params.terrain_sort_token, &terrain_path)
        };
        params.success = result.is_ok() as u32;
        self.params_mut().params.init_landscape_params = params;
        result
    }

    fn query_output_status(&mut self) {
        let (status, configuration) = match self.output_state.lock() {
            Ok(output) if output.snapshot.is_some() => (1, output.configuration),
            Ok(output) if output.failed => (2, output.configuration),
            Ok(output) => (0, output.configuration),
            Err(_) => (2, None),
        };
        if let Some(configuration) = configuration {
            // Startup generation may only mutate the session distant-land enable bit.
            // Keep live horizon-derived state intact rather than replacing the full snapshot.
            self.distant_land.configuration.mge_flags = configuration.mge_flags;
        }
        self.params_mut().params.output_status_params.status = status;
    }

    /// Switches the active world space used by visibility queries.
    fn set_world_space(&mut self) {
        let mut params = unsafe { self.params().params.world_space_params };
        let name = bytes_from_fixed(&params.cellname);
        params.cell_found = self.distant_land.set_current_world_space(name) as u8;
        self.params_mut().params.world_space_params = params;
    }

    /// Handles `Command::GetVisibleMeshesCoarse`.
    fn get_visible_meshes_coarse(&mut self) -> Result<(), HostError> {
        let params = unsafe { self.params().params.mesh_params };
        let distant_land = &self.distant_land;
        let vecs = &mut self.vecs;
        let scratch = &mut self.sort_scratch;
        let output = get_vec_mut_from::<RenderMesh>(vecs, params.visible_set)?;
        distant_land.get_visible_meshes_coarse(
            output,
            scratch,
            &params.view_frustum,
            VisibleSetSort::from_raw_lossy(params.sort),
            params.set_flags,
        )
    }

    /// Handles `Command::GetVisibleMeshes`.
    fn get_visible_meshes(&mut self) -> Result<(), HostError> {
        let params = unsafe { self.params().params.mesh_params };
        if params.set_flags & VIS_STATIC != 0 {
            self.distant_land.prepare_horizon(params.view_sphere);
        }
        let distant_land = &self.distant_land;
        let vecs = &mut self.vecs;
        let scratch = &mut self.sort_scratch;
        let output = get_vec_mut_from::<RenderMesh>(vecs, params.visible_set)?;
        let bands = if params.near_static_end.is_finite()
            && params.far_static_end.is_finite()
            && params.near_static_end > 0.0
            && params.far_static_end > 0.0
        {
            Some(TierBands {
                near_end: params.near_static_end,
                far_end: params.far_static_end,
            })
        } else {
            None
        };
        let stats = distant_land.get_visible_meshes(
            output,
            scratch,
            &params.view_frustum,
            params.view_sphere,
            bands,
            VisibleSetSort::from_raw_lossy(params.sort),
            params.set_flags,
        )?;
        if params.set_flags & VIS_STATIC != 0 && self.distant_land.horizon_culling_enabled() {
            self.distant_land.accumulate_horizon_frame_stats(params.view_sphere, stats);
        }
        Ok(())
    }

    /// Handles `Command::SetHorizonConfig`: applies live horizon-culling tuning from the client.
    fn set_horizon_config(&mut self) -> Result<(), HostError> {
        let params = unsafe { self.params().params.horizon_config_params };
        self.distant_land.apply_horizon_config(params)
    }

    /// Handles `Command::SortVisibleSet`.
    fn sort_visible_set(&mut self) -> Result<(), HostError> {
        let params = unsafe { self.params().params.mesh_params };
        let distant_land = &mut self.distant_land;
        let vecs = &mut self.vecs;
        let scratch = &mut self.sort_scratch;
        let output = get_vec_mut_from::<RenderMesh>(vecs, params.visible_set)?;
        distant_land.sort_visible_set(output, scratch, VisibleSetSort::from_raw_lossy(params.sort));
        Ok(())
    }

    /// Handles `Command::FinishHorizonFrame`: closes the current render frame for the adaptive
    /// horizon gate. Sent once per rendered frame while horizon culling is enabled, independent of
    /// `SortVisibleSet`, so reflection-only frames (main distant statics disabled) still tick the gate
    /// and stats never bleed across a real frame boundary.
    fn finish_horizon_frame(&mut self) -> Result<(), HostError> {
        self.distant_land.finish_horizon_frame();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::abi::{AllocVecParameters, Parameters};
    use crate::win::{commit_pages, create_reserved_mapping};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn test_server() -> Server {
        let shared_mem = OwnedHandle(create_reserved_mapping(size_of::<Parameters>() as u32).unwrap());
        let mapped = map_view(shared_mem.raw(), 0, size_of::<Parameters>()).unwrap();
        commit_pages(mapped, size_of::<Parameters>()).unwrap();
        let mut view = MappedView::new(mapped, size_of::<Parameters>()).unwrap();
        let ptr = NonNull::new(view.as_bytes_mut().as_mut_ptr().cast::<Parameters>()).unwrap();
        unsafe {
            ptr.as_ptr().write(Parameters::default());
        }
        Server {
            shared_mem,
            client_process: OwnedHandle(std::ptr::null_mut()),
            rpc_start_event: OwnedHandle(std::ptr::null_mut()),
            rpc_complete_event: OwnedHandle(std::ptr::null_mut()),
            parameters: Some(MappedParams { _view: view, ptr }),
            vecs: vec![None],
            free_vecs: VecDeque::from([0]),
            distant_land: DistantLandState::new(Configuration::default()),
            sort_scratch: Vec::new(),
            output_state: Arc::new(Mutex::new(OutputState::default())),
        }
    }

    fn test_alloc_request() -> AllocVecParameters {
        AllocVecParameters {
            max_capacity_in_elements: 8,
            window_size_in_elements: 1,
            element_size: std::mem::size_of::<u32>() as u32,
            initial_capacity: 0,
            ..AllocVecParameters::default()
        }
    }

    #[test]
    fn alloc_vec_returns_recycled_id_after_create_failure() {
        let mut server = test_server();
        server.params_mut().params.alloc_vec_params = test_alloc_request();

        assert!(
            server
                .alloc_vec_with(|_, _, _| Err(HostError::init("expected failure")))
                .is_err()
        );
        assert_eq!(server.free_vecs.front().copied(), Some(0));
        assert_eq!(
            unsafe { server.params().params.alloc_vec_params.id },
            crate::abi::INVALID_VECTOR
        );

        server.params_mut().params.alloc_vec_params = test_alloc_request();
        let mut allocated_id = None;
        server
            .alloc_vec_with(|id, _, request| {
                allocated_id = Some(id);
                SharedVec::create(
                    id,
                    unsafe { windows_sys::Win32::System::Threading::GetCurrentProcess() },
                    request,
                )
            })
            .unwrap();

        assert_eq!(allocated_id, Some(0));
        assert!(server.free_vecs.is_empty());
        assert!(server.vecs[0].is_some());
    }

    #[test]
    fn missing_output_root_does_not_yield_a_session_snapshot() {
        let missing = std::env::temp_dir().join(format!(
            "mgehost_output_missing_{}_{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&missing);
        assert!(crate::open_session_output_snapshot(&missing, std::time::Duration::ZERO).is_none());
    }

    #[test]
    fn future_version_byte_does_not_yield_a_session_snapshot() {
        let root = std::env::temp_dir().join(format!(
            "mgehost_output_future_{}_{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        let distantland = root.join("distantland");
        fs::create_dir_all(&distantland).unwrap();
        fs::write(distantland.join("version"), [distantland::MGE_DL_VERSION + 1]).unwrap();
        assert!(crate::open_session_output_snapshot(&root, std::time::Duration::ZERO).is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
