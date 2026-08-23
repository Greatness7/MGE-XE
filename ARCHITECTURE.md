# MGE XE Architecture

MGE XE (Morrowind Graphics Extender XE) is a graphics extension for The Elder Scrolls III:
Morrowind. It has no source-level access to the game; everything it does is achieved by
**replacing `d3d8.dll`** in the Morrowind directory, intercepting the game's Direct3D 8 and
DirectInput 8 calls, and reading/writing the game process's memory directly. On top of that
interception layer it adds distant land rendering, distant statics, animated grass, dynamic
water, shadow maps, atmospheric scattering, post-process shaders, and an in-process scripting
API for MWSE/Lua.

This document is the architecture reference. Subsystem deep-dives live in
[`docs/architecture/`](docs/architecture/):

- [`render-pipeline.md`](docs/architecture/render-pipeline.md) — anatomy of a rendered frame:
  scene detection, render stages, draw-call recording, shaders.
- [`ipc.md`](docs/architecture/ipc.md) — the 32↔64-bit shared-memory RPC protocol and
  cross-process vectors.
- [`shadows.md`](docs/architecture/shadows.md) — two-cascade ESM sun shadows: atlas layout,
  cascade fitting, caster/receiver split, and the config surface.
- [`distantland-data.md`](docs/architecture/distantland-data.md) — generated data files,
  formats, and the generation pipeline.
- [`distantland-lifecycle.md`](docs/architecture/distantland-lifecycle.md) — startup upload,
  host/output overlap, readiness gating, load-path drain, and failure behavior.
- [`static-lod.md`](docs/architecture/static-lod.md) — component-preserving visibility LOD
  inside merged distant-static batches.
- [`horizon-culling.md`](docs/architecture/horizon-culling.md) — implemented terrain horizon-culling
  architecture, correctness contract, runtime policy, and tuning rationale.
- [`horizon-occlusion-asset.md`](docs/architecture/horizon-occlusion-asset.md) — serialized
  terrain-occluder format, validation, and runtime fallback contract.
- [`terrain-bin.md`](docs/architecture/terrain-bin.md) — byte-level terrain
  geometry format.
- [`indexed-skinning.md`](docs/architecture/indexed-skinning.md) — indexed matrix-palette skinning:
  the Morrowind engine hooks, vertex packer, capability handshake, and DXVK dependency.
- [`native-depth-capture.md`](docs/architecture/native-depth-capture.md) — native main-depth
  extraction, INTZ conversion, MSAA resolve interop, and replay fallback policy.

---

## 1. System overview

A configured installation runs up to three processes:

```
┌──────────────────────────────────┐      spawn + inherited handles
│ Morrowind.exe (32-bit)           │ ───────────────────────────────┐
│                                  │                                ▼
│  d3d8.dll   (MGE XE runtime)     │   shared memory   ┌─────────────────────────┐
│   ├─ proxydx: D3D8→D3D9 proxy    │ ◄───────────────► │ mgeHost64.exe (64-bit)  │
│   ├─ DistantLand renderer        │   RPC events      │  ├─ IPC server          │
│   ├─ MWBridge (memory access)    │                   │  ├─ quadtrees/culling   │
│   ├─ MGEAPI → MWSE.dll (Lua)     │                   │  └─ startup generation  │
│   └─ input wrapper, macros       │                   │     (distantland)       │
│  dinput8.dll (shim → d3d8.dll)   │                   └─────────────────────────┘
└──────────────────────────────────┘

┌────────────────────────────────────────────┐
│ MGEXEgui.exe (Rust/egui, 64-bit)           │   The two run independently: the GUI
│  writes game-root mgeXE.toml, including    │   never spawns the host, and the host
│  the [generation] job table                │   never draws UI. They meet only on
│  distant-land generation (distantland)     │   disk, through mgeXE.toml and the
└────────────────────────────────────────────┘   generated Data Files\distantland\.
```

| Binary | Project | Arch | Role |
| --- | --- | --- | --- |
| `d3d8.dll` | `d3d8` — empty `cdylib` shim; `build.rs` compiles the C++ in crate-local `cpp/` via `cc` | Win32 | The runtime. Loaded by Morrowind in place of the system d3d8.dll. All rendering, game-memory patching, input, and scripting support. |
| `dinput8.dll` | `dinput8` — same pattern, one C++ file | Win32 | Tiny shim: forwards `DirectInput8Create` to `d3d8.dll`, which hosts the real input wrapper. |
| `mgeHost64.exe` | `mgeHost64` cargo crate (Rust) | x64 | Helper process. Long-lived IPC host (visibility culling for the runtime) and automatic startup distant-land generation. Links the `distantland` crate for generation. No UI. |
| `MGEXEgui.exe` | `MGEXEgui` cargo crate (Rust/egui) | x64 | Configuration GUI. Edits game-root `mgeXE.toml`, structured input macros, and the shader chain through `mge-config`; uses targeted native profile calls for selected `Morrowind.ini` preferences; and owns distant-land configuration and generation in-process via `distantland`. |

