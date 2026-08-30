use super::*;
use crate::abi::{
    BoundingBox, BoundingSphere, CellName, D3dxMatrix, D3dxVector2, D3dxVector3, RenderMesh, SIZE_OF_TERRAIN_VERT,
    TerrainFileHeader, TerrainFileLayout, TerrainMeshHeader, TerrainMeshLayout, TerrainVertex,
};
use crate::state::horizon::GATE_EVAL_WINDOW;
use crate::state::quadtree::QuadTreeMesh;
use crate::state::test_support::{test_frustum, test_frustum_with_extent};

fn add_visible_mesh(tree: &mut QuadTree, tex: u32, v_buffer: u32, x: f32) -> MeshId {
    tree.insert_mesh(QuadTreeMesh::new(
        RenderMesh {
            enabled: 1,
            has_alpha: 0,
            animate_uv: 0,
            _padding0: 0,
            tex,
            transform: D3dxMatrix::identity(),
            verts: 3,
            v_buffer,
            faces: 1,
            i_buffer: v_buffer + 100,
        },
        BoundingSphere {
            center: D3dxVector3 { x, y: 0.0, z: 0.0 },
            radius: 5.0,
        },
        BoundingBox::default(),
        None,
    ))
}

fn build_state() -> DistantLandState {
    let mut state = DistantLandState::new(Configuration::default());
    state.world_space_indices.insert(CellName::from_bytes(b"test"), 0);
    let mut world_space = WorldSpace::default();
    world_space.near_statics.set_box(400.0, D3dxVector2::default());
    add_visible_mesh(&mut world_space.near_statics, 20, 2, 10.0);
    add_visible_mesh(&mut world_space.near_statics, 10, 1, -10.0);
    state.world_spaces.push(world_space);
    state.current_world_space = Some(0);
    state
}

fn sentinel_mesh(tex: u32, v_buffer: u32) -> RenderMesh {
    RenderMesh {
        enabled: 1,
        has_alpha: 0,
        animate_uv: 0,
        _padding0: 0,
        tex,
        transform: D3dxMatrix::identity(),
        verts: 3,
        v_buffer,
        faces: 1,
        i_buffer: v_buffer + 100,
    }
}

fn horizon_test_configuration() -> Configuration {
    Configuration {
        horizon_culling: true,
        horizon_bias_z: 0.0,
        horizon_object_bias_z: 0.0,
        horizon_near_units: 512.0,
        horizon_ring_step: 1024.0,
        horizon_max_range: 8192.0,
        horizon_bins: 64,
        horizon_sample_spacing: 512.0,
        horizon_adaptive_gate: false,
        // Pinned small so movement-based horizon tests stay independent of the shipped rebuild
        // default: a sub-cell eye move must reliably miss the cache and trigger a rebuild.
        horizon_rebuild_eye_threshold: 1.0,
        ..Configuration::default()
    }
}

fn terrain_height_field(vertices: &[(f32, f32, f32)]) -> TerrainHeightField {
    let mut bytes = Vec::new();
    for &(x, y, z) in vertices {
        let vertex = TerrainVertex {
            position: D3dxVector3 { x, y, z },
            normal: [128, 128, 255, 0],
            color: 0,
        };
        bytes.extend_from_slice(bytemuck::bytes_of(&vertex));
    }
    let vertex_data_size = bytes.len();
    let layout = TerrainFileLayout {
        header: TerrainFileHeader {
            world_origin: [-8192.0, -8192.0],
            world_size: [16384.0, 16384.0],
            vertex_stride: SIZE_OF_TERRAIN_VERT,
            mesh_count: 1,
            ..TerrainFileHeader::default()
        },
        meshes: vec![TerrainMeshLayout {
            header: TerrainMeshHeader {
                vertex_count: vertices.len() as u32,
                triangle_count: 0,
                ..TerrainMeshHeader::default()
            },
            vertex_data_offset: 0,
            vertex_data_size,
            index_data_offset: vertex_data_size,
            index_data_size: 0,
        }],
    };
    TerrainHeightField::build_from_layout(&layout, &bytes, 512.0).unwrap()
}

fn static_bucket_state(configuration: Configuration) -> DistantLandState {
    let mut state = DistantLandState::new(configuration);
    state.world_space_indices.insert(CellName::EXTERIOR, 0);
    let mut world_space = WorldSpace::default();
    world_space.near_statics.set_box(20000.0, D3dxVector2::default());
    world_space.far_statics.set_box(20000.0, D3dxVector2::default());
    world_space.very_far_statics.set_box(20000.0, D3dxVector2::default());
    add_visible_mesh(&mut world_space.near_statics, 10, 1, 8192.0);
    add_visible_mesh(&mut world_space.far_statics, 20, 2, 8192.0);
    add_visible_mesh(&mut world_space.very_far_statics, 30, 3, 8192.0);
    world_space.near_statics.calc_volume();
    world_space.far_statics.calc_volume();
    world_space.very_far_statics.calc_volume();
    state.world_spaces.push(world_space);
    state.current_world_space = Some(0);
    state
}

fn horizon_view_sphere() -> D3dxVector4 {
    D3dxVector4 {
        x: 0.0,
        y: 0.0,
        z: 0.0,
        w: 20000.0,
    }
}

fn view_sphere_at(x: f32, y: f32, z: f32) -> D3dxVector4 {
    D3dxVector4 { x, y, z, w: 20000.0 }
}

fn eye3(x: f32, y: f32, z: f32) -> D3dxVector3 {
    D3dxVector3 { x, y, z }
}

// D3dxVector3 is an ABI struct without PartialEq, so compare eyes component-wise.
fn assert_eye(eye: D3dxVector3, x: f32, y: f32, z: f32) {
    assert_eq!((eye.x, eye.y, eye.z), (x, y, z));
}

/// State with a height field and the real async build path enabled (worker spawns on first post).
fn async_horizon_state() -> DistantLandState {
    async_horizon_state_with(false)
}

/// Like [`async_horizon_state`], parametrized on the hierarchical-march flag so the async/sync
/// pipeline tests can be re-run under the hierarchical builder (`docs/architecture/horizon-culling.md`
/// §5.2 and §6).
fn async_horizon_state_with(hierarchical: bool) -> DistantLandState {
    let mut configuration = horizon_test_configuration();
    configuration.horizon_hierarchical_march = hierarchical;
    let mut state = DistantLandState::new(configuration);
    state
        .horizon_mut_for_tests()
        .install_height_field_for_tests(Some(Arc::new(terrain_height_field(&[(4096.0, 0.0, 2000.0)]))));
    state.enable_async_horizon();
    state
}

