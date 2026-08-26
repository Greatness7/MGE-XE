# Sun shadows

MGE XE casts sun shadows from distant-land geometry onto Morrowind's own geometry.
Two cascades, an orthographic light camera per cascade, and an exponential shadow map
(ESM) packed side by side into one R16F texture.

The important asymmetry: casters and receivers are disjoint sets. Only distant terrain
and distant statics write into the shadow map. Only Morrowind's near geometry and MGE's
grass read from it. Nothing Morrowind draws itself casts a shadow, so actors, the player,
doors, and hand-placed clutter inside the near band produce no MGE shadow. Morrowind's own
stencil shadows still handle actors, controlled by `[General] High Detail Shadows` in
`Morrowind.ini`.

Code lives in `d3d8/cpp/mge/rendershadow.cpp`, with resource creation in `distantinit.cpp`
and gating in `distantland.cpp`.
Shaders are `XE Shadowmap.fx` (caster side) and `XE Mod Shadow.fx` (receiver side,
included by `XE Main.fx`). `XE Mod Shadow Data.fx` holds the constants both sides share
and is included by each of them directly, which is how one edit changes the ESM encode and
decode together. Two of those three files are user-replaceable, see [Core mods](#core-mods).

## Resources

`DistantLand::initShadow` allocates everything once, at distant-land init:

| Resource | Type | Dimensions | Format |
| --- | --- | --- | --- |
| `texShadow` | render-target texture | `2 * res` by `res` | `D3DFMT_R16F` |
| `texSoftShadow` | render-target texture | `2 * res` by `res` | `D3DFMT_R16F` |
| `surfShadowZ` | depth-stencil surface | `2 * res` by `res` | `D3DFMT_D24S8` |
| `vbFullFrame` | vertex buffer | 4 verts, 12 bytes each | full-target quad |

`res` is `Configuration.DL.ShadowResolution`, so the atlas is 2048x1024 or 4096x2048.
Cascade count is `DistantLand::kShadowCascades` on the C++ side, which sizes the atlas and
the `smView`/`smProj`/`smViewproj` arrays, and `shadowCascades` in
`XE Mod Shadow Data.fx` on the shader side. Nothing checks that the two agree, and the
shader copy sits in a file users can replace.

The names are misleading. Casters render into `texSoftShadow`, the horizontal blur writes
`texShadow`, and the vertical blur writes back into `texSoftShadow`. `texSoftShadow` is
what receivers sample, which is the only part the name gets right.

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
survives the pass.

## Building the map

`renderShadowMap` runs in this order:

1. Bind `texSoftShadow` and `surfShadowZ` through `RenderTargetSwitcher`, saving the viewport.
2. Clear depth and stencil with `device->Clear(..., D3DCLEAR_ZBUFFER|D3DCLEAR_STENCIL, 0, 1.0, 0)`.
3. Clear colour by drawing `vbFullFrame` through `PASS_CLEARSHADOWMAP`. `ShadowClearVS`
   forces `depth = 1.0`, so every texel gets `ESM_scale * 1.0`, which is 32768. There is no
   `D3DCLEAR_TARGET` here because the value written is an encoded depth, not zero.
4. Invert `mwView * mwProj` into `inverseCameraProj`, which maps the camera's unit
   clip cube back to world space.
5. `renderShadowLayer(0, 1000.0f, ...)` then `renderShadowLayer(1, 4000.0f, ...)`. Both
   change the viewport.
6. Restore the saved viewport, then run the separable blur.

The blur is two full-target draws, one pass each:

| Pass | Target | Source | Axis |
| --- | --- | --- | --- |
| `PASS_SOFTENSHADOWMAP_H` | `texShadow` | `texSoftShadow` | horizontal |
| `PASS_SOFTENSHADOWMAP_V` | `texSoftShadow` | `texShadow` | vertical |

Both call `shadowSoften` with an axis vector. It takes 5 taps at offsets 0, ±0.71, and
±1.42 texels with weights 1.0, 0.8, and 0.2, then divides by 3.0. Each draw filters the
whole atlas, including across the cascade boundary, which is why receivers keep a 4-texel
margin (below). Filtering happens in linear depth rather than exp space, which looks better
at the cost of expanding silhouettes by roughly a pixel.

## Fitting a cascade

`renderShadowLayer` builds the light camera for one cascade. The interesting part is that
it separates the projection centre into a stable translation and a quantized rotation
remainder, which is what keeps the map from shimmering.

Light direction comes from `sunPos` during the day and `sunVec` at night:

```cpp
D3DXVECTOR4 lightVec = (sunPos.z > 0) ? -sunPos : sunVec;
```

`sunPos` is the normalized sun disc direction from `MWBridge::GetSunDir`, flipped in z when
`sunVis` is zero so it sets instead of bouncing at the horizon. `sunVec` is the D3D sun
light direction captured in `setSunLight`, which Morrowind also uses for interiors.

The projection centre sits one radius ahead of the player, at half that in z on the
assumption the player is looking at ground rather than straight down:

```cpp
lookAt = eyePos + radius * (eyeVec.x, eyeVec.y, 0.5f * eyeVec.z)
```

The light camera does not look at `lookAt`. It looks at `lookAtEye`, the eye position
snapped to a 16-unit grid, from `zrange = kCellSize` (8192) units back along `lightVec`,
with up `(0, 0, 1)`. The ortho projection is `2 * radius` wide, `(1 + |lightVec.z|) * radius`
tall, near 0, far `2 * zrange`. Height scales with sun elevation because a high sun
compresses the world's z extent into less of the light-space y axis.

The difference between the two centres, `lookAtEye - lookAt`, is then transformed into
shadow clip space and folded back into the view-projection translation, quantized to whole
texels in x and y:

```cpp
const float quantizer = 2.0f / Configuration.DL.ShadowResolution;
viewproj->_41 += quantizer * floor(dv.x / quantizer);
viewproj->_42 += quantizer * floor(dv.y / quantizer);
viewproj->_43 += dv.z;
```

Clip space spans [-1, +1] over `res` texels, hence the factor of 2. The 16-unit eye snap
handles translation swimming; this step handles rotation shimmer. z is not quantized
because depth precision does not alias the same way.

## Caster rendering

`renderShadowLayerGeneric` draws one cascade into its half of the atlas.

The viewport is `{ layer * res, 0, res, res }`, which is the only thing keeping cascade 0
and cascade 1 apart. Both use `shadowViewProj[0]` as their transform, since the C++ side
uploads one matrix per layer before the layer draws.

Before any casters, `PASS_SHADOWSTENCIL` draws the camera frustum's silhouette in light
clip space. `buildStencilHull` (in `rendershadow.cpp`) takes the eight corners of the camera
clip cube through `inverseCameraProj` and the cascade view-projection, divides by w, clips
the twelve edges to the light far plane (the near side is clamped in the vertex shader, not
clipped, so it is left alone), offsets every point by the four corners of a
`shadowStencilMarginTexels` (8) square, and takes the convex hull. That is the Minkowski sum
of the silhouette and the square: an exact 8-texel square dilation, so at least 8 texels of
outward coverage in every direction (up to 11.3 on the diagonals), at vertices as well as
along edges. The square is the right structuring element for a separable blur. The hull is
drawn once as a fan through `DrawPrimitiveUP` with `world` and `shadowViewProj[0]` both
identity. A singular camera projection, a non-positive or non-finite w, or any non-finite
point falls back to masking the whole cascade, which keeps NaN out of the hull sort and is
only slower. The pass writes no colour
(`ColorWriteEnable = 0`), disables z, and sets stencil to `replace` with `StencilRef = 1`.
Both caster passes then run `StencilFunc = notequal, StencilRef = 0`, so they only touch
texels the camera could actually see.

The margin exists because the blur reaches about 3 texels. Without it, receivers at the
frustum edge blurred into the cleared "lit" atlas, and the aliased mask edge moving each
frame made them flicker; the frustum's near-plane corners (the bottom screen corners when
looking down) were the visible case. Drawing translated copies of the frustum is not a
substitute: a union of copies leaves a sharp silhouette vertex with no margin at all unless
a copy happens to lie along its bisector.

