use std::sync::Arc;

use hashbrown::{HashMap, HashSet};
use tracing::{trace, warn};

use crate::abi::{
    CellName, D3dxVector3, D3dxVector4, DynVisFlag, EscapedName, PlanResidencyParameters, RenderMesh, ResidencyCommit,
    ResidencyCommitState, ResidencyPlan, ResidencyPlanAction, SetHorizonConfigParameters, VIS_FAR, VIS_GRASS, VIS_LAND,
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

/// Host-side state for one client-owned VB/IB resource.
#[derive(Default)]
pub struct ResidencyResource {
    pub geometry_bytes: u64,
    pub streamable: bool,
    pub resident: bool,
    pub unavailable: bool,
    pub center: D3dxVector3,
    pub mesh_refs: Vec<DynamicMeshRef>,
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
    pub residency_resources: Vec<ResidencyResource>,
    residency_buckets: HashMap<(i32, i32), Vec<u32>>,
    residency_offsets: Vec<(i32, i32)>,
    residency_radius_cells: i32,
    planner_order: Vec<u32>,
    planner_order_scratch: Vec<u32>,
    planner_heading_bin: Option<u8>,
    planner_epoch: Option<u32>,
    planner_cell: Option<(i32, i32)>,
    planner_offset_cursor: usize,
    planner_bucket_cursor: usize,
    /// Whether the in-progress sweep of `residency_offsets` has emitted an admission.
    /// A sweep that admitted something is rewound at its end so resources the client
    /// dropped get re-offered; a sweep that admitted nothing leaves the cursor parked,
    /// because re-offering the same unadmittable resources every frame is a tight retry.
    planner_sweep_admitted: bool,
    /// Resource ids of every streamable resource the client currently holds resident: the
    /// complete set of eviction candidates. `residency_resources` carries one entry per
    /// subset — hundreds of thousands on a full load order, nearly all of them ordinary
    /// statics that are never streamed — so it is far too large to search for one.
    resident_streamable: HashSet<u32>,
    oversize_logged: HashSet<u32>,
    pub land_quadtree: QuadTree,
    horizon: HorizonRuntime,
}

/// Decodes wire heading parameter: 0 = no hint, 1..=32 = bins 0..=31, others = invalid (None).
fn decode_heading_bin(wire_val: u32) -> Option<u8> {
    if (1..=32).contains(&wire_val) {
        Some((wire_val - 1) as u8)
    } else {
        None
    }
}

/// Decodes a 32-bin heading index (0..=31) into a horizontal unit direction vector.
/// Paired with C++ quantizeViewHeadingBin in d3d8/cpp/mge/distantinit.cpp.
/// Bin b covers [b * 2pi/32, (b+1) * 2pi/32). Reconstruct heading from the
/// bin centre (b + 0.5) * 2pi/32 to avoid a 5.6-degree bias against the camera.
fn heading_vector(bin: u8) -> (f32, f32) {
    let angle = (bin as f32 + 0.5) * (std::f32::consts::TAU / 32.0);
    (angle.cos(), angle.sin())
}

/// Tests whether a cell offset is in the camera-forward half-plane.
/// Strictly positive: perpendicular cells and (0,0) are not forward.
fn is_offset_forward(offset: (i32, i32), heading: (f32, f32)) -> bool {
    (offset.0 as f32 * heading.0 + offset.1 as f32 * heading.1) > 0.0
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
            residency_resources: Vec::new(),
            residency_buckets: HashMap::new(),
            residency_offsets: Vec::new(),
            residency_radius_cells: -1,
            planner_order: Vec::new(),
            planner_order_scratch: Vec::new(),
            planner_heading_bin: None,
            planner_epoch: None,
            planner_cell: None,
            planner_offset_cursor: 0,
            planner_bucket_cursor: 0,
            planner_sweep_admitted: false,
            resident_streamable: HashSet::new(),
            oversize_logged: HashSet::new(),
            land_quadtree: QuadTree::default(),
            horizon,
        }
    }

    /// Applies one acknowledged client resource transition to every quadtree placement.
    pub fn apply_residency_commit(&mut self, commit: ResidencyCommit) -> Result<(), HostError> {
        let resource_id = commit.resource_id as usize;
        let Some(resource) = self.residency_resources.get(resource_id) else {
            return Err(HostError::listen(format!(
                "Residency resource {} not found",
                commit.resource_id
            )));
        };
        let refs = resource.mesh_refs.clone();
        match commit.state {
            s if s == ResidencyCommitState::Unloaded as u32 || s == ResidencyCommitState::Unavailable as u32 => {
                for reference in refs {
                    let mesh = self.world_spaces[reference.world]
                        .tree_mut(reference.tree)
                        .mesh_mut(reference.mesh);
                    mesh.resident = false;
                    mesh.render_mesh.v_buffer = 0;
                    mesh.render_mesh.i_buffer = 0;
                    mesh.render_mesh.faces = 0;
                    mesh.far_faces = 0;
                    mesh.very_far_faces = 0;
                }
                let resource = &mut self.residency_resources[resource_id];
                resource.resident = false;
                resource.unavailable = commit.state == ResidencyCommitState::Unavailable as u32;
                self.resident_streamable.remove(&commit.resource_id);
            }
            s if s == ResidencyCommitState::Resident as u32 => {
                if commit.vbuffer == 0 || commit.ibuffer == 0 {
                    return Err(HostError::listen("Resident commit supplied null buffer pointers"));
                }
                for reference in refs {
                    let mesh = self.world_spaces[reference.world]
                        .tree_mut(reference.tree)
                        .mesh_mut(reference.mesh);
                    mesh.render_mesh.v_buffer = commit.vbuffer;
                    mesh.render_mesh.i_buffer = commit.ibuffer;
                    mesh.render_mesh.faces = mesh.near_faces;
                    mesh.far_faces = mesh.retained_far_faces;
                    mesh.very_far_faces = mesh.retained_very_far_faces;
                    mesh.resident = true;
                }
                let resource = &mut self.residency_resources[resource_id];
                resource.resident = true;
                resource.unavailable = false;
                if resource.streamable {
                    self.resident_streamable.insert(commit.resource_id);
                }
            }
            state => return Err(HostError::listen(format!("Unknown residency commit state {state}"))),
        }
        Ok(())
    }

    /// Rebuilds the exterior cell buckets after static initialization.
    pub(super) fn rebuild_residency_index(&mut self) {
        const CELL_SIZE: f32 = 8192.0;
        self.residency_buckets.clear();
        self.resident_streamable.clear();
        for (resource_id, resource) in self.residency_resources.iter().enumerate() {
            if !resource.streamable {
                continue;
            }
            if resource.resident {
                self.resident_streamable.insert(resource_id as u32);
            }
            let cell = (
                (resource.center.x / CELL_SIZE).floor() as i32,
                (resource.center.y / CELL_SIZE).floor() as i32,
            );
            self.residency_buckets.entry(cell).or_default().push(resource_id as u32);
        }
        self.planner_epoch = None;
        self.planner_cell = None;
        self.planner_offset_cursor = 0;
        self.planner_bucket_cursor = 0;
        self.planner_sweep_admitted = false;
        self.oversize_logged.clear();
        self.planner_heading_bin = None;
        self.planner_order_scratch.clear();
        self.planner_order_scratch.reserve(self.residency_offsets.len());
        self.repartition(0);
    }

    fn ensure_residency_offsets(&mut self, radius: f32) {
        const CELL_SIZE: f32 = 8192.0;
        let radius_cells = (radius / CELL_SIZE).ceil().max(0.0) as i32;
        if self.residency_radius_cells == radius_cells {
            return;
        }
        self.residency_radius_cells = radius_cells;
        self.residency_offsets.clear();
        for y in -radius_cells..=radius_cells {
            for x in -radius_cells..=radius_cells {
                if x * x + y * y <= radius_cells * radius_cells {
                    self.residency_offsets.push((x, y));
                }
            }
        }
        self.residency_offsets.sort_unstable_by_key(|&(x, y)| (x * x + y * y, y, x));
        self.planner_offset_cursor = 0;
        self.planner_bucket_cursor = 0;
        self.planner_sweep_admitted = false;
        self.planner_order.clear();
        self.planner_order_scratch.clear();
        self.planner_order.reserve(self.residency_offsets.len());
        self.planner_order_scratch.reserve(self.residency_offsets.len());
        self.planner_heading_bin = None;
        self.repartition(0);
    }

    fn repartition(&mut self, first_reorderable: usize) {
        let len = self.residency_offsets.len();
        if self.planner_offset_cursor >= len || first_reorderable >= len {
            return;
        }

        let heading_dir = self.planner_heading_bin.map(heading_vector);

        if first_reorderable == 0 {
            self.planner_order.clear();
            if let Some(heading) = heading_dir {
                for (idx, &offset) in self.residency_offsets.iter().enumerate() {
                    if is_offset_forward(offset, heading) {
                        self.planner_order.push(idx as u32);
                    }
                }
                for (idx, &offset) in self.residency_offsets.iter().enumerate() {
                    if !is_offset_forward(offset, heading) {
                        self.planner_order.push(idx as u32);
                    }
                }
            } else {
                self.planner_order.extend(0..len as u32);
            }
        } else if let Some(heading) = heading_dir {
            self.planner_order_scratch.clear();
            for &idx in &self.planner_order[first_reorderable..] {
                let offset = self.residency_offsets[idx as usize];
                if is_offset_forward(offset, heading) {
                    self.planner_order_scratch.push(idx);
                }
            }
            for &idx in &self.planner_order[first_reorderable..] {
                let offset = self.residency_offsets[idx as usize];
                if !is_offset_forward(offset, heading) {
                    self.planner_order_scratch.push(idx);
                }
            }
            self.planner_order[first_reorderable..].copy_from_slice(&self.planner_order_scratch);
        }
    }

    /// Flips one resource's residency the way an acknowledged commit would, without needing
    /// quadtree placements to update.
    #[cfg(test)]
    fn set_resident_for_tests(&mut self, resource_id: u32, resident: bool) {
        self.residency_resources[resource_id as usize].resident = resident;
        if resident {
            self.resident_streamable.insert(resource_id);
        } else {
            self.resident_streamable.remove(&resource_id);
        }
    }

    fn distance_sq(resource: &ResidencyResource, center: D3dxVector3) -> f64 {
        let dx = f64::from(resource.center.x - center.x);
        let dy = f64::from(resource.center.y - center.y);
        dx * dx + dy * dy
    }

    /// Picks the resident streamable resource furthest from `center`, ignoring anything inside
    /// `retain_radius`. The scan is exact rather than windowed: the candidate set is bounded by
    /// the streaming byte cap, not by the size of the load order.
    fn farthest_replaceable(&self, center: D3dxVector3, retain_radius: f32) -> Option<(u32, f64)> {
        let retain_sq = f64::from(retain_radius) * f64::from(retain_radius);
        let mut farthest: Option<(u32, f64)> = None;
        for &id in &self.resident_streamable {
            let distance = Self::distance_sq(&self.residency_resources[id as usize], center);
            if distance <= retain_sq {
                continue;
            }
            // Break ties on the lower id so the choice does not follow set iteration order.
            if farthest.is_none_or(|(best_id, best)| distance > best || (distance == best && id < best_id)) {
                farthest = Some((id, distance));
            }
        }
        farthest
    }

    /// Advances the bounded radial residency planner and writes requests to `output`.
    pub fn plan_residency(&mut self, output: &mut SharedVec, params: PlanResidencyParameters) -> Result<(), HostError> {
        const CELL_SIZE: f32 = 8192.0;
        output.reset();
        let center = D3dxVector3 {
            x: params.center_x,
            y: params.center_y,
            z: params.center_z,
        };
        if !center.x.is_finite()
            || !center.y.is_finite()
            || !center.z.is_finite()
            || !params.admission_radius.is_finite()
            || !params.retain_radius.is_finite()
        {
            return Err(HostError::listen("Residency planner received non-finite input"));
        }
        let exterior = self.world_space_indices.get(CellName::EXTERIOR.as_bytes()).copied();
        if self.current_world_space.is_none() || self.current_world_space != exterior {
            return Ok(());
        }

        self.ensure_residency_offsets(params.admission_radius);
        let cell = ((center.x / CELL_SIZE).floor() as i32, (center.y / CELL_SIZE).floor() as i32);
        let incoming_heading = decode_heading_bin(params.view_heading_bin);

        if self.planner_epoch != Some(params.plan_epoch) || self.planner_cell != Some(cell) {
            self.planner_epoch = Some(params.plan_epoch);
            self.planner_cell = Some(cell);
            self.planner_offset_cursor = 0;
            self.planner_bucket_cursor = 0;
            self.planner_sweep_admitted = false;
            self.planner_heading_bin = incoming_heading;
            self.repartition(0);
        } else if let Some(heading) = incoming_heading
            && self.planner_heading_bin != Some(heading)
        {
            self.planner_heading_bin = Some(heading);
            // Pin the current offset unconditionally: planner_bucket_cursor == 0 can mean an
            // untouched cell or a candidate deliberately rolled back after an eviction.
            // repartition ignores a parked cursor, so no extra guard is needed here.
            let first_reorderable = (self.planner_offset_cursor + 1).min(self.residency_offsets.len());
            self.repartition(first_reorderable);
        }

        let resource_limit = params.max_resources.max(1) as usize;
        if params.cap_debt_bytes != 0
            && let Some((resource_id, _)) = self.farthest_replaceable(center, 0.0)
        {
            output.push(ResidencyPlan {
                resource_id,
                action: ResidencyPlanAction::Evict as u32,
                plan_epoch: params.plan_epoch,
                reserved: 0,
            })?;
            return Ok(());
        }

        let admission_sq = f64::from(params.admission_radius) * f64::from(params.admission_radius);
        let cap_bytes = params.cap_bytes;
        let retain_radius = params.retain_radius;
        let mut available = params.available_bytes;
        let mut cells_visited = 0usize;
        let mut resources_visited = 0usize;
        while self.planner_offset_cursor < self.residency_offsets.len()
            && cells_visited < params.max_cells.max(1) as usize
            && resources_visited < resource_limit
        {
            let offset_index = self.planner_order[self.planner_offset_cursor] as usize;
            let offset = self.residency_offsets[offset_index];
            let bucket_key = (cell.0 + offset.0, cell.1 + offset.1);
            let bucket_len = self.residency_buckets.get(&bucket_key).map_or(0, Vec::len);
            while self.planner_bucket_cursor < bucket_len && resources_visited < resource_limit {
                let resource_id = self.residency_buckets[&bucket_key][self.planner_bucket_cursor];
                self.planner_bucket_cursor += 1;
                resources_visited += 1;
                let resource = &self.residency_resources[resource_id as usize];
                if resource.resident || resource.unavailable || Self::distance_sq(resource, center) > admission_sq {
                    continue;
                }
                if resource.geometry_bytes > cap_bytes {
                    if self.oversize_logged.insert(resource_id) {
                        warn!(
                            resource_id,
                            bytes = resource.geometry_bytes,
                            cap = cap_bytes,
                            "Merged static resource exceeds the total streaming cap"
                        );
                    }
                    continue;
                }
                if resource.geometry_bytes <= available {
                    output.push(ResidencyPlan {
                        resource_id,
                        action: ResidencyPlanAction::Admit as u32,
                        plan_epoch: params.plan_epoch,
                        reserved: 0,
                    })?;
                    available -= resource.geometry_bytes;
                    self.planner_sweep_admitted = true;
                    continue;
                }
                let candidate_distance = Self::distance_sq(resource, center);
                if let Some((evict_id, evict_distance)) = self.farthest_replaceable(center, retain_radius)
                    && candidate_distance < evict_distance
                {
                    // The eviction is made for this candidate, so leave the cursor on it. The
                    // client commits the removal at the next frame's stage 0 and the following
                    // sweep call admits the candidate into the freed budget. Advancing past it
                    // would strand both the candidate and the headroom it just bought until the
                    // player crosses a cell, since an evict-only sweep never sets the rewind flag.
                    self.planner_bucket_cursor -= 1;
                    output.push(ResidencyPlan {
                        resource_id: evict_id,
                        action: ResidencyPlanAction::Evict as u32,
                        plan_epoch: params.plan_epoch,
                        reserved: 0,
                    })?;
                    return Ok(());
                }
            }
            if self.planner_bucket_cursor >= bucket_len {
                self.planner_bucket_cursor = 0;
                self.planner_offset_cursor += 1;
                cells_visited += 1;
            }
        }

        // The client admits far fewer resources per frame than one sweep offers and silently
        // drops the surplus once its ready queue fills, so a single pass leaves most of the
        // draw distance unadmitted. Rewind only after a sweep that admitted something: once
        // everything in range is resident, or the cap refuses the rest, the next sweep admits
        // nothing and the cursor parks until the player crosses a cell.
        if self.planner_offset_cursor >= self.residency_offsets.len() && self.planner_sweep_admitted {
            self.planner_offset_cursor = 0;
            self.planner_bucket_cursor = 0;
            self.planner_sweep_admitted = false;
            self.repartition(0);
        }
        Ok(())
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