fn adaptive_horizon_state() -> DistantLandState {
    let mut configuration = horizon_test_configuration();
    configuration.horizon_adaptive_gate = true;
    let mut state = DistantLandState::new(configuration);
    state
        .horizon_mut_for_tests()
        .install_height_field_for_tests(Some(Arc::new(terrain_height_field(&[(4096.0, 0.0, 2000.0)]))));
    state.enable_async_horizon();
    state
}

/// Like [`async_horizon_state`] but with a worker-less builder for deterministic result control.
fn stalled_async_horizon_state() -> DistantLandState {
    let mut state = DistantLandState::new(horizon_test_configuration());
    state
        .horizon_mut_for_tests()
        .install_height_field_for_tests(Some(Arc::new(terrain_height_field(&[(4096.0, 0.0, 2000.0)]))));
    state.install_stalled_horizon_builder();
    state
}

/// Body of `async_eye_move_produces_same_table_as_sync`, parametrized so it can be re-run with
/// the hierarchical builder forced on. The async pipeline (staleness cap, worker pickup,
/// cache population) must behave identically regardless of which builder produced the table.
fn assert_async_eye_move_produces_same_table_as_sync(hierarchical: bool) {
    let mut state = async_horizon_state_with(hierarchical);
    let params = HorizonParams::from_configuration(state.configuration);

    // Cold frame: synchronous build.
    state.prepare_horizon(view_sphere_at(0.0, 0.0, 0.0));
    assert_eq!(state.horizon_for_tests().build_count_for_tests(), 1);

    // Eye move within the staleness cap: dispatched to the worker, no critical-path build.
    let moved = view_sphere_at(10.0, 0.0, 0.0);
    state.prepare_horizon(moved);
    assert_eq!(
        state.horizon_for_tests().build_count_for_tests(),
        1,
        "eye move must not build on the critical path"
    );

    state.flush_horizon_worker();
    state.prepare_horizon(moved);
    assert_eq!(
        state.horizon_for_tests().build_count_for_tests(),
        1,
        "pickup must not build synchronously"
    );

    let cached = state
        .horizon_for_tests()
        .cached_horizon_for_tests()
        .as_deref()
        .expect("table present after pickup");
    let direct = HorizonTable::build(&terrain_height_field(&[(4096.0, 0.0, 2000.0)]), eye3(10.0, 0.0, 0.0), params);
    assert_eye(cached.eye, direct.eye.x, direct.eye.y, direct.eye.z);
    assert_eq!(cached.max_slope, direct.max_slope);
}

#[test]
fn async_eye_move_produces_same_table_as_sync() {
    assert_async_eye_move_produces_same_table_as_sync(false);
}

#[test]
fn async_eye_move_produces_same_table_as_sync_hierarchical() {
    assert_async_eye_move_produces_same_table_as_sync(true);
}

#[test]
fn stale_table_used_during_in_flight_rebuild() {
    let mut state = async_horizon_state();
    state.prepare_horizon(view_sphere_at(0.0, 0.0, 0.0));
    let original = state.horizon_for_tests().cached_horizon_for_tests().clone().expect("table");
    assert_eye(original.eye, 0.0, 0.0, 0.0);

    let moved = view_sphere_at(10.0, 0.0, 0.0);
    state.prepare_horizon(moved); // posts async, keeps the original table this frame

    // Pickup only happens inside prepare_horizon, so the cache is still the original until then.
    assert!(Arc::ptr_eq(
        state.horizon_for_tests().cached_horizon_for_tests().as_ref().unwrap(),
        &original
    ));

    state.flush_horizon_worker();
    state.prepare_horizon(moved); // pickup swaps in the fresh table
    assert_eye(
        state.horizon_for_tests().cached_horizon_for_tests().as_deref().unwrap().eye,
        10.0,
        0.0,
        0.0,
    );
}

#[test]
fn generation_bump_discards_stale_async_result() {
    let mut state = stalled_async_horizon_state();
    state.prepare_horizon(view_sphere_at(0.0, 0.0, 0.0)); // sync O, build 1
    state.prepare_horizon(view_sphere_at(10.0, 0.0, 0.0)); // async A queued
    state.run_worker_once(); // A result ready (old generation)

    // Structural invalidation before pickup bumps the generation and clears the cache.
    state.replace_height_field(Some(Arc::new(terrain_height_field(&[(4096.0, 0.0, 2000.0)]))));
    assert!(state.horizon_for_tests().cached_horizon_for_tests().is_none());

    // Cold pickup frame: the stale-generation result is discarded, a fresh sync build runs.
    state.prepare_horizon(view_sphere_at(0.0, 0.0, 0.0));
    assert_eq!(state.horizon_for_tests().build_count_for_tests(), 2);
    assert_eye(
        state.horizon_for_tests().cached_horizon_for_tests().as_deref().unwrap().eye,
        0.0,
        0.0,
        0.0,
    );
}

#[test]
fn eye_returning_to_valid_cache_ignores_late_async_result() {
    let mut state = stalled_async_horizon_state();
    state.prepare_horizon(view_sphere_at(0.0, 0.0, 0.0)); // sync O
    let o_table = state.horizon_for_tests().cached_horizon_for_tests().clone().unwrap();

    state.prepare_horizon(view_sphere_at(10.0, 0.0, 0.0)); // async A, cache still O
    state.run_worker_once(); // A result ready

    // Eye returns to within threshold of O before the pickup.
    state.prepare_horizon(view_sphere_at(0.0, 0.0, 0.0));

    assert!(
        Arc::ptr_eq(
            state.horizon_for_tests().cached_horizon_for_tests().as_ref().unwrap(),
            &o_table
        ),
        "a still-valid cache must not be replaced by a non-matching late result"
    );
    assert_eq!(state.horizon_for_tests().build_count_for_tests(), 1, "no needless rebuild");
}

#[test]
fn synchronous_build_supersedes_pending_async_request() {
    let mut state = stalled_async_horizon_state();
    state.prepare_horizon(view_sphere_at(0.0, 0.0, 0.0)); // sync O, build 1
    state.prepare_horizon(view_sphere_at(20.0, 0.0, 0.0)); // async A queued (id N)
    state.run_worker_once(); // A result ready, stamped with id N

    // Teleport beyond the stale cap forces a synchronous build B, bumping latest_request_id.
    let b = view_sphere_at(1000.0, 0.0, 0.0);
    state.prepare_horizon(b); // stale_beyond_cap -> sync B (build 2), supersedes A
    assert_eq!(state.horizon_for_tests().build_count_for_tests(), 2);
    assert_eye(
        state.horizon_for_tests().cached_horizon_for_tests().as_deref().unwrap().eye,
        1000.0,
        0.0,
        0.0,
    );

    // A's now-superseded result must not replace B, and must not trigger another build.
    state.prepare_horizon(b);
    assert_eq!(
        state.horizon_for_tests().build_count_for_tests(),
        2,
        "stale result must not trigger a build"
    );
    assert_eye(
        state.horizon_for_tests().cached_horizon_for_tests().as_deref().unwrap().eye,
        1000.0,
        0.0,
        0.0,
    );
}

