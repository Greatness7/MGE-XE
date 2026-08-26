use std::sync::Arc;

use hashbrown::HashMap;
use tracing::trace;

#[cfg(test)]
use crate::abi::D3dxVector3;
use crate::abi::{
    CellName, D3dxVector4, DynVisFlag, EscapedName, RenderMesh, SetHorizonConfigParameters, VIS_FAR, VIS_GRASS, VIS_LAND,
    VIS_NEAR, VIS_VERY_FAR, ViewFrustum, VisibleSetSort,
};
use crate::config::Configuration;
use crate::error::HostError;
use crate::ipc::shared_vec::SharedVec;
#[cfg(test)]
use crate::state::horizon::HorizonParams;
use crate::state::horizon::{HorizonCullStats, HorizonRuntime, HorizonTable, TerrainHeightField};
#[cfg(test)]
use crate::state::horizon::{MAX_PENDING_FRAMES, MAX_STALE_DISTANCE};
use crate::state::quadtree::{MeshId, MeshSink, QuadTree, TierBands};

struct SharedVecMeshSink<'a> {
    output: &'a mut SharedVec,
}

impl MeshSink for SharedVecMeshSink<'_> {
    fn push_mesh(&mut self, mesh: RenderMesh) -> Result<(), HostError> {
        self.output.push(mesh)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StaticTreeKind {
    Near,
    Far,
    VeryFar,
    Grass,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DynamicMeshRef {
    pub(super) world: usize,
    pub(super) tree: StaticTreeKind,
    pub(super) mesh: MeshId,
}

#[derive(Default)]
pub struct WorldSpace {
    pub near_statics: QuadTree,
    pub far_statics: QuadTree,
    pub very_far_statics: QuadTree,
    pub grass_statics: QuadTree,
}

impl WorldSpace {
    pub(super) fn tree_mut(&mut self, kind: StaticTreeKind) -> &mut QuadTree {
        match kind {
            StaticTreeKind::Near => &mut self.near_statics,
            StaticTreeKind::Far => &mut self.far_statics,
            StaticTreeKind::VeryFar => &mut self.very_far_statics,
            StaticTreeKind::Grass => &mut self.grass_statics,
        }
    }
}

pub struct DistantLandState {
    pub(crate) configuration: Configuration,
    pub dynamic_vis_groups: Vec<Vec<DynamicMeshRef>>,
    pub world_spaces: Vec<WorldSpace>,
    /// World spaces keyed by their undecoded engine name; see [`CellName`].
    pub world_space_indices: HashMap<CellName, usize>,
    pub current_world_space: Option<usize>,
    pub land_quadtree: QuadTree,
    horizon: HorizonRuntime,
}

impl DistantLandState {
    pub fn new(configuration: Configuration) -> Self {
        let horizon = HorizonRuntime::new(configuration.horizon_adaptive_gate);
        Self {
            configuration,
            dynamic_vis_groups: Vec::new(),
            world_spaces: Vec::new(),
            world_space_indices: HashMap::new(),
            current_world_space: None,
            land_quadtree: QuadTree::default(),
            horizon,
        }
    }

    pub fn prepare_horizon(&mut self, view_sphere: D3dxVector4) {
        self.horizon.prepare(view_sphere, &self.configuration);
    }

    pub(super) fn replace_height_field(&mut self, field: Option<Arc<TerrainHeightField>>) {
        self.horizon.replace_height_field(field);
    }

    pub(super) fn install_rebuilt_height_field(&mut self, field: Arc<TerrainHeightField>) {
        self.horizon.install_rebuilt_height_field(field);
    }

    pub fn apply_horizon_config(&mut self, params: SetHorizonConfigParameters) -> Result<(), HostError> {
        self.configuration.horizon_culling = params.enabled != 0;
        self.configuration.horizon_bias_z = params.bias_z;
        self.configuration.horizon_object_bias_z = params.object_bias_z;
        self.configuration.horizon_near_units = params.near_units;
        self.configuration.horizon_ring_step = params.ring_step;
        self.configuration.horizon_max_range = params.max_range;
        self.configuration.horizon_bins = params.bins;
        self.configuration.horizon_sample_spacing = params.sample_spacing;
        self.configuration.horizon_adaptive_gate = params.adaptive_gate != 0;
        self.configuration.clamp_horizon();
        self.horizon.apply_gate_config(self.configuration.horizon_adaptive_gate);

        if !self.configuration.horizon_culling {
            self.replace_height_field(None);
            return Ok(());
        }
        if self.height_field_needs_build() {
            self.build_height_field()?;
        } else {
            self.horizon.invalidate_for_config_change();
        }
        Ok(())
    }

    pub fn horizon_culling_enabled(&self) -> bool {
        self.configuration.horizon_culling
    }

    pub fn accumulate_horizon_frame_stats(&mut self, view_sphere: D3dxVector4, stats: HorizonCullStats) {
        self.horizon.accumulate_frame_stats(view_sphere, stats);
    }

    pub fn finish_horizon_frame(&mut self) {
        self.horizon.finish_frame();
    }

    #[cfg(test)]
    pub fn horizon_gate_state_code(&self) -> u32 {
        self.horizon.gate_state_code()
    }

    fn height_field_needs_build(&self) -> bool {
        self.configuration.horizon_culling && self.horizon.height_field_is_none()
    }

    pub fn update_dyn_vis_one(&mut self, update: DynVisFlag) {
        let Some(group) = self.dynamic_vis_groups.get(update.group_index as usize) else {
            return;
        };
        for reference in group {
            if let Some(world_space) = self.world_spaces.get_mut(reference.world) {
                world_space
                    .tree_mut(reference.tree)
                    .mesh_mut(reference.mesh)
                    .render_mesh
                    .enabled = update.enable;
            }
        }
    }

    /// Sets the world space used by subsequent visibility queries.
    ///
    /// Returns `true` when `name` matches a loaded world space.
    pub fn set_current_world_space(&mut self, name: &[u8]) -> bool {
        if let Some(&world_space_index) = self.world_space_indices.get(name) {
            self.current_world_space = Some(world_space_index);
            true
        } else {
            trace!(
                "World space '{}' was not found in the distant statics cache",
                EscapedName(name)
            );
            self.current_world_space = None;
            false
        }
    }

    /// Appends meshes visible in `view_frustum` without applying distance culling.
    ///
    /// # Errors
    ///
    /// Propagates any error returned by the shared output vector.
    pub fn get_visible_meshes_coarse(
        &self,
        output: &mut SharedVec,
        scratch: &mut Vec<RenderMesh>,
        view_frustum: &ViewFrustum,
        sort: VisibleSetSort,
        set_flags: u32,
    ) -> Result<(), HostError> {
        // Coarse queries carry no view sphere, so no horizon is applied and no stats accrue.
        self.collect_visible_meshes(output, scratch, view_frustum, None, None, sort, set_flags)
            .map(|_| ())
    }

    /// Appends meshes visible in `view_frustum` and within `view_sphere`.
    ///
    /// # Errors
    ///
    /// Propagates any error returned by the shared output vector.
    pub fn get_visible_meshes(
        &self,
        output: &mut SharedVec,
        scratch: &mut Vec<RenderMesh>,
        view_frustum: &ViewFrustum,
        view_sphere: D3dxVector4,
        bands: Option<TierBands>,
        sort: VisibleSetSort,
        set_flags: u32,
    ) -> Result<HorizonCullStats, HostError> {
        self.collect_visible_meshes(output, scratch, view_frustum, Some(view_sphere), bands, sort, set_flags)
    }

    /// Shared implementation for the visible-mesh RPC commands.
    fn collect_visible_meshes(
        &self,
        output: &mut SharedVec,
        scratch: &mut Vec<RenderMesh>,
        view_frustum: &ViewFrustum,
        view_sphere: Option<D3dxVector4>,
        bands: Option<TierBands>,
        sort: VisibleSetSort,
        set_flags: u32,
    ) -> Result<HorizonCullStats, HostError> {
        let Some(world_space_index) = self.current_world_space else {
            // Rust keeps this guard instead of mirroring the C++ null dereference; missing
            // world-space state safely produces no meshes until the client selects a valid one.
            return Ok(HorizonCullStats::default());
        };
        let Some(world_space) = self.world_spaces.get(world_space_index) else {
            return Ok(HorizonCullStats::default());
        };

        // Terrain horizon data describes the exterior world space only. Interior world spaces may
        // share its coordinate range, but must never use its terrain horizon.
        let horizon = if self.world_space_indices.get(CellName::EXTERIOR.as_bytes()).copied() == Some(world_space_index) {
            view_sphere.and(self.horizon.table())
        } else {
            None
        };
        let mut stats = HorizonCullStats::default();

        // Only use write signaling for unsorted paths. When sorting is required, all elements must be
        // collected first before sorting can occur, so parallel incremental reading by the 32-bit client
        // isn't possible anyway. For unsorted paths, write signaling enables the client to process
        // elements as they're added (parallel producer-consumer).
        if sort == VisibleSetSort::None {
            output.start_write();
            let traversal_result = {
                let mut sink = SharedVecMeshSink { output };
                self.collect_from_flags(
                    world_space,
                    &mut sink,
                    view_frustum,
                    view_sphere,
                    bands,
                    horizon,
                    set_flags,
                    &mut stats,
                )
            };
            let end_write_result = output.end_write();
            traversal_result?;
            end_write_result?;
            return Ok(stats);
        }

        // Preserve append semantics by including any existing output in the persistent sort buffer.
        // Sorting before the final write avoids a temporary Vec and an extra shared-memory round trip.
        let existing_size = output.size();
        scratch.clear();
        scratch.reserve(existing_size as usize);
        output
            .for_each(|mesh| scratch.push(mesh))
            .expect("reading existing render mesh data must succeed");
        self.collect_from_flags(
            world_space,
            scratch,
            view_frustum,
            view_sphere,
            bands,
            horizon,
            set_flags,
            &mut stats,
        )?;

        let final_size = u32::try_from(scratch.len())
            .map_err(|_| HostError::init("Visible mesh count exceeds shared vector size limit"))?;
        output.reserve(final_size)?;
        match sort {
            VisibleSetSort::ByState => scratch.sort_by(RenderMesh::compare_by_state),
            VisibleSetSort::ByTexture => scratch.sort_by(RenderMesh::compare_by_texture),
            VisibleSetSort::None => unreachable!("the unsorted path returns above"),
        }
        for (index, mesh) in scratch.iter().copied().enumerate() {
            let index = index as u32;
            if index < existing_size {
                output.set(index, mesh);
            } else {
                output.push(mesh)?;
            }
        }
        Ok(stats)
    }

    /// Dispatches traversal to the quadtrees selected by `set_flags`.
    fn collect_from_flags<S: MeshSink>(
        &self,
        world_space: &WorldSpace,
        sink: &mut S,
        view_frustum: &ViewFrustum,
        view_sphere: Option<D3dxVector4>,
        bands: Option<TierBands>,
        horizon: Option<&HorizonTable>,
        set_flags: u32,
        stats: &mut HorizonCullStats,
    ) -> Result<(), HostError> {
        if set_flags & VIS_NEAR != 0 {
            collect_quadtree_meshes(
                &world_space.near_statics,
                sink,
                view_frustum,
                view_sphere,
                bands,
                horizon,
                stats,
            )?;
        }
        if set_flags & VIS_FAR != 0 {
            collect_quadtree_meshes(
                &world_space.far_statics,
                sink,
                view_frustum,
                view_sphere,
                bands,
                horizon,
                stats,
            )?;
        }
        if set_flags & VIS_VERY_FAR != 0 {
            collect_quadtree_meshes(
                &world_space.very_far_statics,
                sink,
                view_frustum,
                view_sphere,
                bands,
                horizon,
                stats,
            )?;
        }
        if set_flags & VIS_GRASS != 0 {
            collect_quadtree_meshes(
                &world_space.grass_statics,
                sink,
                view_frustum,
                view_sphere,
                None,
                None,
                &mut HorizonCullStats::default(),
            )?;
        }
        if set_flags & VIS_LAND != 0 {
            collect_quadtree_meshes(
                &self.land_quadtree,
                sink,
                view_frustum,
                view_sphere,
                None,
                None,
                &mut HorizonCullStats::default(),
            )?;
        }
        Ok(())
    }

    /// Sorts the existing visible set in place.
    pub fn sort_visible_set(&self, output: &mut SharedVec, scratch: &mut Vec<RenderMesh>, sort: VisibleSetSort) {
        match sort {
            VisibleSetSort::ByState => output.sort_render_meshes(scratch, RenderMesh::compare_by_state),
            VisibleSetSort::ByTexture => output.sort_render_meshes(scratch, RenderMesh::compare_by_texture),
            VisibleSetSort::None => {}
        }
    }
}

/// Calls the appropriate quadtree traversal for the requested precision level.
fn collect_quadtree_meshes<S: MeshSink>(
    tree: &QuadTree,
    sink: &mut S,
    view_frustum: &ViewFrustum,
    view_sphere: Option<D3dxVector4>,
    bands: Option<TierBands>,
    horizon: Option<&HorizonTable>,
    stats: &mut HorizonCullStats,
) -> Result<(), HostError> {
    match view_sphere {
        Some(sphere) => tree.get_visible_meshes_with_bands(view_frustum, sphere, horizon, bands, sink, stats),
        None => tree.get_visible_meshes_coarse(view_frustum, sink),
    }
}

/// Test-only hooks to drive the async build path deterministically.
#[cfg(test)]
impl DistantLandState {
    /// Enables the real async build path for tests.
    fn enable_async_horizon(&mut self) {
        self.horizon.enable_async();
    }

    /// Installs a worker-less builder for deterministic result timing in tests.
    fn install_stalled_horizon_builder(&mut self) {
        self.horizon.install_stalled_builder();
    }

    /// Synchronously processes the newest queued request in tests.
    fn run_worker_once(&self) {
        self.horizon.run_worker_once();
    }

    /// Publishes a result stamped with explicit epochs in tests.
    fn inject_async_result(&self, eye: D3dxVector3, generation: u64, request_id: u64) {
        self.horizon
            .inject_async_result(eye, generation, request_id, &self.configuration);
    }

    /// Waits for the real worker to publish a result in tests.
    fn flush_horizon_worker(&self) {
        self.horizon.flush_worker();
    }

    fn horizon_for_tests(&self) -> &HorizonRuntime {
        &self.horizon
    }

    fn horizon_mut_for_tests(&mut self) -> &mut HorizonRuntime {
        &mut self.horizon
    }
}

#[cfg(test)]
mod tests;
