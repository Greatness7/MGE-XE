# Render pipeline

How MGE XE turns Morrowind's D3D8 frame into the final image. Companion to
[`ARCHITECTURE.md`](../../ARCHITECTURE.md) §4.3/§4.6. Code: `d3d8/cpp/mge/mged3d8device.cpp`
(hooks and scene tracking), `d3d8/cpp/mge/distantland.cpp` (stage orchestration),
`d3d8/cpp/mge/render*.cpp` (stage implementations), `d3d8/cpp/mge/distantinit.cpp` (resources).

## 1. Morrowind's frame structure

Morrowind renders one frame as several BeginScene/EndScene pairs against the back buffer:

```
scene 0   opaque world (plus "No Sorter" alpha meshes), preceded by the sky
[scene k] 2x stencil-shadow scenes, if any actors cast stencil shadows
[scene k] post-stencil redraw of shadow casters
[scene k] sorted alpha meshes
[scene k] z-clear, then 1st person weapon / sunglare
scene N   UI (and possibly extra scenes after it, e.g. race menu)
```

MGE XE does not control this structure; it classifies scenes as they happen:

- `MGEProxyDevice` counts main-view scenes in `sceneCount` (reset each `Present`).
- `SetTransform(D3DTS_VIEW)` recognizes UI scenes by matrix shape (`detectMenu`). Morrowind
  never sets an orthographic projection for UI, so the view matrix is the tell.
- MGE uses `isAmbientWhite` (`D3DRS_AMBIENT == 0xffffffff`) to mark skydome/menu rendering
  and delay Stage 0 until Morrowind draws the sky.
- MGE recognizes stencil-shadow scenes via `D3DRS_STENCILENABLE`/`STENCILREF` and skips
  draw-call inspection in them (`stencilRef <= 1`).
- MGE recognizes the water plane by material. `MWBridge::markWaterNode` patches the water's
  material `Power` to `99999.0` once per session, and `SetMaterial` watches for it.
  MGE likewise identifies moon geometry by emissive alpha `88888` (`kMoonTag`).
- `MWPatches::patchWorldRenderingAccumulation` patches Morrowind to accumulate alpha meshes
  into their own scene even on the code path without water, keeping the scene order normative.

## 2. Draw-call recording

The proxy snapshots every `DrawIndexedPrimitive` in a main-view, non-stencil scene
into a `RenderedState` (`d3d8/cpp/mge/ffeshader.h`): current VB/IB/FVF, texture 0,
world/view transforms, material, blend/alpha-test/fog/lighting state, plus the DIP
arguments. Alongside it, the proxy shadows per-stage fragment state in `FragmentState`
(colour/alpha ops, bump-env, the bound texture and texture transform of each of the 8
stages) and lighting state in `LightState` (incl. `ambientWhite`, mirroring
`D3DRS_AMBIENT == 0xffffffff`). Those three shadowed fields keep the FFE path from reading
state back from the device mid-draw. See §7.
`DistantLand::inspectIndexedPrimitive` then decides:

- **Record for later passes.** Appends any z-writing draw to `recordMW` as a
  `RecordedState`, which AddRefs the buffers. Excludes two multi-pass patterns:
  landscape alpha splatting (same VB redrawn blended with vertex colour in scene 0
  exteriors) and decal passes on UV sets > 0, because the later shadow/depth passes only
  sample texture 0/UV 0. Normalizes alpha-test references to `GREATEREQUAL` semantics.
- **Capture the sky.** Before the inspector records a z-writing draw in scene 0 of a weather
  cell, blended draws are the skydome/clouds/moons and go to `recordSky`. With atmospheric
  scattering enabled, MGE suppresses the original draw (returns `false`) and redraws the
  sky later in Stage 0 (`renderSky`: scattering for the dome, separate cloud pass; fixed
  vertex/primitive counts identify clouds and moon billboards).
- **Replace fixed-function rendering.** When per-pixel lighting is active
  (`USE_FFESHADER` + `isPPLActive`), `FixedFunctionShader::renderMorrowind` renders the draw
  with a generated shader and MGE suppresses the original call (§7).
- **Replace the water.** The device hook handles this one level up. `distantWater` is set from
  `USE_DISTANT_LAND || USE_DISTANT_WATER` — either flag alone enables it — and on an eligible
  tagged water draw MGE suppresses the original grid and runs `renderStageWater` once.
  `renderStageWater` itself does nothing unless `CellHasWater()`.

Each stage consumes and clears `recordMW`; Stage 0 consumes and clears `recordSky`.

## 3. Stage 0, distant world (start of scene 0)