#[test]
fn reentry_after_eye_leaves_field_builds_synchronously() {
    let mut state = stalled_async_horizon_state();
    state.prepare_horizon(view_sphere_at(0.0, 0.0, 0.0)); // sync O, build 1
    state.prepare_horizon(view_sphere_at(10.0, 0.0, 0.0)); // async A for eye (10,0,0)
    state.run_worker_once(); // A result ready

    // Eye leaves the field: clears the cache and supersedes A (its result stays in the slot).
    state.prepare_horizon(view_sphere_at(100_000.0, 0.0, 0.0));
    assert!(state.horizon_for_tests().cached_horizon_for_tests().is_none());

    // Re-enter at an eye A does NOT match: must build synchronously, not adopt A.
    state.prepare_horizon(view_sphere_at(30.0, 0.0, 0.0));
    assert_eq!(state.horizon_for_tests().build_count_for_tests(), 2);
    assert_eye(
        state.horizon_for_tests().cached_horizon_for_tests().as_deref().unwrap().eye,
        30.0,
        0.0,
        0.0,
    );
}

#[test]
fn superseded_id_result_matching_current_eye_is_adopted() {
    // Under continuous movement every miss re-posts a fresh id, so the worker's slightly-late result
    // is almost always id-stale yet built for essentially the current eye. Adopting it keeps the
    // rebuild off the critical path (breaks the re-post treadmill) instead of forcing a sync build.
    let mut state = stalled_async_horizon_state();
    state.prepare_horizon(view_sphere_at(0.0, 0.0, 0.0)); // sync O, build 1
    state.prepare_horizon(view_sphere_at(10.0, 0.0, 0.0)); // async A (id = latest), pending set
    let o_table = state.horizon_for_tests().cached_horizon_for_tests().clone().unwrap();

    // A result for the current eye but carrying a superseded id (an older request finished late).
    state.inject_async_result(
        eye3(10.0, 0.0, 0.0),
        state.horizon_for_tests().generation_for_tests(),
        state.horizon_for_tests().request_id_for_tests() - 1,
    );

    state.prepare_horizon(view_sphere_at(10.0, 0.0, 0.0));
    assert!(
        !Arc::ptr_eq(
            state.horizon_for_tests().cached_horizon_for_tests().as_ref().unwrap(),
            &o_table
        ),
        "a superseded-id result matching the current eye must replace the stale cache"
    );
    assert_eye(
        state.horizon_for_tests().cached_horizon_for_tests().as_deref().unwrap().eye,
        10.0,
        0.0,
        0.0,
    );
    assert_eq!(
        state.horizon_for_tests().build_count_for_tests(),
        1,
        "adopting the async result must not build synchronously"
    );
    assert!(
        !state.horizon_for_tests().has_pending_for_tests(),
        "adopting a usable result stops aging the request toward a starvation sync"
    );
}

#[test]
fn superseded_id_result_not_matching_current_eye_is_discarded() {
    // Treadmill-breaking adoption is gated on a real eye+params match: a superseded result for a
    // different eye must never sneak in via the closer-than-cache heuristic (reserved for the
    // current request), so the cache is left untouched and no synchronous build is forced.
    let mut state = stalled_async_horizon_state();
    state.prepare_horizon(view_sphere_at(0.0, 0.0, 0.0)); // sync O, build 1
    state.prepare_horizon(view_sphere_at(10.0, 0.0, 0.0)); // async A (id = latest), pending set
    let o_table = state.horizon_for_tests().cached_horizon_for_tests().clone().unwrap();

    // A superseded result for a far-away eye (a much older request finished late).
    state.inject_async_result(
        eye3(500.0, 0.0, 0.0),
        state.horizon_for_tests().generation_for_tests(),
        state.horizon_for_tests().request_id_for_tests() - 1,
    );

    state.prepare_horizon(view_sphere_at(10.0, 0.0, 0.0));
    assert!(
        Arc::ptr_eq(
            state.horizon_for_tests().cached_horizon_for_tests().as_ref().unwrap(),
            &o_table
        ),
        "a superseded-id result for a different eye must not replace the cache"
    );
    assert_eq!(
        state.horizon_for_tests().build_count_for_tests(),
        1,
        "a discarded stale-id pickup must not build synchronously"
    );
    assert!(
        state.horizon_for_tests().has_pending_for_tests(),
        "the current request stays pending"
    );
}

#[test]
fn eye_jump_beyond_stale_cap_builds_synchronously() {
    let mut state = stalled_async_horizon_state(); // worker never delivers
    state.prepare_horizon(view_sphere_at(0.0, 0.0, 0.0)); // sync O, build 1

    // A single-frame jump past MAX_STALE_DISTANCE must build synchronously despite async being on.
    state.prepare_horizon(view_sphere_at(MAX_STALE_DISTANCE + 40.0, 0.0, 0.0));
    assert_eq!(state.horizon_for_tests().build_count_for_tests(), 2);
    assert_eye(
        state.horizon_for_tests().cached_horizon_for_tests().as_deref().unwrap().eye,
        MAX_STALE_DISTANCE + 40.0,
        0.0,
        0.0,
    );
    assert!(
        !state.horizon_for_tests().has_pending_for_tests(),
        "the synchronous build clears any pending request"
    );
}

#[test]
fn starved_worker_forces_synchronous_build_after_max_pending_frames() {
    let mut state = stalled_async_horizon_state(); // worker never delivers a result
    state.prepare_horizon(view_sphere_at(0.0, 0.0, 0.0)); // sync O, build 1

    // Small in-cap moves each frame keep dispatching async; the worker never delivers.
    let mut builds = Vec::new();
    for frame in 1..=(MAX_PENDING_FRAMES + 1) {
        state.prepare_horizon(view_sphere_at(2.0 + frame as f32, 0.0, 0.0));
        builds.push(state.horizon_for_tests().build_count_for_tests());
    }

    // The frame that reaches the pending-age cap forces exactly one synchronous rebuild.
    assert!(
        builds[..MAX_PENDING_FRAMES as usize].iter().all(|&count| count == 1),
        "in-cap async frames must not build synchronously: {builds:?}"
    );
    assert_eq!(*builds.last().unwrap(), 2, "starved worker forces a synchronous rebuild");
    assert!(
        !state.horizon_for_tests().has_pending_for_tests(),
        "the synchronous build clears the pending request"
    );
}

