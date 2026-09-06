# Sun shadows

MGE XE casts sun shadows from distant-land geometry onto Morrowind's own geometry.
Four cascades, an orthographic light camera per cascade, and a hardware depth atlas
sampled with percentage-closer filtering.

Only distant terrain and distant statics write into the shadow map. Morrowind's near
geometry, MGE's grass, and the distant terrain and statics themselves read from it.
Nothing Morrowind draws itself casts a shadow, so actors, the player, doors, and
hand-placed clutter inside the near band produce no MGE shadow. Morrowind's own
stencil shadows still handle actors, controlled by `[General] High Detail Shadows` in
`Morrowind.ini`.

Casters are the distant-land copies of the same meshes the receivers are built from.
That is what most of the bias machinery below exists for: a coarse terrain mesh or a
decimated static sits a few units off its near twin, and a depth compare with no
tolerance reads that offset as shadow.

Code lives in `d3d8/cpp/mge/rendershadow.cpp`, with resource creation in `distantinit.cpp`
and gating in `distantland.cpp`.
Shaders are `XE Shadowmap.fx` (caster side) and `XE Mod Shadow.fx` (receiver side,
included by `XE Main.fx`). `XE Mod Shadow Data.fx` holds the constants both sides share
and is included by each of them directly. Two of those three files are user-replaceable,
see [Core mods](#core-mods).

## Resources

`DistantLand::initShadow` allocates everything once, at distant-land init:

| Resource | Type | Dimensions | Format |
| --- | --- | --- | --- |
| `texShadow[2]` | depth-stencil textures | `N * res` by `res` | `D3DFMT_D24S8` |
| `surfShadow[2]` | level 0 of each `texShadow` | same | bound as the caster depth-stencil |
| `surfShadowColor` | render target | same | `NULL` FourCC, or `R16F` if unsupported |
| `vbFullFrame` | vertex buffer | 4 verts, 12 bytes each | full-target quad |

`res` is `Configuration.DL.ShadowResolution` and `N` is `DistantLand::kShadowCascades` (4),
so each atlas is 4096x1024 or 8192x2048 for the two resolutions the config allows.

There are two atlases because shadows are cross-faded in time. `shadowCurrent` indexes the
one receivers sample; `shadowBuilding` indexes the one being rebuilt a cascade per frame.
When a rebuild completes, receivers blend from current to next over `shadowBlendSeconds`
(0.25 s of wall-clock time, `rendershadow.cpp`), then the roles swap and the next rebuild
starts. A rasterized silhouette can only move in whole texels as the sun turns, and near
the camera one cascade-0 texel is many screen pixels, so without the fade shadow edges
visibly hop; the fade turns every hop into a quarter-second dissolve regardless of frame
rate, and casters cost one cascade per frame instead of four. Shadows lag the sun by the
build plus fade time, under half a second, which is a few hundredths of a degree.

The atlas is a depth texture. D3D9 requires a colour target while rendering depth, so the
caster passes bind a `NULL`-format surface (`kFormatNull`), a driver hack DXVK honours that
allocates nothing. Casters also set `ColorWriteEnable = 0`, so the fallback `R16F` surface
costs bandwidth only in allocation.

Receivers sample `texShadow` through a linear sampler (`sampShadow` in `XE Common.fx`).
DXVK turns any sampler on a depth-format texture into a Vulkan comparison sampler
(`D3D9CommonTexture::DetermineShadowState`; INTZ and DF16/DF24 are excluded), so a single
`tex2Dlod` returns a 2x2 bilinear percentage-closer result against the z coordinate of the
texcoord, with `VK_COMPARE_OP_LESS_OR_EQUAL`: 1 where the reference depth is at or in front
of the stored depth. There is no way to read the raw depth back through D3D9, which the
[debug view](#debug-view) works around.

## Frame flow

Shadow work splits across three of the render stages described in
[render-pipeline.md](render-pipeline.md).

Stage 0, at the start of scene 0, builds the map. `renderStage0` first captures
`mwView`/`mwProj` from device state and derives `eyePos`, `eyeVec`, `sunPos`, and `sunVis`
in `setView`, then runs `renderShadowMap` under this gate:

```cpp
!isRenderCached && isDistantCell() && (Configuration.MGEFlags & USE_SHADOWS)
    && mwBridge->CellHasWeather() && !mwBridge->IsMenu()
```

`CellHasWeather()` restricts the whole feature to exterior weather cells. Interiors get no
MGE shadows at all.

Stage 1 (end of scene 0) and Stage 2 (end of scenes 1+) each call `renderShadow` to
project the finished map onto recorded geometry, under a gate that keeps the
`!isRenderCached`, `isDistantCell()`, `USE_SHADOWS`, and `CellHasWeather()` checks. These
stages do not repeat Stage 0's `!mwBridge->IsMenu()` check.

`RenderTargetSwitcher` restores the render-target and depth-stencil bindings. The broader
device state is restored by the state block around `renderStage0`, so Morrowind's state
survives the pass. Inside Stage 0, though, the distant-land passes that follow the map
set none of `ColorWriteEnable`, `DepthBias` or `SlopeScaleDepthBias`, so `renderShadowMap`
resets those itself after the caster pass. Without that reset distant land renders with
colour writes off and vanishes into fog outside menu mode.

## Building the map

`renderShadowMap` runs every Stage 0 and does this:

1. Compare the eye with where the current build started (during a fade, where the last one
   started). More than half the near radius away, `shadowRestartDistance` = 500 units,
   counts as a jump. Only a teleport, a load or a door moves the eye that far inside one
   build or one fade.
2. If the next atlas is complete, advance `shadowBlend` by wall-clock time. At 1.0 (or
   immediately if no atlas was ever valid, if the eye jumped, or if this build was restarted
   after a jump) swap `shadowCurrent` and `shadowBuilding` and start a new build.
3. If a build is in progress, first handle a jump: drop back to cascade 0 and flag the
   build so it lands without a fade. Then bind `surfShadowColor` and
   `surfShadow[shadowBuilding]` through `RenderTargetSwitcher` and clear the atlas
   parameters left by the last receiver pass. When starting cascade 0, clear depth (1.0 is
   the far plane, which every receiver compare reads as lit) and record the eye. Render the
   one cascade `shadowBuildLayer` with `renderShadowLayer`. After the last cascade, mark the
   build complete and stamp the fade start time. Restore the viewport and the render states
   above.
4. Upload `shadowCascade[]` (texel size and depth range per cascade of each atlas) and,
   through `uploadShadowMatrices`, both atlases' cascade matrices, both textures and
   `shadowBlend`, for the distant-land receivers that draw next.

After a jump the stale atlas stays on screen until the restarted one lands, four frames
later. It is world-locked, so it is still right wherever it covers the new position, and
empty elsewhere.

There is no blur. Softness comes from the receiver's filter kernel.

## Fitting a cascade

Cascade half-widths come from `shadowCascadeRadius(layer)` in `rendershadow.cpp`: 1000,
4000, 16000, and `Configuration.DL.DrawDist * kCellSize` for the last, which covers the
whole distant-land draw distance. The last is held at or above 1.25 times the third, so it
stays the outermost at draw distances under two cells. Each cascade covers a cylinder of that radius and the
same height (`shadowCascadeHeight`) around the eye. Toward the sun the light camera sits
`max(shadowCasterReach, halfZ)` out, with `shadowCasterReach` two cells, so a hill a couple
of cells away still casts into the near cascades at sunset; the depth range runs from there
to the far side of the box.

Light direction is `sunVec`, the D3D light 6 direction captured in `setSunLight`, day and
night. It is the engine's sky light, not the sun disc; see the measurement below. Older
versions used the disc (`sunPos`, from `MWBridge::GetSunDir`) while it was above the
horizon and switched to the light when `sunVis` reached zero.

The projection centre is the eye itself. Older versions pushed it a radius ahead along the
view direction to spend the atlas on what the camera sees; with the atlas reused for up to
half a second while the camera turns, anything view-dependent in the fit leaves the new
view uncovered until the next build fades in, so the fit depends on eye position only.

The light camera looks at the eye along `lightVec` with up `(0, 0, 1)`, which keeps
light-space y vertical as the receiver offsets assume. Within 26 degrees of the zenith
(`|lightVec.z| >= 0.9`) that up vector nears the light direction and the basis would spin
with every sun step, so world Y takes over there; the sun path never runs north-south. The
swap is a single grid rotation, which the atlas crossfade absorbs. The box is then sized
from the basis axes: along each axis the half-extent is `radius * horizontal + height *
vertical` (`shadowExtentAlong`), so the cylinder fits at any sun angle. The older
`(1 + |lightVec.z|) * radius` height covered only half a radius of relief at low sun and
dropped tall casters from the near cascades exactly when shadows are longest. The ortho
projection is `2 * halfX` wide and `2 * halfY` tall, near 0, far `zSun + halfZ`.

Before the fit, the light elevation is clamped to `shadowMinElevation` (10 degrees), keeping
the azimuth. Below that the constant receiver bias detaches shadows from their casters by
`bias / tan(elevation)` (274 units at 5 degrees), the two-cell caster reach no longer covers
the relief, and a ground texel stretches to `texel / sin(elevation)` along the light. The
receivers fade the shadow term out over `shadowElevationFade` (5 to 10 degrees of elevation
of the same `sunVec`), so the last degrees would dissolve instead of snapping.

Measured against the engine (`WeatherController::updateSun`, transit constants
`(-400, 75, -100)`), the light and the disc are not the same sun. The sky light behind D3D
light 6 points along `(f, 75, -100)`, with `f` sweeping -400 to 400 over the day and back
over the night. Its elevation is 13.8 degrees at sunrise and sunset and 53.1 at noon, never
lower. The sun disc sits at `(-f, -75, 400 - |f|)`, the same azimuth but 0 degrees at the
horizon and 79.2 at noon. The fit follows the light because every lambert term, MGE's
included, and the engine's stencil actor shadows do. Cast shadows then meet the shading
terminator and the actor shadows at every hour. The price is a noon shadow of a 53 degree
sun under a 79 degree disc. One vector for day and night also means nothing hops when the
disc fades out. With the vanilla light the clamp and the fade are dormant. They engage only
if a mod lowers the light.

The view-projection is then snapped to whole texels:

```cpp
const double quantizer = 2.0 / Configuration.DL.ShadowResolution;
viewproj->_41 = float(quantizer * std::floor(viewproj->_41 / quantizer));
viewproj->_42 = float(quantizer * std::floor(viewproj->_42 / quantizer));
```

The translation row is where the world origin lands in clip space. Rounding it to a texel
multiple locks the sampling grid to the world for a given light direction and cascade, so
neither camera translation nor rotation moves where texel centres fall. The grid only
drifts with the sun, through the elevation-dependent height above. Clip space spans
[-1, +1] over `res` texels, hence the factor of 2. z is not quantized because depth
precision does not alias the same way.

Each fit records its texel size and depth range in `shadowFit[atlas][layer]`.
`renderShadowMap` uploads both atlases' values every frame as `shadowCascade[set * 4 + layer]
= (texel size in world units, depth range in world units, 0, 0)`, set 0 for the current atlas
and set 1 for the next, like the matrices. The texel size is the horizontal one. With world Z
up the vertical texel is `sin e + cos e` times larger, up to 41 percent at 45 degrees. The
shaders use the values for the normal offset, to convert the world-unit bias into atlas
depth, and to match the filter spacing across cascades. Each atlas needs its own set because
consecutive fits differ, normally by a cycle of sun motion, at the up-vector swap by a third
on cascade 0.

## Caster rendering

`renderShadowLayerGeneric` draws one cascade into its strip of the atlas.

The viewport is `{ layer * res, 0, res, res }`, which is the only thing keeping cascades
apart. All use `shadowViewProj[0]` as their transform, since the C++ side uploads one
matrix per layer before the layer draws.

The whole cascade box is rendered. Older versions stencil-masked casters to the camera
frustum to save fill (and that mask's edge, sitting within reach of the filter kernel at
the screen corners, was the source of a corner flicker). With the atlas reused while the
camera turns, a frustum mask would leave the new view without casters, and rendering one
cascade per frame more than pays for the extra fill.

Casters, in order:

- `PASS_RENDERSHADOWMAP` draws distant terrain via `renderDistantLand`, only when
  `mwBridge->IsExterior()`, through `ShadowLandVS`, which lowers every vertex by
  `shadowTerrainSink` world units in world space before projection. The terrain mesh is a
  coarse twin of Morrowind's terrain and pokes through roads and floors built on it; sunk,
  only its silhouette casts.
- `PASS_RENDERSTATICSHADOWMAP` draws distant statics, only when `staticsUploaded`.

Both passes write depth only. Each sets `SlopeScaleDepthBias = 2.0` and `DepthBias = 0`,
pushing casters back in proportion to their depth slope, which is what surfaces at a
grazing sun angle need and what a constant bias cannot supply. Vertices clamp to
`pos.z = max(0, pos.z)` so casters behind the light's near plane still occlude instead of
being clipped away.

Only statics alpha test. `StaticShadowPS` clips at `a - 180.0/255.0`, remapping UVs into
the static's atlas region first and sampling with `tex2Dgrad` and explicit derivatives,
since `frac()` on the UVs would otherwise break mip selection. Terrain cannot:
`ShadowLandVS` takes position only, because `TerrainDecl` carries no texcoords.

`hasAlpha` is set per static subset by `VisibleSet::Render` and reset when that render
finishes, so it does not leak into later shadow draws.

## Caster culling

Statics come from the host over IPC:

```cpp
visExtraShared.RemoveAll();
if (staticsUploaded) {
    ipcClient.getVisibleMeshesCoarse(visExtraSharedId, range_frustum, VIS_STATIC);
}
```

`range_frustum` is the cascade's own light box, or, when `distant_land.shadows.static_range`
(cells, default 4, 0 for no limit) is smaller than the cascade radius, a light box of that
radius on the same basis, built by the same `fitLightBox`. Statics beyond the range never
enter the atlas. Statics inside it still throw their shadows past it, and terrain casts over
the whole cascade whatever the range. `VIS_STATIC` is `VIS_NEAR | VIS_FAR | VIS_VERY_FAR`, so
all three static bands are candidates. One cascade is built per frame, so this is one host
query and one terrain draw per frame.

Coarse is genuinely coarse. On the host, `get_visible_meshes_coarse` passes `None` for the
view sphere, which routes `collect_quadtree_meshes` to `QuadTree::get_visible_meshes_coarse`
and skips distance banding, LOD tier selection, and terrain horizon culling entirely. Shadow
casters are frustum-tested and nothing else.

Because the caster draws need no sorting, the query passes `VisibleSetSort::None`, which
lets the host stream results. `visible_set.Render(..., parallelRead = true)` calls
`start_read()` and then blocks in `at_end()` on a Win32 event whenever it catches up to the
host's write cursor, so the 32-bit draw loop consumes meshes while the 64-bit traversal is
still appending them.

`visExtraShared` is a scratch vector shared with `renderReflectedStatics`. Within Stage 0
its users run strictly in sequence, each calling `RemoveAll()` first: each cascade, then
water reflections.

## Depth compare and bias

Constants live in `XE Mod Shadow Data.fx`:

| Constant | Value | Role |
| --- | --- | --- |
| `shadowBias` | 24.0 | Constant receiver bias, world units. Must exceed the LOD error between a caster and its near twin. |
| `shadowBiasTexels` | 1.0 | Extra bias of one texel of the sampled cascade, in world units per unit of texel size, for the rasterisation error of coarse far cascades. |
| `shadowNormalOffset` | 1.5 | Receiver push along its normal, in texels of the sampled cascade; scaled 0.5x facing the sun to 1.5x at grazing angles. |
| `shadowNormalOffsetMax` | 16.0 | Cap on the normal offset in world units, so far cascades do not push receivers a quarter of a cell. |
| `shadowFilterRadius` | 2.0 | Spacing of a 3x3 grid of bilinear-compare taps, in cascade 0 texels, kept the same in world units on the cascades that use the grid; penumbra about `3 * radius + 1` cascade 0 texels. |
| `shadowSingleTapCascade` | 2 | First cascade sampled with one bilinear compare. From there the grid would fall inside a quarter texel and return the same value; cascade 1, at half-texel spacing, keeps the grid. |
| `shadowTerrainSink` | 24.0 | World units terrain casters are lowered by. |
| `shadowElevationFade` | `(5, 10)` | Sun elevation band in degrees over which the shadow term fades out toward the horizon; the top matches the fit clamp `shadowMinElevation`. |
| `shade` | 0.6 | Luminance floor for shadowed areas; higher is lighter. |
| `shadecolor` | `(1.0, 0.985, 0.93)` | Per-channel shadow strength, near neutral. |

Receivers project per pixel from a position and a normal, `shadowVisibility(pos, normal,
sunDir)`: view space with `sunVecView` in the near receiver pass, world space with `sunVec`
for grass, terrain and statics. `shadowSetVisibility` walks the cascades of one atlas in
order. For each, `shadowReceiverPos` pushes the position along the normal by
`shadowNormalOffset * (0.5 + sin(angle to sun)) * texel` and projects it with that atlas's
matrix, `shadowViewProj[set * 4 + layer]`. The first cascade whose clip-space margin contains
the point is sampled by `shadowLayerLit`. It subtracts `(shadowBias + shadowBiasTexels *
texel) / depthRange` from the projected z. On cascades 0 and 1 it averages a 3x3 grid of
compare taps `shadowFilterRadius` cascade 0 texels apart in world units. From
`shadowSingleTapCascade` on it takes one compare, because the grid would fall inside a
quarter texel and return the same value. The outermost cascade fades over its last 4% of
clip space.

`shadowVisibility` returns `1 - lit` from the current atlas. While a fade is running it
blends toward the next atlas's result by `shadowBlend`; otherwise a `[branch]` skips the
second walk. The second walk starts at the cascade the first one chose, or at 0 if the first
found none. The next atlas's boxes are centred at most a fade's travel away, so a finer
cascade could contain the point only right at a boundary. Near pixels therefore cost one
projection per atlas, far pixels one projection for the next atlas during a fade. A build
takes four frames and a fade 0.25 s, so at 60 fps about four frames in five run both walks.
That is why the second walk is kept short.

Why the bias is this large: the old exponential map only began to shadow at 33 units of
caster-receiver separation and saturated hundreds of units later, which quietly absorbed
every LOD mismatch. A depth compare is exact, so the tolerance has to be explicit.

## Receiver rendering

`renderShadow` walks `recordMW`, the list of draw calls MGE recorded from Morrowind this
scene, and re-draws each one with a shadow shader.

It returns early until a first atlas has been built. Otherwise it calls
`uploadShadowMatrices(inverseView)`, which uploads both atlases' cascade matrices
premultiplied by the inverse view (so the shader works from view space), binds the current
atlas to `tex3` and the next to `texShadowNext`, and sets `shadowBlend`. Per record it then
configures:

- Additive blends are skipped outright, since `destBlend == D3DBLEND_ONE` geometry cannot
  darken.
- Alpha-dependent records bind their texture and an alpha reference, either the recorded
  `alphaRef / 255` or the sentinel `0.0101f`. That odd threshold avoids interpolator noise
  along a value that should be constant across a triangle, which a rounder number like 0.5
  would sit right on top of.
- `D3DCULL_NONE` is replaced with `D3DCULL_CW`. Casters are drawn CW-only, so a two-sided
  polygon would otherwise take a false shadow on its back face.
- Skinning uploads either `recordedSkinPalettes` at the record's offset, or the record's
  four `worldViewTransforms`.

Pass selection is a two-way by two-way choice:

| | Standard | Indexed skinning |
| --- | --- | --- |
| FFE inactive | `PASS_RENDERSHADOW` | `PASS_RENDERSHADOW_INDEXED` |
| FFE active | `PASS_RENDERSHADOWFFE` | `PASS_RENDERSHADOWFFE_INDEXED` |

The indexed rows require the `render.indexed_skinning` opt-in, which defaults to false and
is restart-required, so the standard rows are what an untouched install draws. The opt-in
is additionally gated on device capability and shader support. See
[indexed-skinning.md](indexed-skinning.md).

`isPPLActive` drives the FFE choice and is recomputed each Stage 0:

```cpp
isPPLActive = (Configuration.MGEFlags & USE_FFESHADER)
    && !(Configuration.PerPixelLightFlags == 1 && !mwBridge->IntCurCellAddr());
```

The four vertex shaders differ less than the count suggests. Indexed variants use
`indexedSkin` with `BLENDINDICES` instead of sequential palette entries. All four apply the
same depth bias, `pos.z *= 1 - 2e-6` then `pos.z -= clamp(0.05 / pos.w, 0, 1e-3)`, because
the shadow pass re-transforms vertices that the original draw transformed elsewhere. Under
native per-pixel lighting, DXVK transforms them in its own shader, so the results are no
longer bit-identical. The FFE and non-FFE bodies are currently the same code with different
comments. Each emits the view-space position and normal for the per-pixel projection.

`RenderShadowsBaseVS` computes a deliberately non-physical light term so shadows stay
visible when ambient is high:

```hlsl
OUT.light = shadowSunEstimate(saturate(dot(v.normal.xyz, -sunVecView)));
```

`shadowSunEstimate` weights `sunCol` to luminance, scales by `0.25 + 0.75 * sunVis`, and
maps through `x / (shade + x)`. Fog then attenuates it by `fogMWScalar(pos.w)` squared,
or `saturate(4 * fogatt)` when `eyePos` is below sea level, which stops underwater shadows
fading out immediately.
It also fades to zero over `shadowElevationFade`, the band below which the fit clamps the
light elevation.

`RenderShadowsPS` clips unlit fragments below `2/255` and failed alpha tests, then
`v = shadowVisibility(viewpos, normal, sunVecView) * light * alpha`, clipped below `2/255`,
and output as `float4(v * shadecolor, 1)`. With `SrcBlend = Zero, DestBlend = InvSrcColor`, the
framebuffer is multiplied by `1 - v * shadecolor`, so the shader outputs how much light to
remove rather than a colour.

## Cascade selection

Receivers pick a cascade by clip-space containment, not by distance:

```hlsl
static float3 atlasMargin = float3(1-2*4*shadowRcpRes, 1-2*4*shadowRcpRes, 1);

[unroll] for(int i = 0; i < shadowCascades; ++i) {
    float4 sp = shadowReceiverPos(pos, normal, sunDir, set, i);
    [branch] if(all(saturate(atlasMargin - abs(sp.xyz)))) { /* layer i */ }
}
```

The 4-texel margin (doubled because clip space spans 2 units) keeps the filter kernel from
pulling in the neighbouring cascade's texels. Fragments outside every cascade return 0,
unshadowed.

Atlas lookup is a horizontal remap, `x * shadowCascadeSize + layer * shadowCascadeSize`,
and UVs carry the usual half-texel offset with a flipped y.

## Other consumers

Distant terrain and distant statics receive. `renderShadowMap` ends with
`uploadShadowMatrices(nullptr)` (world to shadow for both atlases, both textures, the
blend), and `renderStage0` sets `shadowDistant` to 1 when a valid atlas exists this frame,
else 0. `XE Mod Landscape.fx` and `XE Mod Statics.fx` compute the same `shadowSunEstimate`
term as the near receiver pass in their vertex shaders (`shadowLandVert`,
`shadowStaticVert`) and pass world position and world normal through; their pixel shaders
call `shadowVisibility(worldpos, normalWS, sunVec)` and multiply the lit colour by
`1 - v * shadecolor` before fog, under a `[branch]` on `shadowDistant`. Using the receiver pass's estimate rather than removing the sun term
outright keeps the brightness continuous across the near-to-distant handoff. Interior
statics emit zero terms. The water reflection reuses these shaders, so reflected terrain
and statics are shadowed too.

Grass reads the same way. `renderGrassInst` calls `uploadShadowMatrices(nullptr)` once an
atlas is valid, and `XE Mod Grass.fx` calls `shadowVisibility` under the same `shadowDistant`
branch, from world position with a zero normal, so no normal offset. Before the branch, a
run-time toggle left the last atlas frozen on the grass, and shadows disabled in the config
still cost every grass pixel the full walk.

The replacement water plane, and the sky and scattering passes, do not sample the map.

## Configuration

```toml
[distant_land.shadows]
enabled = true
map_resolution = 2048
static_range = 4.0
```

| TOML key | Default | Range | C++ binding |
| --- | --- | --- | --- |
| `distant_land.shadows.enabled` | `true` | bool | `Configuration.MGEFlags & USE_SHADOWS` (bit 31) |
| `distant_land.shadows.map_resolution` | `2048` | clamped to [1024, 2048] | `Configuration.DL.ShadowResolution` |
| `distant_land.shadows.static_range` | `4.0` | cells, clamped to [0, 300], 0 = no limit | `Configuration.ShadowStaticRange` |

The GUI puts the first two on the Distant Land tab under "Lighting and shadows": a "Dynamic
solar shadows" checkbox and a resolution dropdown offering Medium (1024) and High (2048). The
static range is TOML only. It lives outside `Configuration.DL` because that struct is the
MWSE-facing `DistantLandRenderConfig` and cannot grow.

Three runtime controls exist for the toggle, and none for the resolution:

- `MacroFunctions::ToggleShadows` flips `USE_SHADOWS` and prints a status message. Keybind
  function code 37.
- `MGEAPIv1` exposes `RenderFeature::Shadows` to MWSE, reading and writing the same flag.
- `getDistantLandRenderConfig()` hands out a pointer to `Configuration.DL`, so
  `ShadowResolution` is writable in memory, but writing it does nothing useful.

`initShadow` runs once from `init`, and `ehShadowRcpRes` is set once in `initShader`.
Changing `map_resolution` at runtime leaves the texture and the shader constant at their
old values. It needs a renderer restart. The bias and shade constants are compile-time
values in `XE Mod Shadow Data.fx` and need a relaunch to change.

## Core mods

Users can replace parts of the shadow shaders without touching the install. `CoreModInclude`
in `distantinit.cpp` resolves any `#include` whose filename starts with `XE Mod` against
`Data Files\shaders\core-mods\` first, falling back to `core\`. It is installed only for
`XE Main.fx`, `XE Shadowmap.fx`, and `XE Depth.fx`. `core-mods/README.txt` documents the
workflow: copy the file out of `core\`, edit the copy.

Two shadow files are replaceable this way.

- `XE Mod Shadow.fx` holds every receiver function, including the four bias wrappers.
  `RenderShadowsVS` and `RenderShadowsFFEVS` have identical bodies today and exist
  separately so the FFE path can be biased on its own, which is what a mod would change.
- `XE Mod Shadow Data.fx` holds the cascade count, bias, filter and shade constants. Both
  the caster side (`XE Shadowmap.fx`, for the terrain sink) and the receiver side include
  it, so an edit stays consistent across the two.

`XE Shadowmap.fx` and `XE Common.fx` are not replaceable. `distantinit.cpp` logs "Do not
replace core shaders" if one fails to compile.

Two consequences worth remembering when editing these files. Removing a function or a pass
reference is safe against a user's stale copy, because it only narrows what `XE Main.fx`
asks for, but it silently discards whatever they changed in it. And a mod that fails to
compile disables all core mods for the session, raises a `StatusOverlay` error, recompiles
each mod alone to name the culprit in `mgeXE.log`, then falls back to stock shaders. A mod
that compiles with stale constants gets no such check. A stale copy of `XE Mod Shadow.fx`
from the exponential-map era does not compile against this tree (`shadowDeltaZ`,
`shadowESM` and `ESM_*` are gone), which is the good outcome.

## Recording, and what never gets shadowed

Receivers are limited to what `inspectIndexedPrimitive` records. Two filters run before the
z-write test:

```cpp
bool isLandSplat = sceneCount == 0 && rs->vb == lastVB && rs->blendEnable
    && (rs->fvf & D3DFVF_DIFFUSE) && mwBridge->IsExterior();

const auto& stage0 = frs->stage[0];
bool isDecal = stage0.texcoordIndex != 0
    && (stage0.colorArg1 == D3DTA_TEXTURE || stage0.colorArg2 == D3DTA_TEXTURE);

if (rs->zWrite && !isLandSplat && !isDecal) { /* record */ }
```

`isLandSplat` drops the second and later passes of Morrowind's multi-pass landscape
splatting, detected by the repeated vertex buffer. `isDecal` drops passes sampling a UV set
above 0, because the shadow shader only reads alpha from texture 0 with UV 0.

Further up, `MGEProxyDevice::DrawIndexedPrimitive` skips recording entirely when
`isStencilScene && stencilRef <= 1`, which is Morrowind drawing its own stencil shadow
volumes. MGE stays out of that.

`recordMW` and `recordedSkinPalettes` are cleared at the end of Stage 0, Stage 1, and
Stage 2. Stage 1's `renderShadow` therefore sees only scene 0's opaque draws, and each
Stage 2 call sees only that scene's draws.

## Debug view

`renderShadowDebug` draws all cascades stacked in the top-right corner through
`PASS_DEBUGSHADOW`, colouring depth green to blue and marking in red any shadow texel that
falls inside the camera frustum, which shows how much of each cascade the view is using
(the atlas outside every caster reads as the cleared far plane). Because a depth texture can
only be compare-sampled through D3D9, `shadowDebugDepth` reconstructs a 3-bit depth by
counting how many of eight reference slices pass the compare. Its only call site in
`postProcess` is commented out:

```cpp
///if(!mwBridge->IsMenu()) { renderShadowDebug(); }
```

Uncomment to use it. There is no config flag.

## Gotchas

- No shadows in interiors. `CellHasWeather()` gates the whole feature.
- Nothing Morrowind draws casts a shadow. Only distant terrain and distant statics do, and
  statics only within `distant_land.shadows.static_range` of the eye.
- Cascade radii live in `shadowCascadeRadius()` in `rendershadow.cpp`; the last one follows
  `DrawDist`.
- Cascade count 4 lives in `DistantLand::kShadowCascades`, in `shadowCascades` in
  `XE Mod Shadow Data.fx`, and in the `shadowViewProj[8]` / `shadowCascade[8]` (two atlases)
  declarations in `XE Common.fx`. Nothing checks that they agree, and the second is
  user-replaceable.
- Atlas contents are up to half a second old. Fast movement (a raised Speed attribute, a
  console-set speed, a detached camera) shows through as a quarter-second dissolve rather
  than a cut. An eye jump of more than 500 units (teleport, load, door) restarts the build
  and lands it without a fade, four frames later. Turning the camera never invalidates it:
  nothing in the fit or the caster set depends on the view direction, and that must stay
  true.
- Resolution changes need a renderer restart. Writing `ShadowResolution` through the MWSE
  config pointer does nothing until then.
- The atlas is a depth texture: any sampler bound to it compares, it cannot be read as
  depth, and it must not be INTZ or DF24 or the compare silently stops.
- The caster passes leave colour writes off and a slope bias set; anything added to
  `renderShadowMap` after them, or any new consumer of the effect in Stage 0, inherits
  that unless the reset at the end of `renderShadowMap` still runs.
- `hasAlpha` is shared across every effect in the pool. Any pass that reads it must set it,
  and any pass that sets it per draw must leave it neutral.
