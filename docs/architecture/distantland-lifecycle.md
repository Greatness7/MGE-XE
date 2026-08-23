# Distant-land initialization lifecycle

This document describes the landed startup, upload, readiness, and failure
lifecycle for distant land. Rendering details are in
[render-pipeline.md](render-pipeline.md); generated payloads are in
[distantland-data.md](distantland-data.md); host transport is in
[ipc.md](ipc.md).

## State and readiness

`DistantLand` owns a small device-resource state machine:

```text
Uninitialized
    |
    v
DeviceResourcesReady
    |  uploadComplete && worldResolved
    v
RenderReady

Any initialization or upload failure -> FailedDisabled
release()                         -> Uninitialized
```

The two readiness conditions are independent:

- `uploadComplete` means the host is ready, shared vectors exist, and terrain,
  statics, and grass resources have finished loading.
- `worldResolved` means Morrowind has resolved the save or new-game world, so
  dynamic-visibility object pointers are safe to bind.

`finalizeUploadIfReady()` is the only normal deferred transition from
`DeviceResourcesReady` to `RenderReady`. Render code must gate on
`canRenderDistantLand()`; cleanup code must gate on `hasDeviceResources()` so
partially initialized resources are still released.

## Startup path

At `Direct3DCreate8` time, `d3d8.dll` attempts to launch `mgeHost64.exe` when
MGE is enabled, proxy-only mode is disabled, distant land is enabled, and the
executable exists. This early launch is not gated on automatic generation being
configured; the host decides whether generation is needed. The same process
remains the runtime IPC host. If the early launch is skipped or fails, the
upload pump retries it lazily.

At the first `Present` where Morrowind's environment pointers are valid,
`MGEProxyDevice` installs the engine patches and calls `DistantLand::init()`.
This gives the upload pump the ESM/ESP loading frames as well as main-menu
frames.

`init()` performs the device-resource work that must remain on the D3D thread:
shader/effect initialization, fixed-function emulation, post-processing,
depth, shadow, water, BSA access, and resolution of the partial-view mapping
APIs. It also installs the world-resolution callback, leaving host connection and
geometry upload to the pump.

If no player cell exists, the startup path arms the pump. If a player cell is
already active, the call came from an in-world renderer restart and the
complete host connection and geometry upload run synchronously instead.

## Upload pump

`Present` advances the pump with an 8 ms statics budget after scene work is
complete:

```text
HostWait
  -> OutputWait
  -> IpcSetup
  -> Landscape
  -> Statics
  -> Grass
  -> StaticsHostWait
  -> Done
```

The phases have the following contracts:

| Phase | Work |
| --- | --- |
| `HostWait` | Launch the host if necessary and poll its bootstrap event without blocking the game thread. |
| `OutputWait` | Issue and poll `QueryOutputReady`. The host's startup-generation worker publishes `Pending`, `Ready`, or `Failed`. |
| `IpcSetup` | Allocate the five long-lived shared vectors used for visibility and dynamic-visibility updates. |
| `Landscape` | Validate and upload terrain resources, then leave `InitLandscape` in flight while the host builds its land quadtree. |
| `Statics` | Preflight all 128 static shards and advance the resumable `StaticsLoader` in budgeted slices. Before sending static metadata, collect the landscape RPC result. |
| `Grass` | Create grass instance resources while the host handles the asynchronous `InitDistantStatics` request. |
| `StaticsHostWait` | Poll the static/grass quadtree build, then free its temporary shared vectors. |
| `Done` | Set `uploadComplete`, stop the pump, and attempt the readiness transition. |

The host starts its IPC server before the startup-generation worker finishes.
Bootstrap therefore means the RPC loop is alive, not that generated output is
ready. The separate output-status query keeps generation off the Morrowind
thread while still allowing the client to fail closed if generation fails.

RPC remains strictly serial. The pump deliberately overlaps only work that does
not issue a conflicting command: the landscape build overlaps client-side
static upload, and the static/grass quadtree build overlaps client-side grass
setup.

## World-resolution load gate

`MWPatches::patchResolveDuringInit` invokes `onResolveDuringInit()` from the
engine's save, new-game, and quick-start resolution paths. The callback sets
`worldResolved`.

If the upload pump is still active, the callback drains it synchronously inside
the engine load path. It runs work in 40 ms slices and updates an MGE loading
bar between slices. Because the engine is still blocked in loading code,
gameplay, AI, and physics cannot advance before distant land is ready.

Loading-bar updates render and present frames. `pumpDraining` prevents those
nested `Present` calls from also advancing the ordinary 8 ms pump. Host-wait
phases sleep briefly between polls so the drain does not busy-spin.

When the pump was already complete, the callback only supplies the second gate
and the render path enables immediately. When a later save is loaded while the
renderer is already `RenderReady`, the callback re-resolves dynamic-visibility
groups for the new world.

## Renderer restart and teardown

An in-world renderer restart has an active player cell, so `init()` uses
`initIpcBlocking()` and `uploadDistantLand()` rather than the menu pump. It
starts a fresh host, waits for output readiness, allocates vectors, uploads
terrain/statics/grass, and resolves dynamic-visibility groups before returning.

`release()` aborts any partial pump state, releases client-side D3D resources
and shared vectors, and stops the host. A subsequent initialization starts from
`Uninitialized`.

## Failure behavior

Any host loss, output failure, format-validation error, vector allocation
failure, resource creation error, or unsuccessful initialization RPC calls
`failUpload()` or the equivalent synchronous cleanup:

- stops the pump;
- releases partially created resources;
- sets state to `FailedDisabled`;
- clears `USE_DISTANT_LAND` for the session; and
- logs an error and surfaces it through the status overlay.

The engine load continues without distant land. The drain loop exits naturally
because failure clears `pumpActive`.

## Source map

| Area | Source |
| --- | --- |
| state, phases, pump, drain, resource upload | `d3d8/cpp/mge/distantland.h`, `distantinit.cpp` |
| world-resolution callback | `d3d8/cpp/mge/distantland.cpp` |
| first-`Present` trigger and pump tick | `d3d8/cpp/mge/mged3d8device.cpp` |
| engine load/resolve patch | `d3d8/cpp/mge/mwpatches.cpp` |
| loading bar | `d3d8/cpp/mge/mwbridge.cpp` |
| early host-launch gate | `d3d8/cpp/main.cpp`, `d3d8/cpp/mge/startupgen.cpp` |
| client launch, bootstrap polling, output status, RPC completion | `d3d8/cpp/ipc/client.*` |
| host startup-generation policy, worker, and published output state | `mgeHost64/src/startup_generation.rs`, `mgeHost64/src/main.rs`, `mgeHost64/src/ipc/server.rs` |