#[test]
fn param_change_rebuilds_synchronously_even_with_async_enabled() {
    let mut state = async_horizon_state();
    state.prepare_horizon(view_sphere_at(0.0, 0.0, 0.0)); // sync O, build 1

    // Same eye but a param change must rebuild on the critical path, never deferring to a worker.
    state.configuration.horizon_bias_z += 256.0;
    state.prepare_horizon(view_sphere_at(0.0, 0.0, 0.0));
    assert_eq!(
        state.horizon_for_tests().build_count_for_tests(),
        2,
        "param change must not defer to the worker"
    );
}

#[test]
fn structural_changes_bump_both_epochs_and_clear_cache() {
    let mut state = async_horizon_state();
    state.prepare_horizon(view_sphere_at(0.0, 0.0, 0.0));
    assert!(state.horizon_for_tests().cached_horizon_for_tests().is_some());
    let (gen0, id0) = (
        state.horizon_for_tests().generation_for_tests(),
        state.horizon_for_tests().request_id_for_tests(),
    );

    // replace_height_field invalidates: cache cleared, both epochs bumped.
    state.replace_height_field(Some(Arc::new(terrain_height_field(&[(4096.0, 0.0, 2000.0)]))));
    assert!(state.horizon_for_tests().cached_horizon_for_tests().is_none());
    assert!(
        state.horizon_for_tests().generation_for_tests() > gen0 && state.horizon_for_tests().request_id_for_tests() > id0
    );

    // A failed disk rebuild (no terrain.bin in the test env) must still clear + bump.
    state.prepare_horizon(view_sphere_at(0.0, 0.0, 0.0));
    assert!(state.horizon_for_tests().cached_horizon_for_tests().is_some());
    let (gen1, id1) = (
        state.horizon_for_tests().generation_for_tests(),
        state.horizon_for_tests().request_id_for_tests(),
    );
    assert!(state.build_height_field().is_err());
    assert!(state.horizon_for_tests().cached_horizon_for_tests().is_none());
    assert!(state.horizon_for_tests().height_field_for_tests().is_none());
    assert!(
        state.horizon_for_tests().generation_for_tests() > gen1 && state.horizon_for_tests().request_id_for_tests() > id1
    );
}

#[test]
fn absent_or_invalid_occlusion_asset_degrades_to_cleared_field() {
    let mut configuration = Configuration::default();
    configuration.horizon_culling = true;
    let mut state = DistantLandState::new(configuration);
    // Pre-install so the assertion proves clearing rather than mere absence.
    state.replace_height_field(Some(Arc::new(terrain_height_field(&[(4096.0, 0.0, 2000.0)]))));

    // No Data Files\distantland output exists in the test environment, so the asset load fails
    // (absent or header-mismatched). That must degrade to an inactive field with Ok, never fail
    // the distant-land load.
    let layout = TerrainFileLayout {
        header: TerrainFileHeader {
            world_origin: [-8192.0, -8192.0],
            world_size: [16384.0, 16384.0],
            vertex_stride: SIZE_OF_TERRAIN_VERT,
            mesh_count: 1,
            ..TerrainFileHeader::default()
        },
        meshes: Vec::new(),
    };
    assert!(state.build_height_field_from(&layout).is_ok());
    assert!(state.horizon_for_tests().height_field_for_tests().is_none());
}

#[test]
fn apply_horizon_config_paths_bump_epochs() {
    // Param-only change keeps the field but invalidates the cache and bumps both epochs.
    let mut state = async_horizon_state();
    state.prepare_horizon(view_sphere_at(0.0, 0.0, 0.0));
    let (gen0, id0) = (
        state.horizon_for_tests().generation_for_tests(),
        state.horizon_for_tests().request_id_for_tests(),
    );
    let mut params = horizon_params_from(state.configuration);
    params.bias_z += 100.0;
    state.apply_horizon_config(params).unwrap();
    assert!(state.horizon_for_tests().cached_horizon_for_tests().is_none());
    assert!(state.horizon_for_tests().height_field_for_tests().is_some());
    assert!(
        state.horizon_for_tests().generation_for_tests() > gen0 && state.horizon_for_tests().request_id_for_tests() > id0
    );

    // Disable path drops the field and bumps both epochs.
    let mut state = async_horizon_state();
    state.prepare_horizon(view_sphere_at(0.0, 0.0, 0.0));
    let (gen0, id0) = (
        state.horizon_for_tests().generation_for_tests(),
        state.horizon_for_tests().request_id_for_tests(),
    );
    let mut params = horizon_params_from(state.configuration);
    params.enabled = 0;
    state.apply_horizon_config(params).unwrap();
    assert!(state.horizon_for_tests().cached_horizon_for_tests().is_none());
    assert!(state.horizon_for_tests().height_field_for_tests().is_none());
    assert!(
        state.horizon_for_tests().generation_for_tests() > gen0 && state.horizon_for_tests().request_id_for_tests() > id0
    );
}

#[test]
fn apply_horizon_config_clears_frame_accumulator() {
    let mut state = async_horizon_state();
    state.accumulate_horizon_frame_stats(
        horizon_view_sphere(),
        HorizonCullStats {
            meshes_culled: 7,
            nodes_pruned: 11,
            ..HorizonCullStats::default()
        },
    );

    let params = horizon_params_from(state.configuration);
    state.apply_horizon_config(params).unwrap();

    assert!(state.horizon_for_tests().frame_eye_for_tests().is_none());
    assert_eq!(state.horizon_for_tests().frame_stats_for_tests().meshes_culled, 0);
    assert_eq!(state.horizon_for_tests().frame_stats_for_tests().nodes_pruned, 0);
}

#[test]
fn worker_lifecycle_never_spawned_drops_cleanly() {
    let state = async_horizon_state();
    assert!(!state.horizon_for_tests().has_builder_for_tests());
    drop(state); // no thread spawned, nothing to join
}

#[test]
fn worker_lifecycle_spawned_then_dropped_joins_cleanly() {
    let mut state = async_horizon_state();
    state.prepare_horizon(view_sphere_at(0.0, 0.0, 0.0));
    state.prepare_horizon(view_sphere_at(10.0, 0.0, 0.0)); // spawns worker + posts
    assert!(state.horizon_for_tests().has_builder_for_tests());
    drop(state); // Drop signals shutdown + joins; must not hang
}

#[test]
fn worker_lifecycle_dropped_mid_build_joins_cleanly() {
    let mut state = async_horizon_state();
    state.prepare_horizon(view_sphere_at(0.0, 0.0, 0.0));
    for x in 1..48 {
        state.prepare_horizon(view_sphere_at(x as f32, 0.0, 0.0));
    }
    drop(state); // possibly mid-build; Drop must still join cleanly
}

