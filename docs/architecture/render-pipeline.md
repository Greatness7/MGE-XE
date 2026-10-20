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
additionally requires `USE_SHADOWS`, `IntLikeExterior(true)`, and non-menu mode, and distant land
and statics are skipped entirely while `IsUnderwater(eyePos.z)`. Distant statics also need
`staticsUploaded` and `USE_DISTANT_STATICS`. The numbered steps below are the enabled path:

1. `selectDistantCell`. On cell change, it rescans dynamic-vis groups and asks the host to
   switch worldspace (`setWorldSpaceBlocking`; exteriors use the empty name, interiors
   their cell name). With no worldspace, MGE skips distant rendering this frame but still
   clears reflection and simulates ripples.
2. Captures Morrowind's view/projection (`mwView`/`mwProj`), derives eye position/vector
   and sun position (`setView`), recomputes fog ranges and colour (`adjustFog`, §8), and
   uploads the per-frame shared parameters (`setupCommonEffect`).
3. Shadow map early pass (`renderShadowMap`, exterior weather cells only). Renders two
   cascades side by side into one shadow atlas, each with distant terrain plus host-culled
   distant statics through `XE Shadowmap.fx`, then blurs the atlas. Restores state.
   See [shadows.md](shadows.md).
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
requires `isDistantCell()`, `USE_SHADOWS`, and `IntLikeExterior(true)`. Depth capture is
unconditional.

- `cullGrass`: host query `VIS_GRASS` (frustum-limited to the grass distance), then
  `buildGrassInstanceVB` batches transforms into an instance VB (`GrassInstStride` 48,
  up to `MaxGrassElements` 8192 per batch).
- Grass draw (`renderGrassInst`, `PASS_RENDERGRASSINST`): hardware instancing, wind sway
  (smoothed wind vector), shadow receiving, alpha-to-coverage. Interiors are distant cells
  too when the generator baked grass for them (interior world spaces in `usage.data`, selected
  by cell name). Without weather the smoothed wind targets the constant
  `distant_land.grass.interior_wind` rather than the engine's stale exterior wind, and the
  shadow cascades are bound as matrices that map every receiver outside the atlas, since the
  atlas is only rendered for cells with weather.
- Shadow overlay (`renderShadow`, `PASS_RENDERSHADOW`/`PASS_RENDERSHADOWFFE`): re-draws
  recorded z-writing geometry and projects the soft shadow map onto it with blending.
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
alpha, 1st person), under the same `isDistantCell()` + `USE_SHADOWS` + `IntLikeExterior(true)` gate
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

Water volumes are bodies of water that a mod places apart from the cell's water. Their
surfaces are ordinary game meshes whose material carries a marker in the specular power:
99998 (reflects the sky and what is on screen) or 99997 (reflects the sky only). `SetMaterial`
recognises the marker, `DrawIndexedPrimitive` holds the draw (`renderWaterVolume`), and
`flushWaterVolumes` draws all held surfaces after StageBlend and after each Stage 2, from one
copy of the frame, with `PASS_RENDERWATERVOLUME`, whose pixel shader is the standard water
shader (`XE Mod Water Standard.fx`); the marker reaches it as a value of the look. The
shading reuses the water plane's ripples, fog and specular. It follows the normal of the mesh
(`WaterVolumeVS`): the ripples are tilted to it, and the reflection and the Fresnel term are
taken from it, so a surface can face any way. A mesh without normals faces up. The planar
reflection is replaced by the analytic sky colour plus an optional screen-space march. The
march reflects only what is on screen: the reflection of a thing ends where the thing leaves
the view. Along a ray the march knows three cases: the ray is in the open, it met a
surface (it is a little behind what is on screen there), or a thing nearer to the eye hides
it (it is far behind). A hidden ray goes on, to what it meets when it is in the open again;
it does not stop at the thing that hides it, which would leave the water beside a post, an
arch or the end of a boat without the reflection of what is behind them. The depth of the
scene is read for the ray as it is (`sampDepthPoint`), not smoothed: between a thing and what
is behind it a smoothed depth is that of a surface that is not there, and the reflection
would sparkle along the edge of the thing. The
emissive colour of the marked material is the colour of the water (`waterVolumeTint`, set for
each held draw); black is the usual colour. A distant water subset has that colour in its
vertex colour, written by the generator.

