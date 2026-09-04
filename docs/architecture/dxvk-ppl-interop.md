# DXVK Morrowind interop

The private COM ABI between MGE XE and its DXVK fork (`Greatness7/dxvk`, branch `mge-xe`):
native per-pixel lighting, MSAA depth resolve, and the device-local memory budget used by
merged-static streaming. Compatibility rule for the whole surface: a new service gets its own
versioned IID, never a method appended to an existing interface.

## The contract

Two headers are shared source, copied into both trees:

```
d3d8/cpp/mge/dxvk_morrowind_interop.h   <->  dxvk/src/d3d9/dxvk_morrowind_interop.h
d3d8/cpp/mge/dxvk_morrowind_limits.h    <->  dxvk/src/d3d9/dxvk_morrowind_limits.h
```

They must stay byte-identical. Nothing in either build checks this. Compare SHA-256 hashes after
every edit; a mismatch is an ABI deployment error even when both repositories compile.

## File map

- `ffeshader.cpp:141-158`: acquires the interop, gates on `CAP_PPL_DRAW_V2`
- `ffeshader.cpp:~550`: `encodeNativePplKey`, the producer-side rejection filter
- `ffeshader.cpp:691`: `DrawPplV1`, the actual handoff
- `ffeshader.cpp:730`: branch between native and legacy
- `ffeshader.cpp:1193`: release on teardown
- `mged3d8device.cpp:88-106`: separate `CAP_EXPANDED_LIGHT_LIMIT` probe
- `distantinit.cpp`: memory-budget query, cap selection, and cap resampling
- DXVK `src/d3d9/d3d9_interop.cpp`: `DrawPplV1` entry, size/version rejection
- DXVK `src/d3d9/d3d9_device.cpp`: `ValidateMorrowindPpl`, `DrawMorrowindPpl`
- DXVK `src/dxvk/dxvk_memory.cpp`: locked global-buffer-heap budget snapshot
- DXVK `src/d3d9/shaders/d3d9_morrowind_ppl_common.glsl`: the UBO the packet becomes

## Naming trap

The struct is `DxvkMorrowindPplDrawV1`, but `DXVK_MORROWIND_PPL_STRUCT_VERSION` is 2
and the required capability bit is `CAP_PPL_DRAW_V2`. The `V1` in the type name is
frozen history, not the current version. `CAP_PPL_DRAW_V1` still exists as a bit and is
*not* what MGE XE asks for.

## Negotiation

`QueryInterface` on the D3D9 device for `IDxvkMorrowindPplInterop1`
(`275c3348-5724-4a7e-aac0-46ceda965739`). Distant land separately queries
`IDxvkMorrowindInterop` (`2ff12bfc-4622-4d9d-bcbf-1501f37e8aa3`) for MSAA depth resolve.

Merged-static streaming queries the independent `IDxvkMorrowindMemoryInterop1`
(`2866403d-8842-4bde-81f7-db4aa81f2d2d`). `GetDeviceLocalMemoryBudgetV1` returns two byte counts:
the allocator-policy-adjusted `memoryBudget` and live allocator `memoryUsed` for the heap selected
by DXVK's global-buffer memory-type mask with `VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT`. The allocator
takes its memory mutex for one coherent snapshot. These are DXVK policy values, not raw Vulkan heap
numbers for MGE to reinterpret.

The memory interface is separate because adding a method to either older interface would change
its vtable for existing clients. It has no capability bit and does not change
`DXVK_MORROWIND_INTEROP_VERSION`: successful `QueryInterface` plus a successful method call is the
negotiation. Stock/native D3D9, an older fork, a missing device-local buffer heap, or a zero budget
falls back to infinite-cap ordered full residency. How that budget becomes a streaming cap is in
[distant-static-residency.md](distant-static-residency.md).

Capability bits (`dxvk_morrowind_interop.h:16-24`):

