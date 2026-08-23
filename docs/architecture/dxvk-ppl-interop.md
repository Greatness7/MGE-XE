# DXVK native per-pixel-lighting interop

Scope: the ABI MGE XE uses to hand fixed-function draws straight to our DXVK fork,
bypassing D3DX. Cross-repo contract: both sides must move together.
State: shipping and in sync. `dxvk_morrowind_interop.h` and `dxvk_morrowind_limits.h`
are byte-identical in both repos (verified by hash, not by eye).
Next action: none. Re-verify the header hashes whenever either side changes.
Inspected 2026-08-18:
  MGE-XE  `rust-rewrite`
  DXVK    `Greatness7/dxvk` branch `mge-xe`, tag `v3.0.2-mge1`

## The contract

Two headers are shared source, copied into both trees:

```
d3d8/cpp/mge/dxvk_morrowind_interop.h   <->  dxvk/src/d3d9/dxvk_morrowind_interop.h
d3d8/cpp/mge/dxvk_morrowind_limits.h    <->  dxvk/src/d3d9/dxvk_morrowind_limits.h
```

They must stay byte-identical. Nothing in either build checks this. The check is
`md5sum` on both paths, and it is the first thing to run when native PPL misbehaves.

## File map

- `ffeshader.cpp:141-158`: acquires the interop, gates on `CAP_PPL_DRAW_V2`
- `ffeshader.cpp:~550`: `encodeNativePplKey`, the producer-side rejection filter
- `ffeshader.cpp:691`: `DrawPplV1`, the actual handoff
- `ffeshader.cpp:730`: branch between native and legacy
- `ffeshader.cpp:1193`: release on teardown
- `mged3d8device.cpp:88-106`: separate `CAP_EXPANDED_LIGHT_LIMIT` probe
- DXVK `src/d3d9/d3d9_interop.cpp`: `DrawPplV1` entry, size/version rejection
- DXVK `src/d3d9/d3d9_device.cpp`: `ValidateMorrowindPpl`, `DrawMorrowindPpl`
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