Casters, in order:

- `PASS_RENDERSHADOWMAP` draws distant terrain via `renderDistantLand`, only when
  `mwBridge->IsExterior()`.
- `PASS_RENDERSTATICSHADOWMAP` draws distant statics, only when `staticsUploaded`.

Both pixel shaders write `ESM_scale * depth`, where depth is `pos.z / pos.w` from the
linear ortho projection. Vertices clamp to `pos.z = max(0, pos.z)` so casters behind the
near plane still occlude instead of being clipped away.

Only statics alpha test. `StaticShadowPS` clips at `a - 180.0/255.0`, remapping UVs into
the static's atlas region first and sampling with `tex2Dgrad` and explicit derivatives,
since `frac()` on the UVs would otherwise break mip selection. Terrain cannot: `ShadowVS`
takes position only, because the two declarations bound to it, `TerrainDecl` for terrain
and `WaterDecl` for the stencil hull, carry no texcoords. `ShadowPS` therefore just
encodes depth.

Alpha testing is handled by `StaticShadowPS`, not `ShadowPS`. The shared `hasAlpha` flag is
set per static subset by `VisibleSet::Render` and reset when that render finishes, so it
does not leak into later shadow draws.

## Caster culling

Statics come from the host over IPC:

```cpp
visExtraShared.RemoveAll();
if (staticsUploaded) {
    ipcClient.getVisibleMeshesCoarse(visExtraSharedId, range_frustum, VIS_STATIC);
}
```