The first suitable draw call of scene 0 triggers `DistantLand::renderStage0`. If Morrowind
culled everything, EndScene triggers it instead.

Every Stage-0 call opens with `selectDistantCell`, which runs only under `USE_DISTANT_LAND`:
it rescans dynamic-vis groups when the player-cell pointer changes, then selects the host
worldspace on every call, setting `hasCurrentWorldSpace` — the flag `isDistantCell()` reads.
Without a current worldspace Stage 0 still clears the reflection and still runs ripple
simulation if `DYNAMIC_RIPPLES` is set; everything else is skipped. The shadow-map early pass
additionally requires `USE_SHADOWS`, `CellHasWeather()`, and non-menu mode, and distant land
and statics are skipped entirely while `IsUnderwater(eyePos.z)`. Distant statics also need
`staticsUploaded` and `USE_DISTANT_STATICS`. The numbered steps below are the enabled path:

1. `selectDistantCell`. On cell change, it rescans dynamic-vis groups and asks the host to
   switch worldspace (`setWorldSpaceBlocking`; exteriors use the empty name, interiors
   their cell name). With no worldspace, MGE skips distant rendering this frame but still
   clears reflection and simulates ripples.
2. Captures Morrowind's view/projection (`mwView`/`mwProj`), derives eye position/vector
   and sun position (`setView`), recomputes fog ranges and colour (`adjustFog`, §8), and
   uploads the per-frame shared parameters (`setupCommonEffect`).
3. Shadow map early pass (`renderShadowMap`, exterior weather cells only). Builds the next
   shadow atlas one cascade per frame: distant terrain plus host-culled distant statics,
   depth-only through `XE Shadowmap.fx` into one of two depth atlases, which receivers
   cross-fade between. Restores state. See [shadows.md](shadows.md).
4. Distant geometry. Draws with a projection that pushes the near/far planes out
   (`editProjectionZ(kDistantNearPlane − ε, DrawDist·8192)`) so it always lands behind
   anything Morrowind draws:
   - Terrain (`renderDistantLand`, exteriors): queries the host with `VIS_LAND`; binds the
     terrain vertex declaration, atlas/material/blend-pattern textures, and
     `TerrainRuntimeConstants` (see [distantland-data.md](distantland-data.md)).
   - Distant statics (`cullDistantStatics` + `renderDistantStatics`): three host queries:
     `VIS_NEAR`/`VIS_FAR`/`VIS_VERY_FAR` bands with per-band far planes from
     `Configuration.DL.*StaticEnd`, all starting at the radial handoff
     `nearViewRange − 768`; the server sorts results `ByState`. MGE renders statics with
     alpha-to-coverage where the vendor supports it (`VendorSpecificRendering`); statics
     alpha-dissolve as they cross into Morrowind's range. Interiors with distant land
     (e.g. Mournhold) use a clip plane to avoid overdraw mismatches.
5. Sky. With atmospheric scattering, `renderSky` redraws the recorded sky (§2).
6. Water reflection (`renderWaterReflection`). It mirrors sky (+ optionally terrain and
   near/distant statics, per `REFLECT_*` flags) about the water plane into `texReflection`,
   with optional blur.
7. Ripple simulation (`simulateDynamicWaves`, `DYNAMIC_RIPPLES` only). GPU sim steps
   `texRain`/`texRipples`/`texRippleBuffer` for the water shader.
8. Saves the distant-only frame (`texDistantBlend = PostShaders::borrowBuffer(1)`) for
   StageBlend, unless MW/MGE blending is off.

## 4. Stage 1, StageBlend, Stage 2, StageWater

Stage 1 (`EndScene` of scene 0):

Stage 1 does nothing for a cached frame. Grass culling requires `isDistantCell()` and
`staticsUploaded`; the grass draw additionally requires `USE_GRASS`; the shadow overlay
requires `isDistantCell()`, `USE_SHADOWS`, and `CellHasWeather()`. Depth capture is
unconditional.

- `cullGrass`: host query `VIS_GRASS` (frustum-limited to the grass distance), then
  `buildGrassInstanceVB` batches transforms into an instance VB (`GrassInstStride` 48,
  up to `MaxGrassElements` 8192 per batch).
- Grass draw (`renderGrassInst`, `PASS_RENDERGRASSINST`): hardware instancing, wind sway
  (smoothed wind vector), shadow receiving, alpha-to-coverage.
- Shadow overlay (`renderShadow`, `PASS_RENDERSHADOW`/`PASS_RENDERSHADOWFFE`): re-draws
  recorded z-writing geometry and projects the depth atlas onto it per pixel with blending.
