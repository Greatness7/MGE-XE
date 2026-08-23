mod worker;

use std::sync::Arc;
#[cfg(test)]
use std::thread;
use std::time::Instant;

use crate::abi::{D3dxVector3, D3dxVector4};
use crate::config::Configuration;

use self::worker::{BuildRequest, BuiltTable, HorizonBuilder};
use super::gate::{HorizonGate, HorizonGateMode};
use super::{HorizonCullStats, HorizonParams, HorizonTable, TerrainHeightField};

/// Eye movement beyond this distance forces a synchronous rebuild.
pub(crate) const MAX_STALE_DISTANCE: f32 = 64.0;

/// Maximum prepare calls before an async request falls back to synchronous work.
pub(crate) const MAX_PENDING_FRAMES: u32 = 8;

struct PendingHorizon {
    frames_waited: u32,
}

pub(crate) struct HorizonRuntime {
    height_field: Option<Arc<TerrainHeightField>>,
    cached_horizon: Option<Arc<HorizonTable>>,
    horizon_generation: u64,
    latest_request_id: u64,
    pending_horizon: Option<PendingHorizon>,
    horizon_builder: Option<HorizonBuilder>,
    gate: HorizonGate,
    horizon_frame_stats: HorizonCullStats,
    horizon_frame_eye: Option<D3dxVector3>,
    #[cfg(test)]
    force_synchronous_horizon: bool,
    #[cfg(test)]
    horizon_build_count: u64,
}

impl HorizonRuntime {
    pub(crate) fn new(adaptive_gate: bool) -> Self {
        Self {
            height_field: None,
            cached_horizon: None,
            horizon_generation: 0,
            latest_request_id: 0,
            pending_horizon: None,
            horizon_builder: None,
            gate: HorizonGate::new(adaptive_gate),
            horizon_frame_stats: HorizonCullStats::default(),
            horizon_frame_eye: None,
            #[cfg(test)]
            force_synchronous_horizon: true,
            #[cfg(test)]
            horizon_build_count: 0,
        }
    }

    pub(crate) fn prepare(&mut self, view_sphere: D3dxVector4, configuration: &Configuration) {
        let eye = D3dxVector3 {
            x: view_sphere.x,
            y: view_sphere.y,
            z: view_sphere.z,
        };
        let field = if configuration.horizon_culling {
            self.height_field.clone()
        } else {
            None
        };
        let Some(field) = field.filter(|field| field.contains_xy(eye.x, eye.y)) else {
            self.clear();
            self.gate.on_context_lost();
            return;
        };
        self.gate.on_context_available();

        let params = HorizonParams::from_configuration(*configuration);
        let eye_threshold = configuration.horizon_rebuild_eye_threshold;

        if let Some(pending) = self.pending_horizon.as_mut() {
            pending.frames_waited = pending.frames_waited.saturating_add(1);
        }

        if let Some(built) = self.horizon_builder.as_ref().and_then(HorizonBuilder::take_result) {
            let adoption_threshold = if self.gate.is_warming() {
                MAX_STALE_DISTANCE
            } else {
                eye_threshold
            };
            if self.consider_built_result(built, eye, params, adoption_threshold) {
                self.gate.on_warm_adopted();
            }
        }

        match self.gate.mode() {
            HorizonGateMode::Suspended => {
                self.clear();
                return;
            }
            HorizonGateMode::Warming => {
                self.cached_horizon = None;
                if self.gate.should_post_warm_build() && self.ensure_builder_spawned() {
                    self.post_async_horizon(field, eye, params);
                    self.gate.on_warm_build_posted();
                }
                return;
            }
            HorizonGateMode::Active => {}
        }

        let cache_hit = self
            .cached_horizon
            .as_deref()
            .is_some_and(|cached| horizon_cache_matches(cached, eye, params, eye_threshold));
        if cache_hit {
            return;
        }

        let cold_or_param_change = self
            .cached_horizon
            .as_deref()
            .is_none_or(|cached| !horizon_params_match(cached, params));
        let stale_beyond_cap = self
            .cached_horizon
            .as_deref()
            .is_some_and(|cached| eye_distance_sq(cached.eye, eye) > MAX_STALE_DISTANCE * MAX_STALE_DISTANCE);
        let worker_starved = self
            .pending_horizon
            .as_ref()
            .is_some_and(|pending| pending.frames_waited >= MAX_PENDING_FRAMES);
        let force_sync = self.force_synchronous();

        let build_sync =
            force_sync || cold_or_param_change || stale_beyond_cap || worker_starved || !self.ensure_builder_spawned();

        if build_sync {
            self.sync_build_horizon(&field, eye, params);
        } else {
            self.post_async_horizon(field, eye, params);
        }
    }