The water of the cell has a colour of its own, by the same rule: the emissive colour of the
game's water material (the one marked with 99999), read in `SetMaterial` into `waterPlaneTint`
and used by `WaterPS`. A mod sets it from Lua on the material of the game's water node. The
two colours are apart: each surface has its own material.

The first flush of a frame also draws the distant statics whose subsets carry the water flag
(`RenderMesh::water`), with `PASS_RENDERWATERVOLUME_DISTANT`: the distant vertex shader
gives the pixel shader the same vertex as a surface near the player, so the standard water
shader, or the one of a mod, draws both. A distant subset has the look of its mesh: the
generator copies the look line (`wv:` string data) into the subset record, the loader reads
it with `WaterLooks::parse` and gives each different look an index, and the subset carries
`WaterLooks::firstDistantLook` plus that index in its water byte, which the host passes
through. `VisibleSet` leaves those meshes out of every other
pass (colour, depth, shadow, reflection). `shadeWaterVolume` clips them nearer than
`waterVolumeHandoff`, where the game draws the real mesh. `flushWaterVolumes` sets that
depth for each mesh: `nearViewRange` for a mesh whose reference is in one of the loaded cells
(the cell of the player and the eight around it), and zero for any other mesh, because the
game has no mesh to draw in its place. The flush draws the surfaces near the player first and
the distant water after them: the game draws a mesh in full when a part of it is nearer than
its view distance, and there the depth test leaves the distant mesh out. The screen-space march fades out at
`distant_land.water.volume_reflection_cells`; a value of zero turns the march off for the
surfaces near the camera as well.

`renderStageWater` leaves the water plane of the cell out, with its copy of the frame, when
the plane is more than twice the view distance below the camera. A mod that needs the water
flag on a dry interior puts the level that far down.

A surface can have a look beyond its colour: the drift, speed and size of its ripples,
glow, opacity, clarity (over how many units of water what is under the surface fades
into the colour of deep water), whether the vertex colour of the mesh tints it, and a colour that it reflects
in place of the sky (`WaterLook` in `waterlook.h`). Without that colour a surface reflects
the sky outdoors and the light of the room in an interior, as `clearReflection` takes it for
the water of the cell.

A held surface keeps the stencil test and the depth test of its mesh (`NiStencilProperty`,
`NiZBufferProperty`): `renderWaterVolume` reads them from the device and the flush sets them
again for the draw, after the states of the pass. So a mesh can show its water only through
a mask that another of its shapes wrote into the stencil buffer, and at any depth: the water
in a well, under the ground. The shadow scenes that use the stencil buffer come later. A
surface without the depth test tells the shader so (`look.noDepthTest`): the depth of the
scene is then that of the ground over the water, and the water is taken as deep. Such a
surface is not held: `renderWaterVolume` draws it at once, in its place among the draws of
its mesh, so that what the mesh draws after it covers it, as the wall of a well covers the
rim of its water.

### Light on water volumes

Three options, each a setting that is off by default and a define of the main effect
(`WATER_VOLUME_SHADOWS`, `WATER_VOLUME_LIGHTS`, `WATER_VOLUME_CAUSTICS`, set in `initShader`),
so that the water shading has no code for one that is off. All three are in
`shadeWaterVolumeRippled`, and so in every water shader that calls the standard shading.

- Shadows: `flushWaterVolumes` binds the shadow atlas to the texture place of the planar
  reflection, which the volume passes do not use, and sets the cascade matrices (or matrices
  that map every place outside the atlas, in a cell without weather or with shadows off).
  `volumeSunlight` gives the share of the sun at the surface point; it scales the glint of
  the sun and darkens the depth colour a little. The result is in `WaterShade.sun`.