fn collect_precise_meshes(state: &DistantLandState, set_flags: u32) -> Vec<RenderMesh> {
    let mut output = SharedVec::create_for_tests::<RenderMesh>(64, 1).unwrap();
    let mut scratch = Vec::new();
    state
        .get_visible_meshes(
            &mut output,
            &mut scratch,
            &test_frustum_with_extent(20000.0),
            horizon_view_sphere(),
            None,
            VisibleSetSort::None,
            set_flags,
        )
        .unwrap();
    output.read_all::<RenderMesh>().unwrap()
}

#[test]
fn unsorted_queries_append_to_existing_output() {
    let state = build_state();
    let mut output = SharedVec::create_for_tests::<RenderMesh>(64, 1).unwrap();
    let mut scratch = Vec::new();
    let frustum = test_frustum();

    state
        .get_visible_meshes_coarse(&mut output, &mut scratch, &frustum, VisibleSetSort::None, VIS_NEAR)
        .unwrap();
    state
        .get_visible_meshes_coarse(&mut output, &mut scratch, &frustum, VisibleSetSort::None, VIS_NEAR)
        .unwrap();

    let textures: Vec<u32> = output
        .read_all::<RenderMesh>()
        .unwrap()
        .into_iter()
        .map(|mesh| mesh.tex)
        .collect();
    assert_eq!(textures, vec![20, 10, 20, 10]);
}

#[test]
fn sorted_queries_append_then_sort_the_full_output() {
    let state = build_state();
    let mut output = SharedVec::create_for_tests::<RenderMesh>(64, 1).unwrap();
    let mut scratch = vec![sentinel_mesh(999, 999)];
    let frustum = test_frustum();

    output.push(sentinel_mesh(15, 9)).unwrap();
    state
        .get_visible_meshes_coarse(&mut output, &mut scratch, &frustum, VisibleSetSort::ByState, VIS_NEAR)
        .unwrap();

    let meshes = output.read_all::<RenderMesh>().unwrap();
    let textures: Vec<u32> = meshes.iter().map(|mesh| mesh.tex).collect();
    assert_eq!(textures, vec![10, 15, 20]);
    assert!(
        meshes
            .windows(2)
            .all(|pair| RenderMesh::compare_by_state(&pair[0], &pair[1]) != std::cmp::Ordering::Greater)
    );
}

fn assert_prepare_horizon_reuses_cache_for_same_eye(hierarchical: bool) {
    let mut configuration = horizon_test_configuration();
    configuration.horizon_hierarchical_march = hierarchical;
    let mut state = DistantLandState::new(configuration);
    state
        .horizon_mut_for_tests()
        .install_height_field_for_tests(Some(Arc::new(terrain_height_field(&[(4096.0, 0.0, 2000.0)]))));

    state.prepare_horizon(horizon_view_sphere());
    state.prepare_horizon(horizon_view_sphere());

    assert!(state.horizon_for_tests().cached_horizon_for_tests().is_some());
    assert_eq!(state.horizon_for_tests().build_count_for_tests(), 1);
}

#[test]
fn prepare_horizon_reuses_cache_for_same_eye() {
    assert_prepare_horizon_reuses_cache_for_same_eye(false);
}

#[test]
fn prepare_horizon_reuses_cache_for_same_eye_hierarchical() {
    assert_prepare_horizon_reuses_cache_for_same_eye(true);
}

#[test]
fn adaptive_warming_posts_single_async_and_never_syncs() {
    let mut state = adaptive_horizon_state();

    state.prepare_horizon(horizon_view_sphere());
    let request_id = state.horizon_for_tests().request_id_for_tests();
    assert!(state.horizon_for_tests().has_pending_for_tests());
    assert!(state.horizon_for_tests().cached_horizon_for_tests().is_none());
    assert_eq!(state.horizon_for_tests().build_count_for_tests(), 0);

    state.prepare_horizon(horizon_view_sphere());
    assert_eq!(
        state.horizon_for_tests().request_id_for_tests(),
        request_id,
        "warming must not repost each frame"
    );
    assert_eq!(
        state.horizon_for_tests().build_count_for_tests(),
        0,
        "warming must not build synchronously"
    );
}

#[test]
fn adaptive_warming_adopts_cold_result_within_stale_cap() {
    let mut state = adaptive_horizon_state();

    state.prepare_horizon(view_sphere_at(0.0, 0.0, 0.0));
    state.flush_horizon_worker();
    state.prepare_horizon(view_sphere_at(MAX_STALE_DISTANCE - 1.0, 0.0, 0.0));

    assert!(state.horizon_for_tests().cached_horizon_for_tests().is_some());
    assert_eq!(state.horizon_gate_state_code(), 1);
    assert_eq!(state.horizon_for_tests().build_count_for_tests(), 0);
}

#[test]
fn adaptive_warming_rejects_cold_result_beyond_stale_cap() {
    let mut state = adaptive_horizon_state();

    state.prepare_horizon(view_sphere_at(0.0, 0.0, 0.0));
    state.flush_horizon_worker();
    state.prepare_horizon(view_sphere_at(MAX_STALE_DISTANCE + 1.0, 0.0, 0.0));

    assert!(state.horizon_for_tests().cached_horizon_for_tests().is_none());
    assert_eq!(state.horizon_gate_state_code(), 2);
    assert_eq!(state.horizon_for_tests().build_count_for_tests(), 0);
}

#[test]
fn adaptive_warm_timeout_fails_probe() {
    let mut state = adaptive_horizon_state();

    state.prepare_horizon(horizon_view_sphere());
    for _ in 0..32 {
        state.accumulate_horizon_frame_stats(horizon_view_sphere(), HorizonCullStats::default());
        state.finish_horizon_frame();
    }

    assert_eq!(state.horizon_gate_state_code(), 3);
    assert!(state.horizon_for_tests().cached_horizon_for_tests().is_none());
}

#[test]
fn suspended_gate_posts_no_build_and_yields_no_horizon() {
    // A suspended gate must not build or provide a horizon, even after eye movement.
    let mut state = adaptive_horizon_state();
    state.prepare_horizon(horizon_view_sphere());
    for _ in 0..32 {
        state.accumulate_horizon_frame_stats(horizon_view_sphere(), HorizonCullStats::default());
        state.finish_horizon_frame();
    }
    assert_eq!(state.horizon_gate_state_code(), 3, "precondition: gate is suspended");

    let builds_before = state.horizon_for_tests().build_count_for_tests();
    let had_builder = state.horizon_for_tests().has_builder_for_tests();

    state.prepare_horizon(horizon_view_sphere());
    state.prepare_horizon(view_sphere_at(500.0, 0.0, 0.0));

    assert_eq!(
        state.horizon_for_tests().build_count_for_tests(),
        builds_before,
        "a suspended gate must never build"
    );
    assert!(
        state.horizon_for_tests().cached_horizon_for_tests().is_none(),
        "a suspended gate must yield no horizon to traversal"
    );
    assert!(
        !state.horizon_for_tests().has_pending_for_tests(),
        "a suspended gate must never post an async build"
    );
    assert_eq!(
        state.horizon_for_tests().has_builder_for_tests(),
        had_builder,
        "a suspended gate must not lazily spawn a builder"
    );
}

