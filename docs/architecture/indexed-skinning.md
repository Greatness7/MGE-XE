# Indexed matrix-palette skinning

Morrowind's NetImmerse renderer uses fixed-function hardware skinning for animated NPC
and creature geometry. Before rendering, `NiSkinPartition` divides each skinned geometry
into partitions whose vertices the matrix-blending hardware of the era could process.

Indexed skinning rebuilds those partitions around a larger bone palette, collapsing the
partition count per geometry and cutting CPU-side draw submission. It spans three
components: MGE XE's `d3d8.dll`, a custom DXVK `d3d9.dll`, and Morrowind itself via
in-process code patches. If any component fails its authorization gate, MGE leaves the
stock engine behavior in use.

Indexed skinning ships in the beta as an opt-in for testers. It defaults off and takes
effect only after a full game restart; it is not a finished default-on path.

## 1. Why

The engine reads `D3DCAPS8.MaxVertexBlendMatrices`, which is four, and passes it to
`NiSkinPartition::MakePartitions` as *both*:

- the maximum number of bone influences retained per vertex, and
- the maximum number of distinct bones available to an entire partition.

Four influences per vertex is reasonable. Limiting the whole partition
to the same four-bone set is what causes the draw-call explosion. Every partition gets
its own vertex and index buffers, matrix setup, state changes, and draw submission. Hands
can need dozens of draws; full-body NPC or creature models can contribute more than 200.
MGE XE then replays relevant geometry for depth and shadow passes, multiplying the cost.
In dense cities this is one of the game's largest CPU bottlenecks.

Testing ruled out two alternatives:

- **CPU/software skinning.** It collapses each geometry to one draw, but deforming every
  skinned vertex per frame costs roughly what the partition overhead it replaces costs.
- **Merging partitions inside DXVK.** Morrowind and MGE XE have already paid most of the
  per-draw CPU cost by then, and each partition already has distinct buffers and matrix
  state.

The shipped approach is conventional indexed matrix-palette GPU skinning. Vertices keep
at most four influences, but their four byte-sized indices select from a larger palette
of `N` bones for the draw. Morrowind's partition builder *already* produces the required
`bonePalette` data and its renderer already uploads every matrix in a partition; the
missing work was preserving those indices in the vertex buffer and teaching the render
stack to consume them.

This removes the partition multiplier within each `NiGeometry`. It does not merge
separate body-part, armor, clothing, texture, or material geometries, so one draw per
geometry per pass remains the practical floor.

## 2. Measured baseline

Temporary instrumentation around `NiDX8Renderer::DrawSkinnedPrimitive` (`0x6ADE70`)
collected eight 300-frame windows, 2,400 frames total:

```text
skinnedGeometries        = 383,543
skinnedGeometries/frame  = 159.81
partitionSum             = 731,025
partitions/frame         = 304.59
partitionMultiplier      = 1.906
preExistingPartitions    = 383,253 (99.92%)
```

These are historical measurements from temporary instrumentation. The capture records
the window and frame counts but not the scene, save, build, or configuration, so treat
them as a design baseline rather than a current beta performance guarantee.

Per-window multipliers ranged from 1.70 to 2.35. The theoretical one-partition floor
removes 347,482 partition draws across the sample, 144.78 per frame, or 47.53% of
partition draws.

Palette coverage of the observed workload:

| Palette size | Geometry coverage |
|---:|---:|
| 4 | 77.66% |
| 8 | 95.34% |
| 16 | 99.39% |
| 24 | 99.39% |
| 32 | 99.65% |

The 99.92% pre-existing partition rate drives the design. Only 290 of 383,543 calls lacked
a partition, so rebuilding cannot rely on the engine's lazy null-partition path; the module
must validate and rebuild incompatible existing partitions.

### Palette size: unresolved

The current implementation constant is `MGE_INDEXED_SKINNING_PALETTE_SIZE = 8`, in
`d3d8/cpp/mge/mgeindexedskinning.h`.