- Depth texture (`captureNativeDepth` when enabled and supported, otherwise
  `renderDepth`): writes `texDepthFrame` and its auxiliary depth surface from the active
  main DSV. Without MSAA, Morrowind renders directly into sampleable INTZ; with MSAA,
  the custom DXVK fork first resolves the nearest sample into INTZ. Unsafe projections,
  unsupported renderers/formats, or capture failures use the recorded-geometry replay.
  See [native-depth-capture.md](native-depth-capture.md).

StageBlend (immediately after Stage 1) returns immediately for a cached frame:

- Caustics (`PASS_RENDERCAUSTICS`, exteriors with `WaterCaustics > 0`): screen-space pass
  combining the frame, the water volume texture, and depth.
- MW/MGE blend (`PASS_BLENDMGE`, requires `isDistantCell()` and a clear `NO_MW_MGE_BLEND`):
  blends the saved distant-only frame back over
  Morrowind's near scene by depth/fog distance, hiding the handoff seam.

Stage 2 (`EndScene` of scenes 1+ until the frame is complete) uses the same shadow overlay +
depth merge for geometry Morrowind draws in later scenes (post-stencil redraw, sorted
alpha, 1st person), under the same `isDistantCell()` + `USE_SHADOWS` + `CellHasWeather()` gate
as Stage 1. A safe native frame merges nearer depth from the active DSV; otherwise
`renderDepthAdditional` replays the recorded geometry. One Stage-2 fallback keeps all
remaining Stage-2 invocations on replay for that frame. MGE skips it with no recorded draws.

StageWater replaces Morrowind's water-grid draw (or runs at a later non-UI, non-stencil
EndScene if the water never appeared, e.g. it is beyond Morrowind's draw range while
`USE_DISTANT_LAND` or `USE_DISTANT_WATER` is on).
Draws MGE's water plane (`vbWater`/`ibWater`, radial grid) with
`PASS_RENDERWATER` / `PASS_RENDERUNDERWATER`: reflection texture, ripples, waves,
depth-based shore fade.
Interiors/underwater set a clip plane at the interior fog end to save fillrate.

## 5. Post-processing and frame completion

At `BeginScene` of the first UI scene (`isFrameComplete` not yet set):

- `DistantLand::postProcess` runs the post-shader chain (`PostShaders::shaderTime`) when
  `USE_HW_SHADER` is on. In priority order, the `updatePostShader` callback updates each
  enabled shader's standard variable set (`EV_*`) plus environment flags
  (interior/exterior, underwater, sun visibility). The chain ping-pongs between
  double-buffered render targets. HDR adaptation reads back a downsampled luminance asynchronously.
- An enabled shader declaring `pointlightcount` requests a frame-local point-light list.
  Lit main-world draws reaching `inspectIndexedPrimitive` contribute their active point
  lights. MGE clears observations after `Present` and freezes them immediately before
  `shaderTime`. `pointlightpos[32]` stores world XYZ plus effective radius in W, and
  `pointlightcol[32]` stores decoded diffuse RGB with zero W. `pointlightcount` is
  authoritative. Radius solves the decoded attenuation at a `1/6` cutoff and
  caps at 4096 world units. Above 32 valid lights, MGE selects deterministically by distance
  to each influence sphere, then light ID/signature, with no history or unused-slot clearing.
- **Menu caching.** In menu mode (`USE_MENU_CACHING`), MGE caches the finished frame and
  re-blits it on subsequent menu frames instead of re-rendering the world. The cache
  expires on mouse click or leaving the menu.
- MGE captures the pre-UI screenshot and draws the MGE user HUD (`MGEhud`) before
  Morrowind's UI scene content; `EndScene` then runs the status overlay and post-UI
  screenshot capture.

`Present` then resets per-frame state, runs game-state-driven controllers (crosshair
autohide, zoom/shake, main-menu video), and, when armed, ticks the distant-land upload
pump (8 ms budget per frame). The pump is only the deferred arm of the init path: `init`
creates device resources first, and an already-resolved player cell uploads synchronously
instead. Rendering turns on only at `InitState::RenderReady` (`canRenderDistantLand()`),
after upload completes and the save's world data resolves, while `hasDeviceResources()` also
covers the intermediate `DeviceResourcesReady` state so cleanup paths still run. Full
treatment: [distantland-lifecycle.md](distantland-lifecycle.md).

## 6. Shadows

Full treatment: [shadows.md](shadows.md).

- `renderShadowLayer` fits one cascade per frame (`smView/smProj`, `smViewproj[2][4]`) as
  an eye-centred light box on the sky light's basis, with the translation row snapped to
  texels. Four cascades sit side by side in one `4*res` by `res` D24S8 atlas, separated by
  viewport. Two such atlases exist; once the next one is complete, receivers cross-fade to
  it over a quarter of a second and the roles swap.