`d3d8.dll` and `dinput8.dll` must stay 32-bit because Morrowind is a 32-bit process.
`mgeHost64.exe` and `MGEXEgui.exe` are deliberately 64-bit; only code loaded into the game
process must remain Win32.

### Files in a Morrowind install

| Path | Written by | Read by | Purpose |
| --- | --- | --- | --- |
| `mgeXE.toml` | MGEXEgui (and `MGEAPI::saveConfig`) | d3d8.dll and mgeHost64 through `mge-config`; distantland loader for the owned job table | Versioned MGE XE settings plus the independently versioned `[generation]` job. Parseable documents load known values tolerantly and normalize on save; malformed TOML remains read-only. Restore Defaults preserves the raw generation table; exported copies omit it. The old `MGE3\MGE.ini` is ignored and never migrated, overwritten, or deleted. |
| `MGE3\MGE XE Default Statics Classifiers.toml` | shipped assets | generator | Default global distant-land overrides, enabled in new generation jobs and written as a commented example of the mod-metadata schema. External GUI localization files are intentionally not supported in this schema revision. |
| `Data Files\distantland\…` | MGEXEgui, or mgeHost64 startup generation (journaled in-place, one exclusive writer) | d3d8.dll + mgeHost64 host | Generated distant-land data set. See [distantland-data.md](docs/architecture/distantland-data.md). |
| `Data Files\shaders\core` / `core-mods` / `XEshaders` | shipped assets / users | d3d8.dll | Core HLSL effects, core shader-mod overlays, and user post-process shaders. Compiled at runtime, not into the DLL. |
| `mgeXE.log`, `mgeHost64*.log` | runtime / host | users, MGEXEgui log viewer | Diagnostics. |

---

## 2. Repository map

