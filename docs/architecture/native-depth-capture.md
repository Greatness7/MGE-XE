# Native depth capture

MGE XE needs a full-scene depth texture for water, caustics, post-processing, and
the near/distant-land blend. Historically it produced that texture by recording
Morrowind's z-writing draw calls and replaying their geometry through `XE Depth.fx`.

Native depth capture replaces that replay when the active renderer exposes a safe
source depth-stencil surface. It supports both non-multisampled rendering and MSAA:

```text
no MSAA
    Morrowind renders directly into a sampleable INTZ depth-stencil
    -> XE Depth.fx converts INTZ into MGE's depth outputs

MSAA with the Morrowind DXVK depth interop (`IDxvkMorrowindInterop`)
    Morrowind renders into the normal multisampled automatic DSV
    -> DXVK resolves the nearest depth sample into single-sample INTZ
    -> XE Depth.fx converts INTZ into MGE's depth outputs

unsupported or unsafe state
    -> recorded-geometry replay
```

One setting controls the optimization:

```toml
[distant_land]
native_depth_capture = true
```

It is deliberately optional. Initialization, capability, projection, or per-stage
validation failures fall back to geometry replay rather than failing the renderer.

## 1. Output contract

Native capture preserves the depth representation consumed by the rest of MGE XE:

- `texDepthFrame` is `R32F`.
- Visible pixels contain positive view-space Z.
- Background pixels contain `1.0e38`.
- `surfDepthDepth` contains the same raw hardware depth used for ordering.
- Stage 1 replaces both outputs.
- Later stages merge only values nearer than the depth already captured.

The existing native passes in
`assets/Data Files/shaders/core/XE Depth.fx` implement the conversion:

```hlsl
rawDepth = sample(INTZ);
linearDepth = rawDepth >= 1.0
    ? 1.0e38
    : sourceM43 / (rawDepth - sourceM33);
```

The pixel shader writes `linearDepth` to `COLOR0` and `rawDepth` to `DEPTH`.
The Replace pass uses an unconditional depth write after clearing the auxiliary
depth surface. MergeNearest uses `LessEqual`, so first-person geometry and other
later scenes replace the existing outputs only where they are nearer.

Nothing reprojects raw auxiliary depth. Backend selection therefore checks
source projection and depth-stencil provenance.

## 2. Backend selection

`DistantLand::NativeDepthBackend` records the active implementation:

| Backend | Source DSV | INTZ role |
| --- | --- | --- |
| `None` | none | unavailable; use replay |
| `IntzMainDsv` | `surfDepthStencil` | persistent main DSV and conversion source |
| `DxvkMsaaResolve` | `surfAutoDepthStencil` | single-sample resolve target and conversion source |

`DistantLand::initDepth` always creates the legacy final outputs first. When native
capture is enabled it then:

1. checks the viewport and main render-target extents;
2. verifies the native shader handles;
3. retains the currently active automatic DSV and inspects its actual
   `D3DSURFACE_DESC`;
4. creates a full-size, one-level, single-sample INTZ texture with
   `D3DUSAGE_DEPTHSTENCIL`;
5. selects the backend from the retained DSV's actual multisample type.

The DSV description is authoritative. `Configuration.AALevel` only logs a
disagreement with the active surface.

### 2.1 Non-MSAA

`initDepth` binds the INTZ texture as the main DSV. Morrowind may later try to restore the original
automatic DSV during render-target changes, so `ProxyDevice` substitutes:

```text
surfAutoDepthStencil -> surfDepthStencil
```

All ordinary Morrowind and MGE rendering therefore populates the sampleable INTZ
surface directly.

### 2.2 MSAA

The normal multisampled automatic DSV remains bound. MGE XE queries the real
`IDirect3DDevice9` for `IDxvkMorrowindInterop`, requires interface version 1, and
requires `DXVK_MORROWIND_CAP_MSAA_DEPTH_RESOLVE`.

The MSAA path arms no DSV substitution. Immediately before conversion, MGE asks DXVK to
resolve the active automatic DSV into the single-sample INTZ intermediate.

### 2.3 Replay

The backend remains `None` when native capture is disabled or any initialization
requirement is unavailable. The existing `renderDepth` and
`renderDepthAdditional` paths remain the correctness fallback.

## 3. Frame integration

The dispatcher is `DistantLand::captureNativeDepth`.

For `IntzMainDsv`, it calls the existing INTZ conversion directly. For
`DxvkMsaaResolve`, it performs:

```text
resolveNativeDepthMsaa()
    -> captureNativeDepthIntz()
```

These calls must remain adjacent. DXVK may fold the resolve into Morrowind's
active render pass. The render-target switch at the start of
`captureNativeDepthIntz` ends that pass before INTZ is sampled. Inserting another
draw between the resolve and the target switch would include unintended geometry.

### 3.1 Stage 1

At the end of scene 0, after distant land and grass have contributed to the main
DSV, native capture runs in Replace mode when:

- the setting is enabled;
- a native backend is active;
- DSV provenance is safe;
- Morrowind's scene-0 projection is canonical; and
- the backend capture succeeds.

Otherwise `renderDepth` clears and rebuilds the outputs with geometry replay.

A successful native Stage 1 sets both `stage1UsedNativeDepth` and
`nativeStage2Eligible`.

### 3.2 Stage 2

Later non-UI scenes include post-stencil redraws, sorted alpha, and first-person
geometry. Each eligible invocation:

1. retrieves the current fixed-function projection;
2. verifies projection and DSV provenance;
3. captures in MergeNearest mode.

Native Stage 2 runs only after a successful native Stage 1 in the same
frame. If any Stage 2 invocation falls back, MGE clears `nativeStage2Eligible`
and all remaining Stage 2 invocations in that frame use replay. This prevents raw
native depth from being mixed with a scene whose representation was not accepted.

## 4. Projection and DSV provenance

The current projection does not prove that every pixel already stored in a DSV
was written with that projection. A noncanonical scene can write depth and then
restore a canonical projection without clearing the old values.

`dsvMayBeNoncanonical` conservatively tracks this:

- installing a noncanonical effective projection sets the flag;
- installing a later canonical projection does not clear it;
- a z-buffer clear clears it only when the currently bound DSV equals
  `nativeDepthSourceSurface()`.

The native source is the INTZ main DSV without MSAA and the automatic
multisampled DSV with DXVK MSAA resolve.

`projectionIsCanonical` compares the active projection's `_33` and `_43` terms
against MGE's near-4/far-DrawDist depth encoding. Rejection is intentionally
conservative. A false positive costs performance by using replay, not correctness.

## 5. Private DXVK interop

The COM declaration is duplicated, byte-identically, in the independent
repositories:

```text
MGE-XE/d3d8/cpp/mge/dxvk_morrowind_interop.h
DXVK/src/d3d9/dxvk_morrowind_interop.h
```

Version 1 uses GUID `2ff12bfc-4622-4d9d-bcbf-1501f37e8aa3`:

```cpp
IDxvkMorrowindInterop : IUnknown {
    uint32_t GetInterfaceVersion();
    uint64_t GetCapabilities();
    HRESULT ResolveDepthMinV1(
        IDirect3DSurface9* sourceMsaaDepth,
        IDirect3DSurface9* destinationIntz);
};
```

The depth path requires this capability bit:

```text
DXVK_MORROWIND_CAP_MSAA_DEPTH_RESOLVE
```

The shared header also defines separate per-pixel-lighting capability bits for
`IDxvkMorrowindPplInterop1` (GUID `275c3348-5724-4a7e-aac0-46ceda965739`). That is a
different interface and is not the native depth-resolve dependency.

`DxvkMorrowindInterop` is a direct `D3D9DeviceEx` member. Its COM reference
operations delegate to the parent device, and its public methods acquire the
normal `D3D9DeviceLock`.

### 5.1 Capability

DXVK recomputes the capability from the current automatic DSV. It requires:

- an automatic DSV and backing Vulkan image;
- a depth aspect;
- more than one sample;
- depth resolve mode `VK_RESOLVE_MODE_MIN_BIT`;
- `independentResolveNone` when the actual format carries stencil.

The stencil condition exists because the resolve uses MIN for depth and NONE
for stencil.

### 5.2 Per-call validation

`ResolveDepthMinV1` validates everything before enqueue:

- both pointers are non-null and distinct;
- both pass DXVK's private texture-interoperability QI gate;
- both belong to the current D3D9 device;
- the source is both the automatic DSV and the currently bound DSV;
- both resources are default-pool depth-stencil resources with backing images;
- the source is a standalone, full level-0 multisampled surface;
- the destination is a full level-0, one-mip, single-sample INTZ texture surface;
- extents and layer counts match;
- both images have depth-stencil-attachment usage;
- actual Vulkan formats match;
- the capability predicate still passes.

The resolve region uses the actual Vulkan format's complete aspect mask. D24S8
and D32S8 therefore use both depth and stencil aspects even though stencil is not
resolved.

Validation results:

| Result | Meaning |
| --- | --- |
| `S_OK` | resolve enqueued into the normal command stream |
| `D3DERR_INVALIDCALL` | invalid object, identity, state, usage, or subresource |
| `D3DERR_NOTAVAILABLE` | capability unavailable or actual formats differ |

### 5.3 Resolve command

The adapter captures only reference-counted backend images, the resolve region,
and the actual format into `D3D9DeviceEx::EmitCs`. The queued command calls:

```cpp
ctx->resolveImage(
    destinationImage,
    sourceImage,
    region,
    actualFormat,
    VK_RESOLVE_MODE_MIN_BIT,
    VK_RESOLVE_MODE_NONE);
```

Full matching subresources and the capability checks keep this operation on
DXVK's inline or render-pass attachment resolve path. It does not use the
shader-based framebuffer fallback.

The implementation does not:

- make the automatic DSV sampleable;
- change its layout policy;
- relocate either image;
- force submission;
- flush or synchronize the command-stream thread;
- wait for the GPU;
- read depth back to the CPU.

At a partially covered MSAA pixel, MIN chooses the nearest covered sample. This
preserves thin geometry for downstream depth effects, but its silhouette can
differ from sample-zero resolve or geometry replay.

## 6. Failure and lifecycle behavior

Native capture is an optimization and never a renderer startup requirement.

- Initialization failure cleans up partial INTZ and interop resources and leaves
  replay available.
- `D3DERR_INVALIDCALL` is a stage-local fallback; a temporary unexpected DSV does
  not permanently disable the backend.
- `D3DERR_NOTAVAILABLE` disables the DXVK backend until renderer restart and logs
  once.
- Stage 1 fallback clears and rebuilds both final outputs.
- Stage 2 fallback preserves the existing outputs, adds replayed later geometry,
  and disables further native merges for that frame.

Teardown disarms DSV substitution first. It restores the automatic DSV only for
`IntzMainDsv`; the automatic DSV was never replaced for `DxvkMsaaResolve`.
Teardown releases the DXVK interface unconditionally, including partial
initialization paths.

Queued DXVK commands remain safe during teardown because they retain `Rc<DxvkImage>`
references rather than MGE or D3D9 wrapper pointers.

## 7. Behavioral scope

Native capture reads the depth Morrowind and MGE actually produced. It therefore
preserves:

- original alpha-test coverage;
- original depth bias;
- original skinning and vertex transforms;
- original culling and clipping;
- actual near/distant and grass ordering;
- actual z-write state;
- MSAA sample coverage.

Geometry replay reconstructs those decisions from recorded state and deliberately
normalizes or excludes some draws. Native silhouettes can therefore differ around
foliage, decals, blended alpha, range cutoffs, and partially covered MSAA pixels.
These are intentional consequences of using the authoritative DSV rather than
trying to reproduce the replay heuristics.

This feature removes the depth-replay consumer of `recordMW`. It does not remove:

- main-view draw recording;
- shadow-receiver replay;
- shadow-map caster replay;
- stencil shadows;
- the auxiliary depth surface;
- the geometry-replay fallback.

MSAA native capture requires the matching Morrowind-specific DXVK depth interop.
Stock DXVK, or any D3D9 implementation that does not expose `IDxvkMorrowindInterop`
with the required capability, cannot use `DxvkMsaaResolve` and falls back to geometry
replay. Native D3D9 or another D3D9 implementation can still use the non-MSAA INTZ
backend when INTZ creation succeeds; otherwise it also uses replay.

## 8. Source map

MGE XE:

| File | Responsibility |
| --- | --- |
| `d3d8/cpp/mge/dxvk_morrowind_interop.h` | private COM ABI |
| `d3d8/cpp/mge/distantland.h` | backend, interface, resources, stage state |
| `d3d8/cpp/mge/distantinit.cpp` | resource creation, backend selection, teardown |
| `d3d8/cpp/mge/distantland.cpp` | stage routing, projection/provenance policy |
| `d3d8/cpp/mge/renderdepth.cpp` | MSAA resolve and INTZ conversion dispatcher |
| `d3d8/cpp/proxydx/d3d8device.cpp` | non-MSAA DSV substitution |
| `d3d8/cpp/mge/mged3d8device.cpp` | clear and effective-projection hooks |
| `assets/Data Files/shaders/core/XE Depth.fx` | native conversion and merge passes |

DXVK:

| File | Responsibility |
| --- | --- |
| `src/d3d9/dxvk_morrowind_interop.h` | matching private COM ABI |
| `src/d3d9/d3d9_device.h/.cpp` | adapter ownership and QueryInterface route |
| `src/d3d9/d3d9_interop.h/.cpp` | capability, validation, and resolve enqueue |
| `src/dxvk/dxvk_context.cpp` | existing inline/render-pass resolve implementation |

## 9. Verification

Static checks:

```powershell
cargo check -p d3d8 --target i686-pc-windows-msvc
cargo run -p config-contract-test --target i686-pc-windows-msvc
cargo test -p mge-config
```

The custom DXVK fork requires its 32-bit build:

```text
ninja -C build.w32.release
```

`XE Depth.fx` must continue to compile with its `DEPTH` output semantic.

Runtime acceptance should cover:

- no AA with the INTZ main DSV;
- 2x/4x/8x MSAA where supported;
- native capture disabled;
- missing or older DXVK;
- default D24S8 and deliberately incompatible depth formats;
- dense exterior and interior scenes;
- grass and alpha-tested foliage;
- stencil shadows;
- water, caustics, sunshafts, SSAO/DOF;
- first-person geometry and sorted alpha;
- runtime distant-land toggles;
- fullscreen and device recreation.

The initialization log identifies the selected backend. On the MSAA path it
reports the D3D9 format, actual sample type, interop version, and capability bits.
Renderer teardown reports native Stage 1/2 captures, legacy fallbacks, and replay
draw counts.