Profiling originally selected 16 because 8 leaves 4.66% of geometry samples above the
palette (most of it the 9-bone bucket alone, 3.31%) while 16 covers 99.39%. A later change
reduced the palette from 16 to 8 across MGE XE, DXVK, and the original prototype. Neither
source comments nor commit messages record why. The 1.906 aggregate multiplier above came
from 16 and does not describe the shipped build.

In-game tests validated the 8-bone build. Treat it as deliberate but unexplained, not an
oversight to correct. Before changing it,
re-profile, and update `mgeindexedskinning.h`, the DXVK build, and this document
together. All three must agree, or the capability handshake in §5 will refuse to enable
the feature.

## 3. Data flow

```text
NiSkinData
    |
    +- MakePartitions(N distinct bones, 4 influences)
    |     `- weights + byte bonePalette indices
    |
    +- MGE XE indexed vertex packer (morrowindskinning.cpp)
    |     `- XYZB4 + LASTBETA_UBYTE4
    |
    +- MGE XE
    |     +- derive indexed skinning from FVF
    |     +- replacement PPL/FFE shader path
    |     `- passthrough fixed-function path
    |
    `- Custom DXVK
          `- indexed FFP shader + enlarged matrix palette
```

Vertex layout for the documented Direct3D
[indexed blending](https://learn.microsoft.com/en-us/windows/win32/direct3d9/using-indexed-vertex-blending)
representation:

```cpp
struct IndexedSkinnedPosition {
    float position[3];
    float weights[3];       // Fourth weight is implicit.
    std::uint8_t bones[4];  // Partition-local matrix indices.
};
```

```cpp
D3DFVF_XYZB4 | D3DFVF_LASTBETA_UBYTE4
D3DRS_VERTEXBLEND = D3DVBF_3WEIGHTS
D3DRS_INDEXEDVERTEXBLENDENABLE = TRUE
```

Base stride is 28 bytes (12 position + 12 weights + 4 indices); the packer appends normal,
diffuse color, and texture-coordinate sets in the engine's existing order. Geometry with no
texture-coordinate set still gets one zero-filled set, matching the stock packer's FVF.

## 4. Morrowind engine hooks

Owned by MGE XE in `d3d8/cpp/mge/morrowindskinning.{h,cpp}`. The public interface has
three functions:

```cpp
namespace MorrowindIndexedSkinning {
    void installHooks();      // one-shot, from MGEProxyDevice construction
    bool hooksInstalled();    // one input to the capability gate
    void onDeviceReleased();  // from MGEProxyDevice::Release at refcount zero
}
```

The NetImmerse layouts, engine addresses, and patch helpers are private to that module.
It carries local minimal declarations of `NiObject`/`NiPointer`, `NiSkinPartition` and its
`Partition`, `NiSkinData`, `NiSkinInstance`, `NiGeometryData`, `NiDX8Renderer`, and
`NiDX8VertexBufferManager`, each with size assertions so layout drift is a build break
rather than runtime corruption.

### 4.1 Patch sites

| Purpose | Address | Original |
|---|---:|---|
| `NiDX8VertexBufferManager::PackSkinnedVB` | `0x6BE2B0` | `81 EC 38 01 00 00` |
| Resume after copied prologue | `0x6BE2B6` | none |
| `MakePartitions` call site | `0x6ADEDF` | CALL `0x6C78F0` |
| `DrawSkinnedPrimitive` call site (TriShape) | `0x6ACF36` | CALL `0x6ADE70` |
| `DrawSkinnedPrimitive` call site (TriStrips) | `0x6AD006` | CALL `0x6ADE70` |
| Engine critical-section lock / unlock | `0x693F00` / `0x693F10` | none |
| Vertex-buffer critical section | `0x7DEA78` | none |

Every site verifies the CALL target or prologue bytes it is replacing, fails closed,
and logs the conflict. This is what detects an older development build of the
MWSE fork that still owns these sites (see §7) rather than silently stacking on top of it.

Installation is *not* transactional. A later site failure can prevent rollback of an
earlier write, so every hook preserves stock behavior while authorization is false:

- the packer hook calls its original trampoline,
- the partition hook forwards the original bone limits,
- the draw hook calls the original renderer without rebuilding.

`hooksInstalled()` becomes true only after all four succeed.

The module hooks `PackSkinnedVB` at entry via a trampoline, not at known call sites. A
missed call site would reach the original packer with `numBones > 4`
and overwrite the buffer.

### 4.2 Partition rebuilding

On an intercepted skinned draw, with the feature authorized:

- **Null partition.** Leave it; the engine's lazy creation path handles it.
- **Dense partition.** If `partitions[0].bonePalette == nullptr`, release through normal
  `NiPointer` refcounting, null the engine's pointer, and let it rebuild.
- **Palette partition.** Rebuild if required arrays are missing, `numBones == 0`,
  `numBonesPerVertex != 4`, or if it has more bones than the palette.
- **Compatible embedded palette partition.** Keep as-is.

Lazy render-thread rebuilding preserves the engine's lifecycle and avoids loader/render
concurrency.

If indexed partitioning produces no usable result, the module rebuilds with the original
arguments. When that stock result is usable, it records the partition as a stock fallback
so the module does not retry it each frame. Each fallback-cache entry holds an engine
reference, not a bare pointer. Otherwise an unrelated allocation could recycle a released
partition's address, and the cache could misread it as a previous fallback. A failed stock
result is not cached. `onDeviceReleased()` clears the cache while the Morrowind runtime is
alive, so a static destructor does not release engine references during teardown.

## 5. Capability handshake

A standard Direct3D cap is not a sufficient authorization token. An *unpatched* MGE
passes underlying caps straight through, so it could advertise a custom DXVK's larger
palette while its own shaders still ignore bone indices. The hook module queries a small
versioned private COM interface from the D3D8 device instead:

```cpp
struct MgeIndexedSkinningCaps {
    std::uint32_t structVersion;
    std::uint32_t maxPaletteBones;
};