| Path | Contents |
| --- | --- |
| `d3d8/cpp/main.cpp`, `d3d8/cpp/exports.def` | DLL entry point and exports (`Direct3DCreate8` → `FakeDirect3DCreate`, `DirectInput8Create` → `FakeDirectInputCreate`). |
| `d3d8/cpp/proxydx/` | Plain D3D8→D3D9 and DirectInput8 proxy layer (no MGE logic). `d3d8header.h` defines the D3D8 interfaces; everything real is D3D9. |
| `d3d8/cpp/mge/` | The runtime proper: device hooks, DistantLand renderer, MWBridge, configuration, post shaders, input macros, HUD. |
| `d3d8/cpp/ipc/` | 32-bit IPC **client**: RPC client, shared-vector views, distant-land share state. The 64-bit server lives in `mgeHost64`. |
| `d3d8/cpp/mwse/` | MWSE integration: script-function opcodes (`func*.cpp`) and the bridge that hands `MGEAPI` to MWSE (`mgebridge.cpp`). |
| `d3d8/cpp/support/` | Logging, crash logger, PNG screenshot writer, high-resolution timer. |
| `d3d8/crates/` | Private format contract test crates (`dl-contract-test`, `config-contract-test`). |
| `dinput8/cpp/` | The standalone `dinput8.dll` shim. |
| `mgeHost64/` | Rust crate for `mgeHost64.exe`. |
| `distantland/` | Distant-land generator. A subtree, not a nested workspace — it and the crates under `distantland/crates/` are all root-workspace members. Has its own `ARCHITECTURE.md`. |
| `MGEXEgui/` | Rust/egui GUI crate. |
| `mge-config/` | Shared config schema and TOML document layer crate. |
| `xtask/` | Build orchestration: cross-target builds, release packaging, deployment. Replaced `RELEASE.vcxproj` and the per-project post-build copies. |
| `assets/` | Shipped data: HLSL shaders, MWSE Lua mod (`MGE XE Options` MCM), meshes/textures, and `mge3` (the default distant-land classifiers TOML). `cargo xtask build` mirrors this tree into `bin\MSVC-<Configuration>\`. |

---

## 3. Build system

The product build is one Cargo workspace. There is no Visual Studio solution and no MSBuild
step — the C++ compiles from `build.rs` via the `cc` crate. VS *Build Tools* remain required
(`cl.exe`, and the MSVC `LIB`/`INCLUDE` that `rustc` resolves to link); the IDE does not.

```powershell
cargo xtask build --release      # all four binaries + assets\ into bin\MSVC-Release\
cargo xtask deploy --release     # install into a live Morrowind directory
cargo xtask build --max-perf     # maximum-performance build into bin\MSVC-MaxPerf\
```

- Outputs land in `bin\MSVC-<Configuration>\`.
- **`d3d8`** is a `cdylib` named `d3d8` targeting `i686-pc-windows-msvc`, so the output
  file is `d3d8.dll`. `lib.rs` is empty; `build.rs` globs `cpp\**\*.cpp`, applies the historical flags (`/std:c++17 /arch:SSE2 /fp:fast /GR- /EHsc
  /Z7`), and passes `d3d8\cpp\exports.def` as `/DEF:` so the `Direct3DCreate8=FakeDirect3DCreate`
  aliases resolve against the `_stdcall`-decorated symbols.
- **`dinput8`** is the same pattern over the single `dinput8\cpp\dinput.cpp`.
- **`mgeHost64`** and **`MGEXEgui`** build as ordinary x64 Rust binaries; both also build and
  test standalone with plain `cargo build` / `cargo test` from their own directories.
- **`xtask`** drives the two target triples (a single `cargo build` cannot span i686 and x64),
  collects artifacts, mirrors `assets\` alongside them, and deploys. The Morrowind directory
  comes from `--morrowind-dir`, `MGE_XE_MORROWIND_DIR`, or a gitignored `mge-xe-local.toml`
  at the repo root — this replaced `MGE-XE.User.props`.
- `default-members` excludes the 32-bit crates so a bare `cargo build` stays sane. Release
  DLLs use the `release-dll` profile (`release` plus `debug = true`, `strip = false`) so
  `d3d8.pdb` is still produced.
- `--max-perf` swaps in the `max-perf`/`max-perf-dll` profiles (`codegen-units = 1`, fat LTO),
  enables the `d3d8/max-perf` feature for the C++ side, and uses a separate
  `bin\MSVC-MaxPerf\` output tree. All profiles live in the workspace-root `Cargo.toml`;
  member and path-dependency `[profile.*]` sections are ignored by Cargo.
- `cc` does not supply `/EHsc` (its absence silently disables unwind semantics — C4530).
- MSVC whole-program optimization (`/GL` + `/LTCG`) is available under `--max-perf`, gated by
  the `d3d8/max-perf` feature. It requires `link.exe` (pinned in `.cargo/config.toml`), which
  in turn requires that `d3d8\cpp\main.cpp` reach the linker as a standalone object rather than an
  archive member — `build.rs` uses `compile_intermediates()` for exactly this. Folding it back
  into the archive reproduces `LNK2005: _DllMain@12 already defined in
  msvcrt.lib(dll_dllmain_stub.obj)`.
- Dependencies: DirectX SDK June 2010 (d3dx9/effects) and a Rust toolchain.
  Runtime dependency: d3dx9 runtime (DirectX 9 redist).

Version constants that must stay in sync are described in [§10](#10-versioning--compatibility-contracts).

---

## 4. The d3d8.dll runtime

### 4.1 Startup and injection

`DllMain` (`d3d8/cpp/main.cpp`) runs when Morrowind (or the Construction Set) loads the DLL:

1. Detects the host process. For `TES Construction Set.exe` it only chain-loads `CSSE.dll`
   and otherwise stays inert. For `Morrowind.exe`:
2. Opens `mgeXE.log`, installs the crash logger, sets DPI awareness.
3. `Configuration.LoadSettings()` opens game-root `mgeXE.toml` through the narrow Rust FFI.
   `mge-config` owns schema/default/validation behavior; `inidata.h` only maps TOML paths
   into a staging `ConfigurationStruct`. A missing file uses embedded defaults in memory, while
   an invalid existing file uses defaults with writes disabled. `MGE_DISABLED` still selects the
   system d3d8 path.
4. `MWInitPatch::patch()` applies early patches (borderless window/UI scale hook when
   running proxy-only, skip intro movies, texture-load memory reduction, high-resolution
   frame timer, particle material fix).
5. Loads `MWSE.dll` if present/enabled and hands it the `MGEAPI` instance (§4.8).

When Morrowind calls `Direct3DCreate8`, the exported `FakeDirect3DCreate`:

- launches the persistent 64-bit host early when the existing MGE/startup/distant-land gates pass
  (`StartupGeneration::launchEarlyHost`; mgeHost64 reads the embedded job and owns missing/invalid
  job status plus the `automatic_rebuild` decision), and
- returns `MGEProxyD3D` wrapping a real `IDirect3D9`. In `OnlyProxyD3D8To9` mode a plain
  `ProxyD3D` is returned instead — pure API translation, no MGE features.

`CreateDevice` then yields `MGEProxyDevice`, the central hook object (recreated on Alt-Tab
device loss; its constructor re-initializes per-device state and re-registers the device with
`DistantLand`).

### 4.2 proxydx: D3D8→D3D9 translation

`d3d8/cpp/proxydx/` contains inert proxy classes (`ProxyD3D`, `ProxyDevice`, `ProxyTexture`,
`ProxySurface`, `ProxyDirectInput*`) that implement the D3D8 interface surface
(`d3d8header.h`) on top of D3D9. MGE behaviour is added by subclassing (`MGEProxyD3D`,
`MGEProxyDevice` in `d3d8/cpp/mge/`, `MGEProxyDirectInput` for input). Keep API translation in
`proxydx/` and MGE logic in the `MGEProxy*` subclasses.

### 4.3 MGEProxyDevice: the hook points

`d3d8/cpp/mge/mged3d8device.cpp`. Morrowind renders a frame as multiple scenes
(BeginScene/EndScene pairs): opaque world → optional stencil-shadow scenes → alpha-sorted →
1st person/sunglare → UI. The device wrapper tracks `sceneCount` and per-frame flags and
drives `DistantLand` from these hooks:

| Hook | What MGE does there |
| --- | --- |
| `SetTransform` | Detects UI scenes from the view matrix (`detectMenu`); applies camera-effect matrix (zoom/shake/rotation) to the view; rewrites the projection's near/far planes (`DistantLand::setProjection`). Records transforms. |
| `BeginScene` | Increments `sceneCount` for main-view scenes; first scene applies custom FOV. On the first UI scene runs post-processing (`DistantLand::postProcess`) and draws the MGE user HUD. Initializes HUD/overlay once per device. |
| `DrawIndexedPrimitive` | The central interception. Fills a `RenderedState` snapshot and calls `DistantLand::inspectIndexedPrimitive`, which records z-writing draws for later passes, captures the sky, redirects fixed-function draws to the FFE shader, and can skip the call. Water-material draws are replaced by MGE's water (`renderStageWater`). Triggers Stage 0 at the first suitable draw. |
| `EndScene` | Scene 0: ensures Stage 0 ran, then Stage 1 (grass/shadows/depth) and StageBlend. Scenes 1+: Stage 2 and (if needed) StageWater. After the UI: status overlay + post-UI screenshot capture. |
| `SetRenderState` | Records state; suppresses Morrowind's fog-mode/range states (MGE owns fog); tracks stencil/ambient for scene classification (`isAmbientWhite` marks sky/menu rendering). |
| `SetLight` / `SetMaterial` / `SetTexture` / `SetStreamSource` / `SetIndices` / `SetTextureStageState` | State recording for the FFE shader and draw-call recording. Light 6 is the sun; material `Power == 99999` marks the water node (planted via `MWBridge::markWaterNode`); emissive alpha `88888` marks moon geometry. Sampler states are overridden to force anisotropic/trilinear filtering. |
| `Present` | End of frame: connects MWBridge on first opportunity and installs load-time patches; updates crosshair autohide, zoom/shake controllers, main-menu video patch; resets per-frame flags; ticks the distant-land **upload pump** (§4.5) with an 8 ms budget. |
| `Clear` | Captures the horizon colour. |
| `Release` (refcount 0) | Releases DistantLand, HUD, overlay resources. |

The full frame walkthrough is in [render-pipeline.md](docs/architecture/render-pipeline.md).

### 4.4 Morrowind memory and executable patches

`d3d8/cpp/mge/mwbridge.h/.cpp` is a singleton that accesses Morrowind's memory at **hardcoded
addresses** (tied to one specific Morrowind executable layout). It provides:

- Game-state reads: weather, fog, wind, sun direction/visibility, water level, cell
  type/name, player position/camera/target, menu/loading state, simulation time, GMSTs,
  globals, journal indices, object references.
- Game-state writes: view distance, FOV, fog colour via scenegraph, UI scale, crosshair,
  haggle amounts.
- Scenegraph tagging and loading-bar calls that traverse bridge-owned engine state.

`d3d8/cpp/mge/mwpatches.h/.cpp` owns fixed-address executable patch implementations,
including the load callback, save-resolution callback, alpha-accumulation split, screenshot
suppression, splash-screen correction, and frame-timer redirection. Call sites retain the
lifecycle decision: DLL attach, device creation, first usable `Present`, or distant-land init.

`d3d8/cpp/mge/mwtextureloader.h/.cpp` owns the NetImmerse DDS vtable hooks, BC7 extension,
and staging-to-default-pool upload path. Its hooks install during DLL attach; device creation
later records whether the active D3D9 runtime supports BC7.

There is no symbol derivation: when Morrowind's layout changes, offsets in `mwbridge.cpp`
and patch sites in `mwpatches.cpp`/`mwtextureloader.cpp` must be updated by hand.

### 4.5 DistantLand lifecycle

`DistantLand` (`d3d8/cpp/mge/distantland.h`, `distantinit.cpp`, `distantland.cpp`, `render*.cpp`)
is an all-static class — effectively a namespace with state — owning every MGE-rendered
feature. At the first usable `Present`, initialization creates device resources
and arms a frame-budgeted startup pump. Its phases poll host bootstrap and
generated-output readiness, allocate shared vectors, overlap host landscape
construction with resumable client static upload, and overlap host
static/grass construction with client grass setup.

The render path enables only when both geometry upload and the save/new-game
world resolution have completed. If world resolution wins the race, the
resolution callback drains the pump synchronously inside the engine load path
while displaying an MGE loading bar, so gameplay cannot begin in a partially
initialized state. An in-world renderer restart retains the synchronous
end-to-end path. Any failure releases partial resources, disables distant land
for the session, and lets Morrowind continue.

Upload details (what is read from `Data Files\distantland`, what is sent to the host) are in
[distantland-data.md](docs/architecture/distantland-data.md) and [ipc.md](docs/architecture/ipc.md).
The complete state machine is in
[distantland-lifecycle.md](docs/architecture/distantland-lifecycle.md).
Gate every render-path use on `canRenderDistantLand()`; gate resource cleanup on
`hasDeviceResources()`.

Per-frame visibility works by **querying the 64-bit host**: the client sends a view frustum
and flags, the host walks its quadtrees and writes `RenderMesh` records (containing 32-bit
D3D9 buffer/texture pointers previously registered by the client) into a shared vector that
the client then draws. Dynamic visibility groups (quest/global/object-gated distant statics)
are scanned on cell change (`scanDynamicVisGroups`) and pushed to the host.

### 4.6 Render stages (summary)

- **Stage 0** (start of scene 0, after sky): select worldspace, update per-frame shader
  state and fog; shadow-map early render; distant terrain; distant statics (with alpha
  dissolve near the Morrowind boundary); atmospheric-scattering sky; water reflection
  render; ripple simulation; saves the distant-only frame for blending.
- **Stage 1** (end of scene 0): grass (instanced, wind-animated), shadow overlay on near
  geometry, depth texture from recorded draws.
- **Stage 2** (end of scenes 1+, pre-UI): shadow overlay + depth for late scenes
  (post-stencil redraw, alpha, 1st person).
- **StageBlend** (after Stage 1): underwater caustics, then distance-blend between
  Morrowind's scene and MGE's distant land.
- **StageWater** (replaces Morrowind's water draw): reflective/refractive water plane with
  dynamic waves, or underwater variant.
- **postProcess** (first UI scene): the post-shader chain (§4.7), HDR adaptation readback,
  menu render caching.

See [render-pipeline.md](docs/architecture/render-pipeline.md).

### 4.7 Shaders

Three shader systems coexist:

- **Core effects** (`assets/Data Files/shaders/core/*.fx`): `XE Main.fx` (distant land,
  statics, grass, water, shadows, blend passes — pass IDs in `distantshader.h`),
  `XE Depth.fx`, `XE Shadowmap.fx`, `XE FixedFuncEmu.fx`, `XE HUD.fx`, sharing parameters
  via an `ID3DXEffectPool`. Users can drop overlays into `shaders/core-mods/`; they are
  composed at compile time by `createCoreEffectWithMods` (`CoreModInclude` include handler).
- **Fixed-function emulation** (`d3d8/cpp/mge/ffeshader.cpp`): when per-pixel lighting is on,
  `inspectIndexedPrimitive` routes Morrowind's fixed-function draws to generated shader
  permutations keyed by a packed `ShaderKey` (texture stages, skinning, fog mode, lighting),
  cached per key.
- **Post-process chain** (`d3d8/cpp/mge/postshaders.cpp`, user shaders in
  `Data Files/shaders/XEshaders/`): double-buffered fullscreen passes ordered by priority,
  with standard variables (`EV_*`: depth frame, eye/sun vectors, fog, time, HDR, and an
  optional frame-local list of up to 32 world-space point lights) bound per frame, async
  initial load, and live reload support via the API.

### 4.8 MWSE integration and the public API

- `d3d8/cpp/mwse/mgebridge.cpp` runs when `MWSE.dll` is loaded: it registers ~120 script-function
  opcodes (`d3d8/cpp/mwse/func*.cpp` — HUD, weather, camera/zoom/shake, shaders, raycasts,
  entity/GMST access) against MWSE's VM, supporting both modern MWSE (exported
  `MWSEAddInstruction`) and the legacy bundled 0.9.4a via hardcoded offsets.
- Modern MWSE additionally receives `api::MGEAPI` (`d3d8/cpp/mge/api.h/.cpp`), a versioned
  C++ vtable interface (currently v3) exposing feature toggles, distant-land render config,
  camera/zoom/shake, HUD management, post-shader variable access, weather scattering, and
  screenshots. This is what `mwse.mge`/Lua uses, including the shipped
  `assets/Data Files/mwse/mods/MGE XE Options` MCM menu.
- `d3d8/cpp/mge/macrofunctions.cpp` + `mgedinput.cpp` implement hotkey macros (screenshot,
  toggles, view-range/zoom/FOV adjust, haggle, 3rd-person camera nudge) and input shaping
  (tap/push/hammer/disallow), configured from structured `input.macros`, `input.triggers`, and
  `input.remap` TOML values. `mge-config` renders those into the existing runtime multi-string
  buffers.

### 4.9 Auxiliary modules

- `statusoverlay.cpp` — versioned status/FPS overlay text (also error display).
- `userhud.cpp` — up to 256 scriptable HUD elements (texture + optional effect).
- `videobackground.cpp` — Bink-video main-menu background replacement.
- `morrowindbsa.cpp` — BSA/loose-file texture loading + cache for distant statics and HUD.
- `support/crashlog.cpp` — vectored exception handler writing crash dumps/logs.
- `support/pngsave.cpp` — PNG screenshot encoding; `doublesurface.h` — RT double buffering.
- `visibleset.cpp` — `VisibleSet`, the draw loop over host-streamed `RenderMesh` records
  (all culling quadtrees live in the host).
- `dlformat.h` — length-aware decoder for the host-pinned distant-land output descriptor.
  Build its Win32 harness with `cargo build -p dl-contract-test --target i686-pc-windows-msvc
  --release`. Run it with no arguments for the compile-time `MGE_DL_VERSION` assertion, or pass
  the root of a generated tree (the `Data Files` directory of a Morrowind install) to validate
  `distantland/version`, every `static_meshes_N` shard header, the shard-count sum against
  `distantland/statics/usage.data`, and the `terrain.bin` magic and version.
  Note: `run_mge_xe_descriptor_vectors.py`, which drove a 16-case descriptor corpus against this
  harness, no longer exists — it was removed in a `distantland` cleanup.

---

## 5. mgeHost64 (the 64-bit host)

Rust crate `mgeHost64/`. One binary,
one mode, one launcher:

| Invocation | Behaviour |
| --- | --- |
| four hex handles as the whole command line | **IPC host**: services RPC commands from `d3d8.dll` until the game exits. A worker thread runs the startup-generation policy alongside it (may regenerate distant land or disable it for the session). |
| anything else, including no arguments | Rejected by `win::parse_startup_handles` with a message naming the likely cause, logged to `mgeHost64.log`; exits 1. The host is not meant to be run by hand. |

The native Win32 configuration wizard that used to live here behind `--configure` was deleted
along with `src/config_ui/`; `MGEXEgui.exe` hosts those settings and calls `distantland`
in-process (§8).

As IPC host it owns the distant-land world state: per-worldspace quadtrees (near/far/very-far
statics, grass), the landscape quadtree, and dynamic-visibility groups. It answers
visibility queries by streaming `RenderMesh` records into shared vectors. It re-reads
`usage.data` and `terrain.bin` itself (64-bit address space) — the 32-bit client only
uploads D3D resources and registers their pointers. For version 16, the host validates and pins the
complete `generation_state.bin` inventory under a retained shared lock. The client opens the fixed
the terrain and 128 static-shard paths directly.

Generation itself lives in the **`distantland/` subtree** — a set of root-workspace
member crates, documented by `distantland/ARCHITECTURE.md`: ESM/ESP parsing, terrain
mesh/atlas building, static mesh reduction, and the output-contract types that both the
GUI's generator window and the host's startup generation drive.

---

## 6. IPC between d3d8.dll and mgeHost64

Full protocol description: [docs/architecture/ipc.md](docs/architecture/ipc.md). In short:

- The client (`d3d8/cpp/ipc/client.*`) spawns the host with four inherited handles: a shared
  `Parameters` block mapping, the client process handle, and two events (`rpcStart`,
  `rpcComplete`). One RPC at a time: write parameters → signal start → host dispatches on
  `Command` → host signals complete.
- Bulk data moves through **shared vectors** (`SharedVec` on the Rust side; `vecbase`/
  `view`/`vecwrap` on the C++ side): named-mapping-backed, growable, type-tagged arrays
  addressed by `VecId`, with sliding-window views on the 32-bit side (using
  `MapViewOfFile3`/`VirtualAlloc2`, hence the Windows 8/10+ requirement).
- ABI rules: every shared struct is `#pragma pack(4)` / `#[repr(C)]`, uses `ptr32`/`ptr64`
  wrappers for one-sided pointers, exists in **both** `d3d8/cpp/ipc/bridge.h` and
  `mgeHost64/src/abi/`, and gets a layout assertion in
  `mgeHost64/src/abi/layout_tests.rs`. Change both sides together.
- Distant-land payloads use fixed paths. The Rust host pins and validates the
  complete-or-absent state inventory; no selected-generation path descriptor crosses IPC.

---

## 7. Distant land data and generation

Full inventory and flow: [docs/architecture/distantland-data.md](docs/architecture/distantland-data.md).
Key contracts:

- `Data Files\distantland\version` — one byte. Production generation and all current readers require version 16.
- `terrain.bin` (`XELAND02`) + five DDS textures (atlas, material, material flags, patch
  albedo, blend patterns) — terrain geometry and the texture-atlas scheme. Byte-level spec:
  [`terrain-bin.md`](docs/architecture/terrain-bin.md).
- all 128 fixed `statics\static_meshes_000..127` files (`XESTAT05` v5) — static mesh geometry/subsets plus
  merged-component provenance used for cumulative static LOD face counts. Loaded by the
  32-bit client for geometry upload; completed metadata is sent to the host over IPC.
- `statics\usage.data` — per-worldspace placements, dynamic-vis groups, and trailing
  metadata. Loaded by the 32-bit client for dynamic-vis groups and by the 64-bit host for
  placements/quadtrees.
- Mod authors influence generation via per-plugin `-metadata.toml` files
  ([guide](mod-metadata-guide.md)) and legacy `.ovr` classifier files.

Generation runs through `distantland::ensure_generated`, called in-process by
`MGEXEgui.exe`'s generator window or by `mgeHost64.exe`'s startup-generation worker. Version-16
output commits in place through the exclusive writer lock and sole `generation_state.bin`
publication authority; journal/index, staging, quarantine, and directory promotion are retired.

---

## 8. MGEXEgui

Native Rust/egui configuration tool. Responsibilities:

- Edits game-root `mgeXE.toml` through the shared `mge-config` typed schema and
  `toml_edit` document layer. Unknown keys, comments, whitespace, and relative item order
  survive targeted saves; revisions and atomic replacement guard concurrent edits.
- Manages graphics and distant-land runtime settings, per-weather fog/wind and lighting,
  structured macros/triggers/key remaps, shader-chain/source/flags, Morrowind preferences
  through native `Get/WritePrivateProfile* A` calls, and the 32-bit registry view.
- Enumerates display modes with Win32. It intentionally has no DirectX device, shader
  compiler, or preview renderer; the game runtime owns shader errors and live preview.
- Owns **distant-land configuration and generation**. The Distant Land tab's generator
  button opens a second native window (an egui child viewport) holding the Plugins,
  Landscape, Statics and Advanced settings, edits the `[generation]` table in `mgeXE.toml`, and runs
  `distantland::ensure_generated` on a worker thread with per-stage progress. An
  existing incompatible tree is classified when that window opens: consent is asked
  before a run replaces it, and a tree from a newer format is refused outright. After a
  successful run the generated set is re-inspected and activated in the render settings.
- Embeds English, French, Polish, and Russian `rust-i18n` catalogs. `gui.language`
  stores `auto` or an embedded locale code; the system locale is reduced to the
  best available language and English is the fallback. See
  [localization.md](docs/gui/localization.md).

---

## 9. Assets

`assets/` ships verbatim into the release package (and to `$(MorrowindDir)` when configured):

- `Data Files/shaders/core` — core pipeline effects (must match the runtime's pass/variable
  expectations; `distantshader.h` and `DistantLand` effect-handle lookups bind to them).
- `Data Files/shaders/core-mods` — optional user/community overlays composed into the core
  effects at load (`XE Mod *.fx`).
- `Data Files/shaders/XEshaders` — default post-process shaders (SSAO, bloom, DoF, etc.).
- `Data Files/mwse/mods/MGE XE Options` — Lua MCM menu driving the in-game settings UI via
  the MGE API.
- `Data Files/meshes`, `textures` — replacement meshes/textures used by features.
- `mge3/` — the default global statics-classifier TOML (`MGE XE Default Statics Classifiers.toml`)
  that new generation jobs enable. Legacy `.lng` assets were removed; GUI translations are
  embedded from `MGEXEgui/locales/`.

---

## 10. Versioning & compatibility contracts

These constants gate interop and must move together:

| Constant | Where | Pair(s) |
| --- | --- | --- |
| `XE_VERSION_STRING` / `MGE_*_VERSION` | `d3d8/cpp/mge/mgeversion.h` | `VERSION_NUMBER` / `VERSION_STRING` and package version in `MGEXEgui` |
| `mge_config::SCHEMA_VERSION` (3) | `mge-config/src/schema.rs` | root `schema_version` in `mgeXE.toml`; mismatches warn, known fields load, and the current version is written on save |
| `MGE_SAVE_VERSION` (47) | `mgeversion.h` | Legacy constant only; no longer governs persistent configuration |
| `MGE_DL_VERSION` (16) | `mgeversion.h` (distantland data compat) | Rust-host ABI constant, generator output `version` file. `MGEXEgui` keeps no copy — it uses `distantland::MGE_DL_VERSION`. Both remaining copies are pinned to the generator by tests in `mgeHost64/src/abi/constants.rs`, which parse `mgeversion.h` directly |
| IPC ABI structs | `d3d8/cpp/ipc/bridge.h` | `mgeHost64/src/abi/*` + `layout_tests.rs` |
| `terrain.bin` layout | `d3d8/cpp/mge/dlformat.h` (`TerrainBin`) + static asserts | `mgeHost64/src/abi/terrain.rs`, `distantland` writer, [`terrain-bin.md`](docs/architecture/terrain-bin.md) |
| Fixed `static_meshes_000..127` v5 layout/order | `d3d8/cpp/mge/dlformat.h` (`StaticMeshesBin`), `d3d8/cpp/mge/distantinit.cpp` | `distantland` writer; client loader builds concatenated `DistantSubset` records consumed by host `state/loading.rs` |
| Host exit codes | `mgeHost64/src/error.rs` | C++ launcher interpretation |
| Core shader passes/variables | `d3d8/cpp/mge/distantshader.h`, `distantland.h` handles | `assets/Data Files/shaders/core/*.fx` |

Also sticky: Morrowind memory offsets (`mwbridge.cpp`), the D3D8 interface layout
(`proxydx/d3d8header.h`), and MWSE opcode numbers (`d3d8/cpp/mwse/mgebridge.cpp`).

---

## 11. Document index

Every doc in the repository. The runtime deep-dives are also linked from the top of this file.

### Root

| Doc | What it is |
| --- | --- |
| [`README.md`](README.md) | User-facing overview and install instructions. |
| [`BUILD.md`](BUILD.md) | Prerequisites, targets, profiles, packaging, deployment. Source of truth for building. |
| [`CHANGELOG.md`](CHANGELOG.md) | Release notes. |
| `ARCHITECTURE.md` | This reference. |

### Runtime deep-dives ([`docs/architecture/`](docs/architecture/))

| Doc | What it is |
| --- | --- |
| [`render-pipeline.md`](docs/architecture/render-pipeline.md) | Frame anatomy: scene detection, render stages, draw-call recording, shaders. |
| [`ipc.md`](docs/architecture/ipc.md) | The 32↔64-bit shared-memory RPC protocol and cross-process vectors. |
| [`shadows.md`](docs/architecture/shadows.md) | Two-cascade ESM sun shadows: atlas, cascade fitting, casters vs receivers, config and runtime control. |
| [`distantland-data.md`](docs/architecture/distantland-data.md) | Generated data set under `Data Files\distantland`: inventory, producers, consumers, formats. |
| [`distantland-lifecycle.md`](docs/architecture/distantland-lifecycle.md) | Startup upload, overlap, readiness, drain, restart, and failure contract. |
| [`static-lod.md`](docs/architecture/static-lod.md) | Implemented cumulative visibility LOD for merged static batches. |
| [`terrain-bin.md`](docs/architecture/terrain-bin.md) | Byte-level `terrain.bin` contract. |
| [`horizon-culling.md`](docs/architecture/horizon-culling.md) | Implemented terrain horizon-culling architecture and operational contract. |
| [`horizon-occlusion-asset.md`](docs/architecture/horizon-occlusion-asset.md) | Serialized `terrain_occlusion.bin` format, validation, and fallback contract. |
| [`indexed-skinning.md`](docs/architecture/indexed-skinning.md) | Implemented indexed matrix-palette skinning: engine hooks, packer, capability gate, rollback. |
| [`native-depth-capture.md`](docs/architecture/native-depth-capture.md) | Implemented native depth extraction: INTZ conversion, DXVK MSAA resolve, provenance gates, replay fallback. |
| [`dxvk-ppl-interop.md`](docs/architecture/dxvk-ppl-interop.md) | Cross-repo ABI with our DXVK fork: shared headers, capability negotiation, packet semantics, silent-breakage list. |
| [`mwse-api.md`](docs/architecture/mwse-api.md) | Cross-binary MWSE contract: versioned vtable, opcode surface, 0.9.4a fallback, silent-breakage list. |
| [`mwbridge.md`](docs/architecture/mwbridge.md) | Engine memory map and patches: the 20 static anchors verified against Morrowind 1.6.1820, and the two-phase install. |

### Configuration and GUI

| Doc | What it is |
| --- | --- |
| [`mge-toml.md`](docs/configuration/mge-toml.md) | Persistent configuration schema, ownership, load/write behavior, and legacy-key inventory. |
| [`localization.md`](docs/gui/localization.md) | Embedded GUI catalogs, locale resolution, key contract, and contributor workflow. |

### Distant-land generator ([`distantland/`](distantland/))

A subtree of root-workspace members, with a separate document set.

| Doc | What it is |
| --- | --- |
| [`distantland/ARCHITECTURE.md`](distantland/ARCHITECTURE.md) | Primary generator reference: module map, pipeline walkthrough, output contract, settings. |
| [`vfs.md`](distantland/docs/architecture/vfs.md) | Data-directory + BSA stack, case-insensitive resolution. |
| [`statics.md`](distantland/docs/architecture/statics.md) | Plugin references and NIF meshes to `static_meshes_*` shards, `usage.data`, and atlases. |
| [`terrain.md`](distantland/docs/architecture/terrain.md) | `terrain.bin` plus the five terrain DDS outputs. |
| [`binary-formats.md`](distantland/docs/architecture/binary-formats.md) | On-disk structures for the four custom binary outputs — contracts with the runtime. |
| [`storage-foundation.md`](distantland/docs/architecture/storage-foundation.md) | Normative complete-or-absent store contract. |
| [`caching-and-startup.md`](distantland/docs/architecture/caching-and-startup.md) | Fingerprints, domain gates, startup handoff. |
| [`incremental-generation.md`](distantland/docs/incremental-generation.md) | Making generation proportional to changed inputs. |
| [`grass-plugin-list.md`](distantland/docs/development/grass-plugin-list.md) | The optional ordered `grass_plugins` job field. |