- `CAP_MSAA_DEPTH_RESOLVE` = 1<<0
- `CAP_PPL_DRAW_V1` = 1<<1
- `CAP_PPL_DRAW_V2` = 1<<2, the bit native PPL requires
- `CAP_EXPANDED_LIGHT_LIMIT` = 1<<3

Failure paths all degrade silently to the legacy D3DX path, which is correct behavior,
not a bug. However, this means "native PPL quietly stopped working" looks identical to
"native PPL was never enabled". Stock DXVK and real D3D9 return `E_NOINTERFACE`; a fork
predating `CAP_PPL_DRAW_V2` is released and nulled at `ffeshader.cpp:151-158`.

## Non-obvious packet semantics

Sizes are pinned by `static_assert` in the shared header: stage 32 bytes, draw 1956.
DXVK's internal `D3D9MorrowindPplData` is 1920, the same payload minus the 36-byte
draw header, which DXVK consumes rather than uploads.

What you cannot infer from the field names:

- `sunDirection`, `lightPosition` are view space, not world space.
- `lightPosition` is structure-of-arrays: `[3][32]`, i.e. 32 X then 32 Y then 32 Z.
  Every other light array is array-of-structs. Getting this wrong produces plausible
  but wrong lighting rather than an obvious failure.
- `sceneAmbient` and `sunDiffuse` arrive pre-scaled by MGE's ambient/sun multipliers.
- `lightFalloffConstant` is a single global, not per-light; only the quadratic term is
  per-light, because that is how Morrowind's own falloff model is shaped.
- `lightSlotCount` must be exactly the packed count and must be 0 for unlit draws.
- Unused `stages[]` entries must be zeroed; DXVK rejects the draw otherwise.
- `reserved0` and `reserved1[2]` must be 0.

## What falls back to legacy

`encodeNativePplKey` rejects, producer-side, before any call: indexed skinning
(`source.indexedSkinning`), more than `DXVK_MORROWIND_PPL_MAX_STAGES` (6) stages, more
than 4 UV sets, more than 4 total output texcoords, and any texture op or argument
outside its whitelist. DXVK rejects further at draw time: bound programmable shaders,
enabled user clip planes, non-2D texture types.

Indexed skinning is the significant case: those draws keep using `ID3DXEffect` with the
matrix palette, so the skinning path and the native path are permanently disjoint.

## The expanded light limit interlock

`MWPatches::patchExpandedLightLimit()` raises Morrowind's per-node light cap from 7 to 32
by patching `NiNode::PushLocalEffects`. The patch is irreversible for the process;
the lighting mode is not (`ToggleLightingMode`, `MGEAPI::lightingModeSet`, stepping
outdoors from an interior all change it at runtime).

That asymmetry is why this has its own capability bit instead of riding on
`CAP_PPL_DRAW_V2`: after the patch, *every* reachable path must handle 32 lights,
including ordinary fixed-function, not just native packets. DXVK enforces its half with
`static_assert(DXVK_D3D9_MAX_ENABLED_LIGHTS == DXVK_MORROWIND_PPL_MAX_LIGHTS)`. The
legacy MGE path stays at `MGE_LEGACY_PPL_MAX_LIGHTS` = 8 (`ffeshader.h:16`).

## What breaks silently

Caught: any size change (`static_assert` on both sides, plus runtime `structSize` and
`structVersion` rejection); patching the engine without renderer support (the
`CAP_EXPANDED_LIGHT_LIMIT` probe).

Not caught by anything:

- Reordering two same-sized fields, or changing a `DxvkMorrowindPplFlags` bit's meaning.
  Size is unchanged, both `static_assert`s pass, rendering is quietly wrong.
- Editing DXVK's `D3D9MorrowindPplData` without editing `d3d9_morrowind_ppl_common.glsl`
  (or vice versa). There is no offset assertion between the C++ struct and the GLSL UBO;
  shaders still compile and sample garbage.
- Editing one repo's copy of a shared header and not the other. Only the hash check
  finds this.