#[test]
fn reflection_only_frames_still_tick_gate_without_main_static_pass() {
    // Reflection-only queries must still tick the gate.
    let mut state = adaptive_horizon_state();
    state.horizon_mut_for_tests().gate_context_available_for_tests();
    state.horizon_mut_for_tests().gate_warm_adopted_for_tests();
    assert_eq!(state.horizon_gate_state_code(), 1);

    for _ in 0..(GATE_EVAL_WINDOW * 2) {
        // Only the reflection-style call runs this "frame" - no separate main-pass accumulation.
        state.accumulate_horizon_frame_stats(horizon_view_sphere(), HorizonCullStats::default());
        state.finish_horizon_frame();
    }

    assert_eq!(
        state.horizon_gate_state_code(),
        3,
        "reflection-only frames must still tick the gate to suspension"
    );
}

#[test]
fn main_and_reflection_accumulation_in_one_frame_count_as_a_single_gate_tick() {
    // Main and reflection queries in one frame must produce one gate tick.
    let mut state = adaptive_horizon_state();
    state.horizon_mut_for_tests().gate_context_available_for_tests();
    state.horizon_mut_for_tests().gate_warm_adopted_for_tests();

    for i in 0..GATE_EVAL_WINDOW {
        // Main pass: up to 3 unsorted precise queries (VIS_NEAR/FAR/VERY_FAR) into one accumulator.
        state.accumulate_horizon_frame_stats(horizon_view_sphere(), HorizonCullStats::default());
        state.accumulate_horizon_frame_stats(horizon_view_sphere(), HorizonCullStats::default());
        // Reflection: one more inline-sorted query landing in the same frame's accumulator.
        state.accumulate_horizon_frame_stats(horizon_view_sphere(), HorizonCullStats::default());
        state.finish_horizon_frame();

        if i + 1 < GATE_EVAL_WINDOW {
            assert_eq!(
                state.horizon_gate_state_code(),
                1,
                "the eval window must not resolve before {GATE_EVAL_WINDOW} frames"
            );
        }
    }

    assert_eq!(
        state.horizon_gate_state_code(),
        3,
        "exactly one window's worth of frames (not half) must resolve the probe"
    );
}

#[test]
fn context_lost_stats_do_not_leak_into_next_evaluation_window() {
    // Stats from a lost context must not leak into the next evaluation window.
    let mut state = adaptive_horizon_state();
    state.horizon_mut_for_tests().gate_context_available_for_tests();
    state.horizon_mut_for_tests().gate_warm_adopted_for_tests();
    assert_eq!(state.horizon_gate_state_code(), 1);

    // A huge-benefit frame arrives, but context is lost before the frame closes.
    state.accumulate_horizon_frame_stats(
        horizon_view_sphere(),
        HorizonCullStats {
            meshes_culled: 999,
            nodes_pruned: 999,
            ..HorizonCullStats::default()
        },
    );
    state.horizon_mut_for_tests().gate_context_lost_for_tests();
    state.finish_horizon_frame();
    assert!(
        state.horizon_for_tests().frame_eye_for_tests().is_none(),
        "the per-frame accumulator must always reset on finish"
    );

    // Context returns; the gate re-warms and its evaluation window must start clean rather than
    // pre-loaded with the disabled frame's huge counts.
    state.horizon_mut_for_tests().gate_context_available_for_tests();
    state.horizon_mut_for_tests().gate_warm_adopted_for_tests();
    for _ in 0..GATE_EVAL_WINDOW {
        state.accumulate_horizon_frame_stats(horizon_view_sphere(), HorizonCullStats::default());
        state.finish_horizon_frame();
    }

    assert_eq!(
        state.horizon_gate_state_code(),
        3,
        "a clean zero-benefit window must suspend, proving the earlier 999/999 frame was discarded"
    );
}

fn horizon_params_from(configuration: Configuration) -> SetHorizonConfigParameters {
    SetHorizonConfigParameters {
        enabled: configuration.horizon_culling as u32,
        bias_z: configuration.horizon_bias_z,
        object_bias_z: configuration.horizon_object_bias_z,
        near_units: configuration.horizon_near_units,
        ring_step: configuration.horizon_ring_step,
        max_range: configuration.horizon_max_range,
        bins: configuration.horizon_bins,
        sample_spacing: configuration.horizon_sample_spacing,
        adaptive_gate: configuration.horizon_adaptive_gate as u32,
    }
}

#[test]
fn bias_only_change_rebuilds_cached_horizon() {
    // Regression guard for the cache-match fix: a bias-only change with an unchanged eye must
    // invalidate the cached table (it would otherwise be reused, leaving live tuning dead).
    let mut state = DistantLandState::new(horizon_test_configuration());
    state
        .horizon_mut_for_tests()
        .install_height_field_for_tests(Some(Arc::new(terrain_height_field(&[(4096.0, 0.0, 2000.0)]))));

    state.prepare_horizon(horizon_view_sphere());
    assert_eq!(state.horizon_for_tests().build_count_for_tests(), 1);

    state.configuration.horizon_bias_z += 256.0;
    state.prepare_horizon(horizon_view_sphere());

    assert_eq!(state.horizon_for_tests().build_count_for_tests(), 2);
}

#[test]
fn apply_horizon_config_bias_change_clears_cache_without_rebuilding_field() {
    // Existing field ⇒ no disk rebuild regardless of param changes. (There is no terrain.bin in the
    // test environment, so any attempted rebuild would make apply_horizon_config return Err.)
    let mut state = DistantLandState::new(horizon_test_configuration());
    state
        .horizon_mut_for_tests()
        .install_height_field_for_tests(Some(Arc::new(terrain_height_field(&[(4096.0, 0.0, 2000.0)]))));
    state.prepare_horizon(horizon_view_sphere());
    assert!(state.horizon_for_tests().cached_horizon_for_tests().is_some());

    let mut params = horizon_params_from(state.configuration);
    params.bias_z += 100.0;
    state.apply_horizon_config(params).unwrap();

    assert!(state.horizon_for_tests().cached_horizon_for_tests().is_none());
    assert!(state.horizon_for_tests().height_field_for_tests().is_some());
    assert_eq!(state.configuration.horizon_bias_z, params.bias_z);
}

#[test]
fn adaptive_config_change_moves_gate_to_warming() {
    let mut state = adaptive_horizon_state();
    state.prepare_horizon(horizon_view_sphere());

    let mut params = horizon_params_from(state.configuration);
    params.bias_z += 100.0;
    state.apply_horizon_config(params).unwrap();

    assert_eq!(state.horizon_gate_state_code(), 2);
    assert!(state.horizon_for_tests().cached_horizon_for_tests().is_none());
}