    pub(crate) fn table(&self) -> Option<&HorizonTable> {
        self.cached_horizon.as_deref()
    }

    pub(crate) fn replace_height_field(&mut self, field: Option<Arc<TerrainHeightField>>) {
        self.invalidate_epoch();
        self.height_field = field;
        if self.height_field.is_some() {
            self.gate.on_config_change();
        } else {
            self.gate.on_context_lost();
        }
    }

    /// Installs a rebuilt field after invalidation.
    pub(crate) fn install_rebuilt_height_field(&mut self, field: Arc<TerrainHeightField>) {
        self.height_field = Some(field);
    }

    pub(crate) fn height_field_is_none(&self) -> bool {
        self.height_field.is_none()
    }

    pub(crate) fn apply_gate_config(&mut self, enabled: bool) {
        self.gate.set_enabled(enabled);
        self.horizon_frame_eye = None;
        self.horizon_frame_stats = HorizonCullStats::default();
    }

    pub(crate) fn invalidate_for_config_change(&mut self) {
        self.invalidate_epoch();
        self.gate.on_config_change();
    }

    pub(crate) fn accumulate_frame_stats(&mut self, view_sphere: D3dxVector4, stats: HorizonCullStats) {
        self.horizon_frame_eye = Some(D3dxVector3 {
            x: view_sphere.x,
            y: view_sphere.y,
            z: view_sphere.z,
        });
        self.horizon_frame_stats.meshes_culled += stats.meshes_culled;
        self.horizon_frame_stats.nodes_pruned += stats.nodes_pruned;
    }

    pub(crate) fn finish_frame(&mut self) {
        if let Some(eye) = self.horizon_frame_eye.take() {
            self.gate.on_frame(
                Instant::now(),
                eye,
                self.horizon_frame_stats.meshes_culled as u32,
                self.horizon_frame_stats.nodes_pruned as u32,
                MAX_STALE_DISTANCE,
            );
        }
        self.horizon_frame_stats = HorizonCullStats::default();
    }

    fn force_synchronous(&self) -> bool {
        #[cfg(test)]
        {
            self.force_synchronous_horizon
        }
        #[cfg(not(test))]
        {
            false
        }
    }

    fn clear(&mut self) {
        if self.cached_horizon.is_some() || self.pending_horizon.is_some() {
            self.cached_horizon = None;
            self.pending_horizon = None;
            self.latest_request_id = self.latest_request_id.wrapping_add(1);
        }
    }

    fn consider_built_result(
        &mut self,
        built: Arc<BuiltTable>,
        eye: D3dxVector3,
        params: HorizonParams,
        eye_threshold: f32,
    ) -> bool {
        let is_current = built.request_id == self.latest_request_id;
        if is_current {
            self.pending_horizon = None;
        }
        if built.generation != self.horizon_generation {
            return false;
        }

        let result_matches = horizon_cache_matches(&built.table, eye, params, eye_threshold);
        if !is_current && !result_matches {
            return false;
        }

        let adopt = match self.cached_horizon.as_deref() {
            Some(cached) if horizon_cache_matches(cached, eye, params, eye_threshold) => false,
            Some(cached) => {
                let improves = eye_distance_sq(built.table.eye, eye) < eye_distance_sq(cached.eye, eye);
                result_matches || (is_current && improves)
            }
            None => result_matches,
        };
        if adopt {
            self.cached_horizon = Some(Arc::clone(&built.table));
            self.pending_horizon = None;
        }
        adopt
    }

    fn sync_build_horizon(&mut self, field: &Arc<TerrainHeightField>, eye: D3dxVector3, params: HorizonParams) {
        let table = HorizonTable::build(field, eye, params);
        self.cached_horizon = Some(Arc::new(table));
        self.latest_request_id = self.latest_request_id.wrapping_add(1);
        self.pending_horizon = None;
        #[cfg(test)]
        {
            self.horizon_build_count += 1;
        }
    }

    fn post_async_horizon(&mut self, field: Arc<TerrainHeightField>, eye: D3dxVector3, params: HorizonParams) {
        self.latest_request_id = self.latest_request_id.wrapping_add(1);
        let request_id = self.latest_request_id;
        let frames_waited = self.pending_horizon.as_ref().map_or(0, |pending| pending.frames_waited);
        self.pending_horizon = Some(PendingHorizon { frames_waited });
        if let Some(builder) = self.horizon_builder.as_ref() {
            builder.post(BuildRequest {
                field,
                eye,
                params,
                generation: self.horizon_generation,
                request_id,
            });
        }
    }