`range_frustum` is built from the cascade's own view-projection, and `VIS_STATIC` is
`VIS_NEAR | VIS_FAR | VIS_VERY_FAR`, so all three static bands are candidates.

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
the three users run strictly in sequence, each calling `RemoveAll()` first: cascade 0,
cascade 1, then water reflections.

## ESM encoding

Constants live in `XE Mod Shadow Data.fx`:

| Constant | Value | Role |
| --- | --- | --- |
| `ESM_c` | 60.0 | Exponent. Higher means a sharper shadow root and weaker softening. Float range caps it near 88. |
| `ESM_bias` | `2e-3 * ESM_c` | Counters blurred depth pushing surface values through objects. |
| `ESM_scale` | 32768.0 | Spreads stored depth across most of the FP16 range. |
| `shade` | 0.4 | Luminance floor for shadowed areas. |
| `shadecolor` | `(1.0, 0.97, 0.81)` | Per-channel shadow strength, so shadows lean blue. |

The map stores `ESM_scale * depth`. A receiver decodes with a divide, subtracts its own
light-space depth to get `dz`, and converts:

```hlsl
float shadowESM(float dz) {
    return 1 - saturate(exp(ESM_c * dz + ESM_bias));
}
```

Blurring an exponential encoding is what makes this cheap: filtering the stored values
approximates filtering the visibility function, so one 5-tap separable blur buys soft edges
without per-receiver PCF.

## Receiver rendering

`renderShadow` walks `recordMW`, the list of draw calls MGE recorded from Morrowind this
scene, and re-draws each one with a shadow shader.

Per record it uploads `viewToShadow[2]` (inverse view times each cascade's view-projection,
so the shader can work from view space), binds `texSoftShadow` to `tex3`, and configures:

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
comments.

`RenderShadowsBaseVS` computes a deliberately non-physical light term so shadows stay
visible when ambient is high:

```hlsl
OUT.light = shadowSunEstimate(saturate(dot(v.normal.xyz, -sunVecView)));
```

`shadowSunEstimate` weights `sunCol` to luminance, scales by `0.25 + 0.75 * sunVis`, and
maps through `x / (shade + x)`. Fog then attenuates it by `fogMWScalar(pos.w)` squared,
or `saturate(4 * fogatt)` when `eyePos` is below sea level, which stops underwater shadows
fading out immediately.

`RenderShadowsPS` clips four times: unlit fragments below `2/255`, failed alpha tests,
`clip(-dz)` for anything the map says is lit, and a final shadow contribution below
`2/255`. The survivors get
`v = shadowESM(dz) * light * alpha`, faded at the atlas edge by
`saturate(25 * (1 - abs(shadow1pos.xy)))`, and output as `float4(v * shadecolor, 1)`. With
`SrcBlend = Zero, DestBlend = InvSrcColor`, the framebuffer is multiplied by
`1 - v * shadecolor`, so the shader outputs how much light to remove rather than a colour.

## Cascade selection

Receivers pick a cascade by clip-space containment, not by distance:

```hlsl
static float3 atlasMargin = float3(1-2*4*shadowRcpRes, 1-2*4*shadowRcpRes, 1);

[branch] if(all(saturate(atlasMargin - abs(shadow0pos.xyz)))) { /* layer 0 */ }
else if(all(saturate(atlasMargin - abs(shadow1pos.xyz)))) { /* layer 1 */ }
```