#[test]
fn apply_horizon_config_clamps_and_disables_without_touching_disk() {
    let mut state = DistantLandState::new(horizon_test_configuration());
    state
        .horizon_mut_for_tests()
        .install_height_field_for_tests(Some(Arc::new(terrain_height_field(&[(4096.0, 0.0, 2000.0)]))));

    let params = SetHorizonConfigParameters {
        enabled: 0,
        bias_z: -100.0,
        object_bias_z: 1.0e9,
        near_units: -1.0,
        ring_step: 0.0,
        max_range: 1.0e12,
        bins: 0,
        sample_spacing: 0.0,
        adaptive_gate: 1,
    };
    state.apply_horizon_config(params).unwrap();

    assert!(!state.configuration.horizon_culling);
    assert!(state.horizon_for_tests().height_field_for_tests().is_none());
    assert!(state.horizon_for_tests().cached_horizon_for_tests().is_none());
    assert_eq!(state.configuration.horizon_bias_z, 0.0);
    assert_eq!(state.configuration.horizon_bins, 64);
}

#[test]
fn height_field_needs_build_only_when_enabled_without_field() {
    let mut state = DistantLandState::new(horizon_test_configuration());

    // Enabled, no field yet ⇒ build.
    assert!(state.horizon_for_tests().height_field_for_tests().is_none());
    assert!(state.height_field_needs_build());

    // Enabled, field present ⇒ no build. A runtime march-step (sample spacing) change must NOT
    // rebuild the field: the occluder grid is fixed at load time by the generated asset.
    state
        .horizon_mut_for_tests()
        .install_height_field_for_tests(Some(Arc::new(terrain_height_field(&[(4096.0, 0.0, 2000.0)]))));
    assert!(!state.height_field_needs_build());
    state.configuration.horizon_sample_spacing += 512.0;
    assert!(!state.height_field_needs_build());

    // Disabled ⇒ never.
    state.configuration.horizon_culling = false;
    assert!(!state.height_field_needs_build());
}

#[test]
fn absent_height_field_keeps_precise_static_output_byte_identical() {
    let baseline = static_bucket_state(Configuration::default());
    let mut enabled_without_field = static_bucket_state(horizon_test_configuration());
    enabled_without_field.prepare_horizon(horizon_view_sphere());

    let baseline_meshes = collect_precise_meshes(&baseline, VIS_NEAR | VIS_FAR | VIS_VERY_FAR);
    let enabled_meshes = collect_precise_meshes(&enabled_without_field, VIS_NEAR | VIS_FAR | VIS_VERY_FAR);

    assert_eq!(
        bytemuck::cast_slice::<RenderMesh, u8>(&baseline_meshes),
        bytemuck::cast_slice::<RenderMesh, u8>(&enabled_meshes)
    );
}

#[test]
fn horizon_culling_applies_to_all_static_ranges() {
    let mut baseline = static_bucket_state(Configuration::default());
    let mut culled = static_bucket_state(horizon_test_configuration());
    culled
        .horizon_mut_for_tests()
        .install_height_field_for_tests(Some(Arc::new(terrain_height_field(&[(4096.0, 0.0, 2000.0)]))));
    baseline.prepare_horizon(horizon_view_sphere());
    culled.prepare_horizon(horizon_view_sphere());

    let baseline_meshes = collect_precise_meshes(&baseline, VIS_NEAR | VIS_FAR | VIS_VERY_FAR);
    let culled_meshes = collect_precise_meshes(&culled, VIS_NEAR | VIS_FAR | VIS_VERY_FAR);

    assert_eq!(baseline_meshes.len(), 3);
    assert!(culled_meshes.is_empty());
}

#[test]
fn horizon_culling_does_not_apply_to_interior_world_spaces() {
    let mut state = static_bucket_state(horizon_test_configuration());
    state
        .horizon_mut_for_tests()
        .install_height_field_for_tests(Some(Arc::new(terrain_height_field(&[(4096.0, 0.0, 2000.0)]))));
    state.prepare_horizon(horizon_view_sphere());
    assert!(state.horizon_for_tests().cached_horizon_for_tests().is_some());

    state.world_space_indices.remove(CellName::EXTERIOR.as_bytes());
    state.world_space_indices.insert(CellName::from_bytes(b"interior"), 0);
    state.current_world_space = Some(0);

    let meshes = collect_precise_meshes(&state, VIS_NEAR | VIS_FAR | VIS_VERY_FAR);
    assert_eq!(meshes.len(), 3);
}

#[test]
fn dynamic_visibility_updates_flip_groups_and_ignore_out_of_range_indices() {
    let mut state = DistantLandState::new(Configuration::default());
    state.world_space_indices.insert(CellName::from_bytes(b"test"), 0);
    let mut world_space = WorldSpace::default();
    world_space.near_statics.set_box(400.0, D3dxVector2::default());
    let first = add_visible_mesh(&mut world_space.near_statics, 20, 2, 10.0);
    let second = add_visible_mesh(&mut world_space.near_statics, 10, 1, -10.0);
    state.world_spaces.push(world_space);
    state.dynamic_vis_groups = vec![
        Vec::new(),
        vec![
            DynamicMeshRef {
                world: 0,
                tree: StaticTreeKind::Near,
                mesh: first,
            },
            DynamicMeshRef {
                world: 0,
                tree: StaticTreeKind::Near,
                mesh: second,
            },
        ],
    ];

    state.update_dyn_vis_one(DynVisFlag {
        group_index: 1,
        enable: 0,
        _padding0: 0,
    });
    let world_space = &state.world_spaces[0];
    assert_eq!(world_space.near_statics.mesh_enabled(first), 0);
    assert_eq!(world_space.near_statics.mesh_enabled(second), 0);

    state.update_dyn_vis_one(DynVisFlag {
        group_index: 99,
        enable: 1,
        _padding0: 0,
    });
    let world_space = &state.world_spaces[0];
    assert_eq!(world_space.near_statics.mesh_enabled(first), 0);
    assert_eq!(world_space.near_statics.mesh_enabled(second), 0);
}