    fn ensure_builder_spawned(&mut self) -> bool {
        if self.horizon_builder.is_none() {
            self.horizon_builder = HorizonBuilder::spawn();
        }
        self.horizon_builder.is_some()
    }

    fn invalidate_epoch(&mut self) {
        self.cached_horizon = None;
        self.pending_horizon = None;
        self.horizon_generation = self.horizon_generation.wrapping_add(1);
        self.latest_request_id = self.latest_request_id.wrapping_add(1);
    }

    #[cfg(test)]
    pub(crate) fn gate_state_code(&self) -> u32 {
        self.gate.state_code()
    }

    #[cfg(test)]
    pub(crate) fn enable_async(&mut self) {
        self.force_synchronous_horizon = false;
    }

    #[cfg(test)]
    pub(crate) fn install_stalled_builder(&mut self) {
        self.horizon_builder = Some(HorizonBuilder::stalled_for_tests());
        self.force_synchronous_horizon = false;
    }

    #[cfg(test)]
    pub(crate) fn run_worker_once(&self) {
        let builder = self.horizon_builder.as_ref().expect("builder installed");
        builder.run_worker_once();
    }

    #[cfg(test)]
    pub(crate) fn inject_async_result(
        &self,
        eye: D3dxVector3,
        generation: u64,
        request_id: u64,
        configuration: &Configuration,
    ) {
        let params = HorizonParams::from_configuration(*configuration);
        let field = self.height_field.as_ref().expect("field present");
        let table = Arc::new(HorizonTable::build(field, eye, params));
        let builder = self.horizon_builder.as_ref().expect("builder installed");
        builder.publish_result_for_tests(BuiltTable {
            table,
            generation,
            request_id,
        });
    }

    #[cfg(test)]
    pub(crate) fn flush_worker(&self) {
        if let Some(builder) = self.horizon_builder.as_ref() {
            let deadline = Instant::now() + std::time::Duration::from_secs(5);
            while !builder.has_result() {
                assert!(Instant::now() < deadline, "horizon worker did not deliver a result in time");
                thread::yield_now();
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn install_height_field_for_tests(&mut self, field: Option<Arc<TerrainHeightField>>) {
        self.height_field = field;
    }

    #[cfg(test)]
    pub(crate) fn height_field_for_tests(&self) -> &Option<Arc<TerrainHeightField>> {
        &self.height_field
    }

    #[cfg(test)]
    pub(crate) fn cached_horizon_for_tests(&self) -> &Option<Arc<HorizonTable>> {
        &self.cached_horizon
    }

    #[cfg(test)]
    pub(crate) fn generation_for_tests(&self) -> u64 {
        self.horizon_generation
    }

    #[cfg(test)]
    pub(crate) fn request_id_for_tests(&self) -> u64 {
        self.latest_request_id
    }

    #[cfg(test)]
    pub(crate) fn has_pending_for_tests(&self) -> bool {
        self.pending_horizon.is_some()
    }

    #[cfg(test)]
    pub(crate) fn has_builder_for_tests(&self) -> bool {
        self.horizon_builder.is_some()
    }

    #[cfg(test)]
    pub(crate) fn build_count_for_tests(&self) -> u64 {
        self.horizon_build_count
    }

    #[cfg(test)]
    pub(crate) fn frame_eye_for_tests(&self) -> Option<D3dxVector3> {
        self.horizon_frame_eye
    }

    #[cfg(test)]
    pub(crate) fn frame_stats_for_tests(&self) -> &HorizonCullStats {
        &self.horizon_frame_stats
    }

    #[cfg(test)]
    pub(crate) fn gate_context_available_for_tests(&mut self) {
        self.gate.on_context_available();
    }

    #[cfg(test)]
    pub(crate) fn gate_warm_adopted_for_tests(&mut self) {
        self.gate.on_warm_adopted();
    }

    #[cfg(test)]
    pub(crate) fn gate_context_lost_for_tests(&mut self) {
        self.gate.on_context_lost();
    }
}

fn horizon_params_match(table: &HorizonTable, params: HorizonParams) -> bool {
    table.bin_count == params.bin_count
        && table.ring_count == params.ring_count
        && table.ring_step == params.ring_step
        && table.r_near == params.r_near
        && table.bias_obj_z == params.bias_obj_z
        && table.bias_z == params.bias_z
}

fn eye_distance_sq(a: D3dxVector3, b: D3dxVector3) -> f32 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    let dz = a.z - b.z;
    dx * dx + dy * dy + dz * dz
}

fn horizon_cache_matches(table: &HorizonTable, eye: D3dxVector3, params: HorizonParams, eye_threshold: f32) -> bool {
    horizon_params_match(table, params) && eye_distance_sq(table.eye, eye) < eye_threshold * eye_threshold
}