struct IMgeIndexedSkinningCaps : IUnknown {
    virtual HRESULT GetIndexedSkinningCaps(MgeIndexedSkinningCaps* caps) = 0;
};
```

`MGEProxyDevice::QueryInterface` recognizes the private GUID before forwarding unknown
GUIDs onward. `GetIndexedSkinningCaps` is the feature's single composite authorization
point and reports a zero palette unless *all* of:

- every engine hook installed successfully,
- `render.indexed_skinning` is true,
- the generated fixed-function effect exposes an 8-matrix `vertexBlendPalette`, and
  `XE Main.fx` and `XE Depth.fx` expose the indexed shadow and depth passes,
- the underlying device reports enough indexed matrices.

In the normal MGE rendering mode, all four conditions are required. MGE-disabled and
proxy-only modes intentionally skip the MGE shader checks because they do not use the
replacement shader path; those modes still require installed hooks, the setting, and
enough device matrices.

When authorized it reports
`min(MGE_INDEXED_SKINNING_PALETTE_SIZE, underlyingCaps.MaxVertexBlendMatrixIndex + 1)`.

The hook module negotiates once per distinct device pointer and requires the exact
structure version and at least `MGE_INDEXED_SKINNING_PALETTE_SIZE` matrices; it never
negotiates down to a smaller indexed mode. Hooks install from the `MGEProxyDevice`
constructor rather than at first `Present`, because Morrowind can issue draws before
presenting a frame. Device recreation renegotiates but never re-patches. The executable
patches are process-lifetime.

The DXVK fork (`Greatness7/dxvk`) does not participate in `IMgeIndexedSkinningCaps`. It
supplies the ordinary D3D9 contract instead: `D3D9Adapter::GetDeviceCaps` reports
`MaxVertexBlendMatrices = 4` and `MaxVertexBlendMatrixIndex = 7`, while its indexed
fixed-function vertex shader selects matrices from `in_BlendIndices`. The private
`IDxvkMorrowindPplInterop1` interface is for native per-pixel-lighting packets and is not
the indexed-skinning handshake.

## 6. MGE XE render path

`MGEProxyDevice::DrawIndexedPrimitive` identifies indexed draws from the FVF. The FVF
is the authoritative layout signal, and `RenderedState` already records it:

```cpp
const bool indexedSkinning =
    rs.vertexBlendState != D3DVBF_DISABLE &&
    (rs.fvf & D3DFVF_LASTBETA_UBYTE4) != 0;