#[test]
fn dynamic_visibility_only_updates_referenced_world_space() {
    let mut state = DistantLandState::new(Configuration::default());
    state.world_space_indices.insert(CellName::from_bytes(b"first"), 0);
    state.world_space_indices.insert(CellName::from_bytes(b"second"), 1);

    let mut first_world = WorldSpace::default();
    first_world.near_statics.set_box(400.0, D3dxVector2::default());
    let first_mesh = add_visible_mesh(&mut first_world.near_statics, 20, 2, 10.0);

    let mut second_world = WorldSpace::default();
    second_world.near_statics.set_box(400.0, D3dxVector2::default());
    let second_mesh = add_visible_mesh(&mut second_world.near_statics, 10, 1, -10.0);

    state.world_spaces.push(first_world);
    state.world_spaces.push(second_world);
    state.dynamic_vis_groups = vec![
        Vec::new(),
        vec![DynamicMeshRef {
            world: 0,
            tree: StaticTreeKind::Near,
            mesh: first_mesh,
        }],
    ];

    state.update_dyn_vis_one(DynVisFlag {
        group_index: 1,
        enable: 0,
        _padding0: 0,
    });

    assert_eq!(state.world_spaces[0].near_statics.mesh_enabled(first_mesh), 0);
    assert_eq!(state.world_spaces[1].near_statics.mesh_enabled(second_mesh), 1);
}

#[test]
fn world_spaces_with_distinct_high_byte_names_select_distinct_indices() {
    // Two interior names from a Russian install's cp1251 bytes. They differ in one byte, but
    // a lossy UTF-8 decode renders both as the same run of replacement characters, so a
    // String-keyed map would have bound the second interior to the first one's statics.
    let balmora = b"\xc1\xe0\xeb\xec\xee\xf0\xe0";
    let balmara = b"\xc1\xe0\xeb\xec\xe0\xf0\xe0";
    assert_eq!(
        String::from_utf8_lossy(balmora),
        String::from_utf8_lossy(balmara),
        "the fixture only means something while these decode alike"
    );

    let mut state = DistantLandState::new(Configuration::default());
    state.world_spaces.push(WorldSpace::default());
    state.world_spaces.push(WorldSpace::default());
    state.world_space_indices.insert(CellName::from_bytes(balmora), 0);
    state.world_space_indices.insert(CellName::from_bytes(balmara), 1);

    assert!(state.set_current_world_space(balmora));
    assert_eq!(state.current_world_space, Some(0));

    assert!(state.set_current_world_space(balmara));
    assert_eq!(state.current_world_space, Some(1));

    assert!(!state.set_current_world_space(b"\xc1\xe0\xeb"));
    assert_eq!(state.current_world_space, None);
}

/// Builds an exterior-only state holding `count` streamable resources, all inside the
/// planner's first cell so one sweep covers them without depending on the offset order.
fn residency_state(count: u32) -> DistantLandState {
    let mut state = DistantLandState::new(Configuration::default());
    state.world_space_indices.insert(CellName::EXTERIOR, 0);
    state.world_spaces.push(WorldSpace::default());
    state.current_world_space = Some(0);
    state.residency_resources = (0..count)
        .map(|i| ResidencyResource {
            geometry_bytes: 1024,
            streamable: true,
            center: D3dxVector3 {
                x: 100.0 + f32::from(i as u16),
                y: 100.0,
                z: 0.0,
            },
            ..ResidencyResource::default()
        })
        .collect();
    state.rebuild_residency_index();
    state
}

fn plan_params(max_resources: u32) -> PlanResidencyParameters {
    PlanResidencyParameters {
        plan: 0,
        plan_epoch: 1,
        center_x: 100.0,
        center_y: 100.0,
        center_z: 0.0,
        admission_radius: 8192.0,
        retain_radius: 16384.0,
        max_cells: 64,
        max_resources,
        reserved: 0,
        cap_bytes: u64::MAX,
        available_bytes: u64::MAX,
        cap_debt_bytes: 0,
    }
}

fn admitted_ids(output: &mut SharedVec) -> Vec<u32> {
    output
        .read_all::<ResidencyPlan>()
        .unwrap()
        .into_iter()
        .filter(|plan| plan.action == ResidencyPlanAction::Admit as u32)
        .map(|plan| plan.resource_id)
        .collect()
}

/// The client accepts far fewer admissions per frame than a sweep offers and silently drops
/// the surplus, so a one-shot sweep strands most of the draw distance until the player
/// crosses a cell. An admitting sweep must rewind and re-offer what was never committed.
#[test]
fn an_admitting_sweep_rewinds_and_re_offers_uncommitted_resources() {
    let mut state = residency_state(4);
    let mut output = SharedVec::create_for_tests::<ResidencyPlan>(64, 1).unwrap();

    // Two resources per call, and nothing is ever committed resident: the client dropped
    // every admission. One sweep of the four takes three calls, so eight covers more than
    // two sweeps.
    let mut seen = Vec::new();
    for _ in 0..8 {
        state.plan_residency(&mut output, plan_params(2)).unwrap();
        seen.extend(admitted_ids(&mut output));
    }

    assert_eq!(&seen[..4], &[0, 1, 2, 3], "one sweep should offer every resource once");
    assert!(
        seen.len() > 4,
        "the cursor parked after its first sweep instead of re-offering: {seen:?}"
    );
}

/// A live save load can resolve to the same exterior cell. Its fresh client epoch must restart
/// the host sweep instead of continuing from the old save's cursor.
#[test]
fn a_new_epoch_restarts_the_sweep_within_the_same_cell() {
    let mut state = residency_state(4);
    let mut output = SharedVec::create_for_tests::<ResidencyPlan>(64, 1).unwrap();
    let mut params = plan_params(1);

    state.plan_residency(&mut output, params).unwrap();
    assert_eq!(admitted_ids(&mut output), vec![0]);

    params.plan_epoch += 1;
    state.plan_residency(&mut output, params).unwrap();
    assert_eq!(admitted_ids(&mut output), vec![0]);
}

#[test]
fn residency_planner_holds_a_new_epoch_in_interiors() {
    let mut state = residency_state(4);
    state.world_spaces.push(WorldSpace::default());
    state.current_world_space = Some(1);
    let mut output = SharedVec::create_for_tests::<ResidencyPlan>(64, 1).unwrap();
    let mut params = plan_params(4);
    params.plan_epoch += 1;

    state.plan_residency(&mut output, params).unwrap();

    assert!(admitted_ids(&mut output).is_empty());
    assert_eq!(state.planner_epoch, None);
    assert_eq!(state.planner_cell, None);
}

/// The counterpart: a sweep that admits nothing must leave the cursor parked, because
/// re-offering resources the client cannot take is the tight retry the design forbids.
#[test]
fn a_sweep_that_admits_nothing_parks_the_cursor() {
    let mut state = residency_state(4);
    let mut output = SharedVec::create_for_tests::<ResidencyPlan>(64, 1).unwrap();
    for resource in &mut state.residency_resources {
        resource.resident = true;
    }

    for _ in 0..8 {
        state.plan_residency(&mut output, plan_params(2)).unwrap();
        assert!(admitted_ids(&mut output).is_empty());
    }
    assert_eq!(
        state.planner_offset_cursor,
        state.residency_offsets.len(),
        "the cursor rewound despite admitting nothing"
    );
}