- Point lights: when the game submits the surface, `renderWaterVolume` copies up to four of
  the point lights that are on for that draw (`takePointLights`, from the proxy's
  `LightState`, decoded as the fixed-function replacement decodes them), and the flush sets
  them for the draw. The shading adds the mirror image of each light in the ripples, in the
  hue of the light, with the game's falloff. Distant water has no lights.
- Caustics: the formula of `XE Mod Caustics.fx`, applied to the frame behind the surface
  where the bed is read, for surfaces that face up, scaled by the sun at the point. The
  strength is that of the water of the cell, and zero in a cell without weather.

### Dry spaces

A mod reports closed meshes inside which there is no water (`MGE_WaterMasksSet`, the
triangles in the world; with True Water a shape named `WaterMask`), and whether the camera
is in one (`MGE_WaterDrySet`; `IsUnderwater` is then false). Water is not drawn inside them.
Whether a pixel of a water surface is inside is counted in one bit of the stencil buffer:
a quad over the screen sets the bit to zero, every face of the dry spaces turns it over, and
after the draw the bit is set to zero again (`countDrySpaces`, `endDrySpaceCount`).

The mod knows only the meshes that the game has loaded. For the others the distant land
carries the dry spaces. The generator keeps each shape with a mask name as a subset of its
distant static that is a dry space (`SubsetWater::DrySpace`, flag bit 4 of the subset record;
see `distantland/docs/architecture/statics.md`). The loader gives such a subset the water
byte `WaterLooks::drySpace`, so the host puts it in the tree of the distant water and no
ordinary pass gets it. `cullDistantStatics` fills `visWaterShared` once a frame, in
`renderStage0`, and `findDistantWaterInView` then says whether a dry space is in it
(`distantDrySpacesInView`). `countDrySpaces` draws those subsets after the triangles of the
mod (`countDistantDrySpaces`), into the same bit: with the pass of the distant water
(`PASS_RENDERWATERVOLUME_DISTANT`), the vertices of a distant static, the matrix of each mesh
and the projection of the distant land. The wave height, the handoff depth and the plain mix
that `beginDrySpacePass` sets hold for that pass too.

A boat near the player is in both places: the mod reports its dry space, and its distant
static can still be in the tree. Two counts of the same faces leave the bit as it was. So a
distant dry space is counted only when its reference lies outside the loaded cells, the cell
of the player and the eight around it (`LoadedCells`, the same rule that sets
`waterVolumeHandoff` for distant water). In an interior, under water, in a cached frame and
without distant statics there are no distant dry spaces.

- Held water volume surfaces and distant water (`flushWaterVolumes`): the surfaces write
  their depth, and the faces behind that depth are counted. An odd count is inside, wherever
  the eye is. Distant water takes part only when the flush draws it.
- The water plane of the cell (`renderStageWater`): the faces on the far side of the level of
  the water are counted, with a clip plane and without the depth. An odd count is inside,
  wherever the eye is. With the eye in a dry space and within `kDrySpaceLevelMargin` of the
  level, the plane is left out: it is edge-on, and its waves put it now over the eye and now
  under it.
- The caustics (`renderStageBlend`): the faces in front of the depth of the scene are
  counted. An odd count is inside when the eye is outside, and the other way round when the
  eye is in a dry space. The faces behind are not used here: the floor of a boat lies in a
  face of its dry space, and which side of it the depth falls on is not sure.

Known limits: the count of the faces in front misses a face that the near plane cuts, so
for a few frames, as the eye passes a face of a dry space, the caustics can show inside it.
The counts of the faces behind miss a face beyond the far plane of the game's view, at the
end of the view distance. The depth that a cut surface wrote stays in the depth buffer.

### Stencil bits

The count sets its bit to zero itself and leaves the other bits alone, so it does not need
a free bit; it needs one that no mesh tests later in the frame. What is in the buffer, found
by test in the game on 2026-10-07 (each bit in turn used for a count that did not yet set it
to zero, looking at the body of the player outdoors), and from meshes of mods:

| Bits | Who sets them | How it is known |
| --- | --- | --- |
| `0x01`, `0x04`, `0x08`, `0x40` (together `0x4d`) | The game, on the pixels of the player's body outdoors | With each of these four the water was drawn over the body; with each of the other four it was not. The values themselves were not read back |
| a count, 0 to 255 | The game's shadow scenes, which `MGEProxyDevice` knows by `stencilRef <= 1` | The game's code, see below |
| `0xf4` (low byte of the reference 500) | Meshes of mods that draw through a mask: a shape writes 500, other shapes test for it, with all bits. The wells of "Water In Wells", the shield of "Darksun's Eclipse" | The meshes |
| `0x02` | The dry space count (`kDrySpaceBit`) | Not a bit of 500, so the count does not change what those meshes test |

What the game does with the stencil buffer, from its code (Morrowind.exe, addresses of the
English 1.6.1820 build):

- The only thing that writes it is the shadow manager (`init` at 0x434260, `render` at
  0x4352A0, called from the main scene render at 0x41C400 after the world and before the
  water). For each light that casts shadows it clears the stencil buffer, draws the shadow
  volumes twice, the faces of one winding with "add one where the depth test fails" and the
  faces of the other with "take one away where the depth test fails", and then draws a dark
  quad over the screen where the value is not zero. Reference 0, mask all bits. The renderer
  maps the two actions to the saturating ones (table in the render state at +0x178:
  keep, zero, replace, add, take away, invert), so the value is a count from 0 to 255: in how
  many shadow volumes the visible point lies.
- The game then draws the shadow casters again, without a stencil test, which hides the dark
  quad on them. So the body of an actor keeps the count of its own shadow volumes in the
  buffer. That is the value on the body of the player.
- Nothing reads the value after the dark quad. The buffer is cleared with the frame, before
  each light, and before the first person scene.
- The other stencil properties that the game makes (the water node, mirrored body parts, the
  collision bounds view) are not enabled: they only set which side of a face is drawn.

So no bit is free by rule: a count of 2 or 3 sets bit `0x02`. That the test found it clear
on the body, with `0x40` set, is not explained; it holds for the scene of the test. For that
reason the dry space count sets its bit to zero first. It does not clear the whole buffer:
that would take the mask from a mesh of a mod that tests for it later in the frame.

A mesh that writes a stencil reference with bit `0x02` set and tests for it after the water
is drawn loses that bit where a dry space is in the scene. A surface with a stencil test of
its own is not cut by dry spaces; while the faces of a dry space are counted in front of it,
between the two zero passes, its test sees the bit, so such a surface behind a dry space is
not drawn. A mod reports looks through the `MGE_WaterLookSet` export, one per slot, and
marks a surface with its slot in the specular power of the material, `100000 + slot`; the two
old markers stay valid. `SetMaterial` reads the slot, the held draw carries it, and
`flushWaterVolumes` sets `waterVolumeFlow` and `waterVolumeMix` for each draw from the slot's
look. The shader (`WaterSurfaceLook`) drifts the ripple coordinates by the flow turned into
the world, scales the ripple time by the speed, and mixes glow and the frame behind the
surface by the opacity. A slot stands for a look, so every surface with the same look shares
it.

A look can name a water shader of a mod, `Data Files\shaders\water\<name>.fx`. When the main
effect is compiled, `findWaterShaders` lists the files, and `CoreModInclude` writes an include
of each into `XE Water Shaders.fx` and a pass for each into `XE Water Shader Passes.fx`; both
files are empty on disk. The passes come after the last fixed pass
(`PASS_WATERSHADER_FIRST`). `flushWaterVolumes` asks `waterShaderPass` for the pass of each
held draw and gives it the free values of the look and the base texture of the mesh. When the
effect does not compile with the water shaders, it is compiled without them and each failing
file is named in the log. The contract is in `docs/water-shaders.md`.

A mod can also tell MGE which volume the camera is in, through the `MGE_WaterVolumesSet`
export: while the camera is inside one, `CellHasWater()` and `WaterLevel()` describe that
volume, which gives the underwater view and fog at the right height.

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