The 4-texel margin (doubled because clip space spans 2 units) keeps the blur kernel from
pulling in the neighbouring cascade's texels. Fragments outside both cascades keep the
initial `dz = 1e-6`, a small positive value that `clip(-dz)` rejects, so they render unshadowed.

Atlas lookup is a horizontal remap, `x * 0.5 + layer * 0.5`, and UVs carry the usual
half-texel offset with a flipped y.

## Other consumers

Grass is the only other reader. `renderGrassInst` binds `texSoftShadow` to `tex3`, and
`XE Mod Grass.fx` calls the same `shadowDeltaZ` and `shadowESM` with the same cascade logic,
differing only in that it transforms from world space rather than view space and applies the
result directly to the pixel colour instead of relying on blend state.

Distant statics, distant terrain, the replacement water plane, and the sky and scattering
passes do not sample the shadow map. Distant geometry casts but does not receive.

## Configuration

```toml
[distant_land.shadows]
enabled = true
map_resolution = 2048
```

| TOML key | Default | Range | C++ binding |
| --- | --- | --- | --- |
| `distant_land.shadows.enabled` | `true` | bool | `Configuration.MGEFlags & USE_SHADOWS` (bit 31) |
| `distant_land.shadows.map_resolution` | `2048` | clamped to [1024, 2048] | `Configuration.DL.ShadowResolution` |

The GUI puts both on the Distant Land tab under "Lighting and shadows": a "Dynamic solar
shadows" checkbox and a resolution dropdown offering Medium (1024) and High (2048).

Three runtime controls exist for the toggle, and none for the resolution:

- `MacroFunctions::ToggleShadows` flips `USE_SHADOWS` and prints a status message. Keybind
  function code 37.
- `MGEAPIv1` exposes `RenderFeature::Shadows` to MWSE, reading and writing the same flag.
- `getDistantLandRenderConfig()` hands out a pointer to `Configuration.DL`, so
  `ShadowResolution` is writable in memory, but writing it does nothing useful.

`initShadow` runs once from `init`, and `ehShadowRcpRes` is set once in `initShader`.
Changing `map_resolution` at runtime leaves the textures, the depth surface, and the shader
constant at their old values. It needs a renderer restart.

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
- `XE Mod Shadow Data.fx` holds the ESM and shade constants. Both the caster encode and the
  receiver decode include it, so an edit stays consistent across the two.

`XE Shadowmap.fx` and `XE Common.fx` are not replaceable. `distantinit.cpp` logs "Do not
replace core shaders" if one fails to compile.

Two consequences worth remembering when editing these files. Removing a function or a pass
reference is safe against a user's stale copy, because it only narrows what `XE Main.fx`
asks for, but it silently discards whatever they changed in it. And a mod that fails to
compile disables all core mods for the session, raises a `StatusOverlay` error, recompiles
each mod alone to name the culprit in `mgeXE.log`, then falls back to stock shaders. A mod
that compiles with stale constants gets no such check.

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

`renderShadowDebug` draws both cascades in the top-right corner through `PASS_DEBUGSHADOW`,
colouring depth green to blue and marking in red any shadow texel that falls inside the
camera frustum, which makes wasted atlas area obvious. Its only call site in `postProcess`
is commented out:

```cpp
///if(!mwBridge->IsMenu()) { renderShadowDebug(); }
```

Uncomment to use it. There is no config flag.

## Gotchas

- No shadows in interiors. `CellHasWeather()` gates the whole feature.
- Nothing Morrowind draws casts a shadow. Only distant terrain and distant statics do.
- Cascade radii are compile-time constants, `shadowNearRadius = 1000` and
  `shadowFarRadius = 4000`, both in `rendershadow.cpp`.
- Cascade count 2 lives in `DistantLand::kShadowCascades` and in `shadowCascades` in
  `XE Mod Shadow Data.fx`. Nothing checks that they agree, and the second is user-replaceable.
- Resolution changes need a renderer restart. Writing `ShadowResolution` through the MWSE
  config pointer does nothing until then.
- `texShadow` holds the horizontal blur intermediate, not the final map. Receivers want
  `texSoftShadow`.
- `hasAlpha` is shared across every effect in the pool. Any pass that reads it must set it,
  and any pass that sets it per draw must leave it neutral.