- Casters: distant terrain and host-culled distant statics only, depth-only through
  `XE Shadowmap.fx` (`effectShadow`) against a NULL colour target. Statics cast only within
  `distant_land.shadows.static_range` of the camera. Recorded Morrowind geometry does not
  cast.
- Receivers: Stage 1/2 re-draw recorded geometry and project the atlas per pixel
  (`PASS_RENDERSHADOW`, or `PASS_RENDERSHADOWFFE` matching the per-pixel-lighting model),
  sampling through DXVK's comparison sampler. Grass, distant terrain and distant statics
  sample it in their own passes under `shadowDistant`. There is no depth-buffer shadowing of
  Morrowind's own rendering.

## 7. Fixed-function emulation (FFE)

`d3d8/cpp/mge/ffeshader.cpp`. Active when `USE_FFESHADER` and the per-pixel-lighting setting
applies to the current cell type. The FFE path maps each suppressed fixed-function draw to
a `ShaderKey`, a packed encoding of UV-set count, skinning, vertex colour, material source,
fog mode, and the colour/alpha ops of each active texture stage (incl. bump-env and
texgen). It caches keys; `generateMWShader` compiles
`XE FixedFuncEmu.fx` permutations on miss; a one-entry LRU avoids re-Begin on repeat keys;
a magenta fallback shader marks failures. The shader implements per-pixel sun + point
lights (`LightState` capture from `SetLight`/`LightEnable`) with the configured
sun/ambient weather multipliers. It replaces Morrowind's vertex lighting.

No device readback. `renderMorrowind` uses the shadowed state in §2 for the effect's
texture bindings, texgen matrix, and ambient-white check, not
`GetTexture`/`GetTransform`/`GetRenderState`. This is safe because the proxy alone writes
those states on the real device. `ProxyDevice::SetTexture`/`SetTransform` are the sole
translation points; the D3D8 state-block entry points are `UnusedFunction()` stubs, so
Morrowind cannot restore state behind the proxy. MGE wraps every owned texture-rebinding
pass in `CreateStateBlock(D3DSBT_ALL)`/`Apply()` (§9). Any unwrapped device-side
`SetTexture`/`SetTransform` breaks this invariant. In-game validation
covered 4.5 M draws / 36 M stage-slot comparisons with zero divergence. That run did *not*
exercise the texgen path because mods removed enchanted-item effects from the test install.

## 8. Fog and atmosphere

`DistantLand::adjustFog` (per frame, Stage 0):

- Fog ranges come from MGE config (`Configuration.DL.*FogStart/End`). The function modulates
  them by weather (`FogD`/`FgOD` distance/offset tables, with interpolation across weather
  transitions) and environment (underwater, interior density), then clamps the end so it
  never falls inside vanilla range. It also interpolates wind scaling and per-weather
  sun/ambient multipliers.
- **Exponential fog.** With `EXP_FOG`, shaders use `fogExpStart/Divisor`; MGE linearly
  approximates the exp curve for Morrowind's near range at 1280 units and the view range,
  so near (Morrowind-rendered) and far (MGE-rendered) fog match.
- **Atmospheric scattering.** In nice weather with `USE_ATM_SCATTER`, the function replaces
  Morrowind's fog colour with an inscatter approximation of the shader's scattering model,
  evaluates it at the near-fog boundary along the view azimuth, and writes it back through
  the scenegraph (`setScenegraphFogCol`) so Morrowind restores it on mid-frame fog-mode
  switches. Scripts set scattering coefficients via `weatherScatteringSet`.
- MGE intercepts and ignores Morrowind's own `D3DRS_FOGSTART/END/VERTEXMODE/TABLEMODE`
  sets while MGE owns fog.

## 9. Render-state hygiene rules

Patterns to preserve when touching this code:

- Every stage that changes FVF/vertex declarations snapshots device state with
  `CreateStateBlock(D3DSBT_ALL)` and re-applies it before returning. Morrowind assumes its
  state survives across its own draw calls. `RenderTargetSwitcher` (RAII) is narrower — it
  saves and restores only the render target and depth-stencil surface, not a state block.
- MGE begins effects with `D3DXFX_DONOTSAVESTATE` and performs explicit manual state
  restoration.
- `recordMW`/`recordSky` hold AddRef'd buffer references (`RecordedState`); MGE clears them
  every stage/frame to release them.