- `renderShadowLayer` fits two cascade layers (`smView/smProj/smViewproj[2]`) per frame
  around the camera and computes a sun-aligned ortho projection per layer radius. The two
  cascades sit side by side in one `2*res` by `res` R16F atlas, separated by viewport.
- Casters: distant terrain and host-culled distant statics only, drawn through
  `XE Shadowmap.fx` (`effectShadow`) into `texSoftShadow`, then blurred via `texShadow`
  and back into `texSoftShadow`. Recorded Morrowind geometry does not cast.
- Receivers: Stage 1/2 re-draw recorded geometry and sample the shadow map with blending
  (`PASS_RENDERSHADOW`, or `PASS_RENDERSHADOWFFE` matching the
  per-pixel-lighting model). Grass samples the map in its own pass; distant statics and
  terrain do not. There is no depth-buffer shadowing of Morrowind's own rendering.

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

`render.camera_relative`, on by default. Owned by `d3d8/cpp/mge/camerarelative.{h,cpp}`,
whose header comment describes the mechanism; this section records what the rest of the
pipeline relies on.

- **Installation.** The engine hooks install at device creation only when the option is on.
  Turning it on at runtime takes effect after a restart; turning it off applies from the next
  scene. A hook group (camera pose, rigid draws, skinned draws, first-person eye) that cannot
  claim all of its sites restores the ones it took and reports itself uninstalled, so a conflict
  with another patcher does not leave a group half-owned. The rollback writes are themselves
  unchecked, which matters only if `VirtualProtect` refuses a page the module already wrote.
- **Which scenes.** The `SetCameraData` hook recovers the `NiCamera` being clicked and
  activates only when it is `WorldController::worldCamera` or `armCamera` (the pointers at
  +0x134 and +0x160) and the render target is the back buffer. The menu, splash, shadow,
  reflection and any mod-created camera render scenes that stay absolute, and so does the
  view `NiDX8Renderer::RenderScreenPoly` sets around screen polygons — a constant identity
  written once into `screenPolyView` by `NiDX8Renderer::init`, never derived from camera data,
  and matching no camera pose. Which scene a pose belongs to is decided by camera identity,
  never by matrix shape; the rotation comparison against the standing pose is a second gate
  that only says whether this view is the one that pose produced. `detectMenu` stays for MGE's
  own main-view bookkeeping only.
- **Space convention** while the world or first-person scene is active: the real device and the FFE/PPL
  shaders see world matrices whose translation is `world - camera` and a rotation-only view.
  `rs.worldTransforms` stays absolute for the sky and water replays, and `renderStage0` takes
  `mwView` from `CameraRelative::absoluteView()` instead of the device. View space is unchanged,
  so depth, shadow receivers, fog and lighting consume the same values as before.
- **Lights.** The engine uploads a light to the device only when the light's own revision
  changes (`NiDX8LightManager::LightEntry::Update`) and otherwise just re-enables it as the
  enabled set churns per object, so a point light rebased once in `SetLight` would keep a dead
  origin as the camera moves. The proxy records every light in absolute space and uploads it
  again, at each view and at each `LightEnable`, when the space or origin it was uploaded under
  no longer matches. Records are capped at 1024, evicting the least recently uploaded or
  enabled light; the lights of an unloaded cell are never enabled again, so the cap only
  matters if more than that many are hot at once. This is the general hazard of the feature:
  any engine-side redundancy cache over a value that is now camera-dependent becomes a per-frame staleness bug. Lights are
  the only known instance; fixed-function clip planes would be another, but Morrowind never
  sets one.
- **Limits.** Precision lost before a value reaches the scene graph cannot be recovered. A
  MWSE mod that writes the camera or the first-person model position from Lua (head bobbing,
  camera noise, body inertia) stores a float at world magnitude, so far out its offset lands on
  the float grid (0.125 units at 135 cells) and the exact eye or arms step with it. Such mods
  need their own distance cutoffs, or an exact offset passed through MWSE separately.