```

Indexedness is part of `FixedFunctionShader::ShaderKey`; without it, indexed and
non-indexed draws would reuse the same cached shader.

- **Passthrough.** When PPL/FFE is inactive, MGE XE sets
  `D3DRS_INDEXEDVERTEXBLENDENABLE` true around the draw and sets it false afterward,
  keeping the state out of Morrowind's cache.
- **Palette capture.** A separate skin-palette state holds `N` world and worldview
  matrices outside `RenderedState`. `captureTransform` updates it for every tracked
  `D3DTS_WORLDMATRIX` slot, and each recorded indexed draw copies all `N` worldview
  matrices into the per-frame palette arena.
- **Shaders.** The skinned vertex input carries `BLENDWEIGHT`/`BLENDINDICES`, and the
  palette is a shared effect-pool parameter `matrix vertexBlendPalette[N]`. It stays a
  full projective matrix (not `float4x3`) because the same shared parameter also carries
  projective shadow-debug matrices that need `w`. The build must compile the shared effect
  pool together because shared parameters need identical type and size everywhere.
- **Depth and shadow replay.** Records carry `skinPaletteOffset`/`skinPaletteCount` into
  a per-frame matrix arena, which MGE resets with `recordMW`. MGE cannot reliably infer
  the active palette count from the undelimited `SetTransform` stream, so it copies a fixed
  `N` matrices per skinned draw. `XE Depth.fx` gains indexed weighted position;
  `XE Mod Shadow.fx` gains indexed weighted position and normal. `XE Main.fx`'s use of
  `vertexBlendPalette[0..1]` for `shadowToCameraProj` is unrelated to actor skinning, and
  MGE XE preserves it.

Before authorizing indexed partitions, MGE XE reflects the compiled core effects and
requires an `N`-element matrix palette plus the indexed color, depth, and shadow passes.
Skinned draws that arrive before effect initialization receive a pending capability result
and retry later. A stale or mod-overridden core shader therefore leaves the stock partition
path active instead of allowing the DLL and replay shaders to disagree.

The vertex packer also sanitizes non-finite weights and palette indices outside the current
partition. This is done once when the vertex buffer is created, not in any per-frame draw
path, and prevents malformed embedded `NiSkinPartition` data from indexing beyond the
matrix palette.

## 7. Component ownership

| Component | Responsibility |
|---|---|
| MGE XE `d3d8.dll` | Engine hooks, indexed vertex packer, capability interface, indexed FFE/PPL/depth/shadow paths |
| Custom DXVK `d3d9.dll` | Eight hardware matrix transforms; reports `MaxVertexBlendMatrixIndex = 7` while keeping `MaxVertexBlendMatrices = 4`; existing indexed fixed-function shader |
| MWSE | **None.** |

The engine hooks and packer were originally prototyped in a fork of MWSE
(`SharedSE/NIDX8Renderer.*`, `SharedSE/NISkinInstance.*`, and the
`MWSE_INDEXED_SKINNING` block of `MWSE/PatchUtil.cpp`, all GPLv2). Keeping them there
would have made upstream MWSE depend on this specific MGE XE and DXVK stack, so ownership
moved into `d3d8.dll`. MGE XE adapted its implementation from that prototype.

Consequences worth knowing:

- The feature works with no MWSE installed, and a clean upstream MWSE changes neither
  behavior nor performance. Pre-feature MWSE does not touch any of the four patch sites.
- An old development MWSE build carrying the feature is incompatible. The checked
  installation in §4.1 detects and logs its altered call targets and prologue, then leaves
  the MGE XE-owned feature disabled. MGE XE does not support simultaneous ownership.
- Two implementation details differ from the prototype because MGE XE's environment
  differs. The packer uses D3D9 `Lock`/`Unlock`/`Release`; MGE XE does not wrap vertex
  buffers. `d3d8header.h` defines `IDirect3DVertexBuffer8` as `void`, and
  `ProxyDevice::CreateVertexBuffer` forwards the real `IDirect3DVertexBuffer9` straight
  to Morrowind. MGE XE uses index loops instead of `std::span` because the `d3d8` C++
  builds at C++17.

## 8. Configuration and rollback

```toml
[render]
indexed_skinning = true
```

Default `false`; the block above opts in. MGE XE reads it once at process start;
changing it takes effect only after a full game restart and never applies to existing
partitions. It is a standalone
`bool` in `ConfigurationStruct` rather than an `MGEFlags` bit, because that legacy
bitfield is full. See [mge-toml.md](../configuration/mge-toml.md).

Four independent gates in normal MGE rendering leave the stock `(4, 4)` path intact:

1. **Hook installation failure.** Every installed wrapper delegates stock behavior.
2. **Shader compatibility failure.** In normal MGE rendering, stale or overridden core
   effects keep the feature off; MGE-disabled and proxy-only modes skip this MGE shader gate.
3. **Capability failure.** The module creates no indexed partition.
4. **`render.indexed_skinning = false`.** The feature stays disabled for the process
   lifetime.

MGE XE does not support disabling the feature *after* it builds indexed partitions;
doing so would require converting them back to `(4, 4)`. This is why the setting is
restart-only.

The custom DXVK `d3d9.dll` can stay installed while the feature is off. MGE XE can keep
its indexed shader code compiled in; without indexed partitions it is dormant.

## 9. Verification

Static checks:

- Engine layouts pass their size assertions on i686.
- Every hook verifies its expected original bytes or CALL target.
- The hook installer is one-shot; partial installation cannot authorize indexed
  partitions.
- Palette constants agree between MGE XE's shaders, MGE XE's C++, and DXVK.

Smallest build checks that cover the change:

```sh
cargo check -p d3d8 --target i686-pc-windows-msvc
cargo test -p mge-config
cargo run -p config-contract-test --target i686-pc-windows-msvc
```

The contract test cross-checks every C++ `iniSettings[]` row against the Rust schema for
path, storage width, and default, and asserts the total binding count. Adding or removing
a setting requires an explicit count update.

Runtime matrix. Indexed skinning's acceptance criteria are almost all runtime-observable,
so compile checks cannot substitute for these tests:

1. MGE XE + custom DXVK, no MWSE installed.
2. The same, plus clean upstream MWSE.
3. PPL off, then PPL on.
4. Depth and actor shadows.
5. `render.indexed_skinning = false`.
6. An unmodified/non-capable D3D9 implementation.
7. Fullscreen alt-tab and renderer/device recreation.
8. One normal humanoid, one hand-heavy model, one full-body replacer, one creature.
9. An incompatible old development MWSE build, to confirm conflict detection.

Record per scene: frame time, main-render geometry correctness, depth and shadow agreement,
and the capability/hook-install log lines. The runtime logs unusable indexed partitions
that fall back to stock rebuilding, but it does not expose per-scene partition-draw or
rebuild counters; those require external instrumentation or a profiler.

Release criteria: partition draws drop consistently with the tested build; no weight or
index corruption; main, depth, and shadow passes agree visually; an unmatched stack never
enters indexed partition mode.

## 10. Non-goals

- Merging distinct body-part, armor, clothing, texture, or material geometries.
- Replacing Morrowind's partition builder.
- Moving the optimization solely into DXVK.
- Sharing a source package between MGE XE and MWSE.
- Generalizing a full detour framework before another MGE XE feature needs one.
- Supporting simultaneous ownership by a patched MWSE and MGE XE.