- The projection trick. MGE does not depth-composite distant geometry against Morrowind's
  scene. It draws it first with a biased far-plane projection; Morrowind draws over it and
  StageBlend smooths the seam. The depth texture (`texDepthFrame`) is the
  only unified depth representation; if you add a feature that needs scene depth, append
  to it in the appropriate stage rather than reading the device z-buffer.

## 10. Camera-relative rendering

`render.camera_relative`, off by default while it is being tested. Owned by
`d3d8/cpp/mge/camerarelative.{h,cpp}`.

**Problem.** Far from the world origin the game's float32 world coordinates are large enough
that arithmetic on them rounds by a visible amount: one float step is 0.03 units at 32 cells
out and 0.06 units at 64. The engine hands the proxy an exact world matrix per draw
(`NiDX8Renderer::SetModelTransform` copies the translation verbatim) but a view matrix whose
translation it already rounded (`SetCameraData` dots a world-magnitude location with the
camera basis in float), and `captureTransform` multiplied the two with D3DX in float. The
rounding differs per object and per camera rotation, which shows as shimmering objects,
cracks along landscape patch edges, and a whole-scene shift when the view turns.

**Mechanism.**

- A vtable hook on `NiDX8Renderer::SetCameraData` (slot `0x74F588`, verified before it is
  written) records the exact camera location and basis before the engine builds its view.
- `SetTransform(D3DTS_VIEW)` for a main-view scene activates the space when the incoming
  rotation matches the recorded pose bitwise. While active, the recorder view
  (`rs.viewTransform`) and the real device's view are rotation-only; every world matrix has the
  camera position subtracted in double before it is captured (`rs.worldViewTransforms`, the
  indexed-skinning palette, the DXVK PPL packet) and before it reaches the real device; point
  lights get the same offset in `SetLight` and in the FFE light transform.
- `rs.worldTransforms` stays absolute. The sky and water passes replay it against `mwView`,
  which `renderStage0` now takes from `CameraRelative::absoluteView()` (the pose-derived
  absolute view with camera effects applied) instead of reading the device.
- View space is unchanged geometrically: the camera sits at its origin either way. Depth,
  shadow receivers, fog and lighting consume the same values as before, only more precise.

**Actors and the first-person view.** The engine rounds every node's world translation in
its own scene-graph update, and composes the skin palette in float at world magnitude, before
any of the above runs, so the matrices it hands the proxy already carry that error for
anything that moves. The hooks recover the exact position instead: a node's world translation
is the sum, down its parent chain, of each local translation rotated and scaled by the parent's
stored world rotation and scale, all exact inputs, summed in double. Vtable hooks on
`RenderShape` and `RenderTriStrips` record which node is being drawn (both `Display` paths pass
`&geometry->worldTransform`); the five `SetModelTransform` call sites place rigid draws from
that exact position; the three `SetSkinnedModelTransforms` call sites replace the engine version and
upload each bone matrix once, composed in double from exact bone, root-parent and shape
positions (the two model-space camera axes it also set are replicated); and the `SetCameraData` hook derives the camera's own exact position the same way.
In first person the engine copies the stored world position of the `Camera` node of the
first-person model into the world, arm and shadow camera roots each frame
(`PlayerAnimController::updateCameraTransforms`); the five call sites of that function are
patched so the copy is paired with the exact position of the node at that instant and with the
roots it went into. A `SetCameraData` location is turned back into its `NiCamera` (the recovered
object must carry the NiCamera vtable before anything else is read), and a camera hanging from
one of those roots gets the exact position of the pair plus the float offset between its location
and the copy, so the arms and the eye share one set of inputs; any other camera uses its own
parent chain. Exact positions
are memoized per frame in a fixed table keyed by node pointer, so a skeleton is walked once
however many body parts hang from it. A node whose recomposed position disagrees with its
stored one by more than the engine's plausible rounding (sixteen float steps at its magnitude,
at least one unit) had its transform written directly and keeps the stored value. Every
recovered pointer read is guarded, so a wrong caller falls back instead of faulting.

What this cannot recover is precision lost before a value reaches the scene graph. A MWSE mod that
writes the camera or the first-person model position from Lua (head bobbing, camera noise, body
inertia) stores a float at world magnitude, so far from the origin its offset lands on the float grid
(0.125 units at 135 cells) and the exact eye or arms step with it; stock rendering hides the same
steps under its own jitter. Such mods need their own distance cutoffs, or an exact offset passed
through MWSE separately from the float position.

**Measuring.** `render.camera_relative_probe` compares, for every main-scene draw, the
world-view translation that reaches the shader against a double-precision reference built from
the exact pose, and logs the maximum and mean error in world units and pixels every 300
frames. It works with the feature on or off, which makes it the before/after measurement.
