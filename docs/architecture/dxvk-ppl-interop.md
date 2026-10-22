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
- `mged3d8device.cpp`: separate `CAP_EXPANDED_LIGHT_LIMIT` and `CAP_SOFT_LIGHT_RANGE` probe
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

`DxvkMorrowindPplDrawV3` does match its version, 3. It embeds the version 2 struct as
`base` and is still submitted through `DrawPplV1(&packet.base)`.

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
- `CAP_PPL_DRAW_V3` = 1<<4, `DrawPplV1` also accepts the version 3 packet
- `CAP_SOFT_LIGHT_RANGE` = 1<<5, the ordinary path fades over the last quarter of `Range`

Failure paths all degrade silently to the legacy D3DX path, which is correct behavior,
not a bug. However, this means "native PPL quietly stopped working" looks identical to
"native PPL was never enabled". Stock DXVK and real D3D9 return `E_NOINTERFACE`; a fork
predating `CAP_PPL_DRAW_V2` is released and nulled at `ffeshader.cpp:151-158`.

## Non-obvious packet semantics

Sizes are pinned by `static_assert` in the shared header: stage 32 bytes, draw 1956,
version 3 draw 2084. DXVK's internal `D3D9MorrowindPplData` is 2048: the version 3
payload minus the 36-byte draw header, which DXVK consumes rather than uploads. A
version 2 packet uploads with the fade array zeroed.

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

## Light fade (version 3)

Version 3 appends `lightFadeInvRadius[32]`, the reciprocal of each point light's cutoff
distance `R`. Attenuation is multiplied by `(1 - x^2)^2` with
`x = saturate(4 * d / R - 3)`, so a light fades out between `0.75R` and `R`, the
curve OpenMW uses. A value of 0 gives `x = 0` and leaves the light unchanged, which is
how a version 2 packet renders.

Morrowind submits every light with an effectively infinite `Range`, so the radius is
recovered from the attenuation instead (`MWBridge::pointLightRadius`, at `SetLight`
capture). `R` is `distant_land.per_pixel_light_fade_radius` times that radius, floored at
16. A radius that cannot be recovered comes back as 0 and that light does not fade. The
recovery inverts `EntityLight::createLightOnReference` for record lights and the fixed
`10 / r^2` of `MobileObject::setLightEffectFalloff`, which is every spell and projectile light
and, through `EntityLight::setupInternalLight`, every light an actor carries; the record form
is tried first. With no INI constant and a quadratic `[LightAttenuation]` term the two produce
the same shape (constant 0, linear 0, quadratic positive). Read as a record light, a body
light comes out `sqrt(quadraticValue / 10)` times too long under method 2 and
`quadraticValue * r / 10` times under method 1, which puts a torch at the one-cell cap:
attached to every node in reach and first in each node's list, which the engine sorts by
distance less radius. Both derivations start from an integer radius, so the recovery keeps
whichever reading comes back as one. That settles most radii, and none at all for a
`quadraticValue` of 10; when both readings are integers the record one stands, since a long
reading costs slots but changes no pixel and a short one would shrink a record light. A
record light is never taken for a body light. The stock INI and the one MGE recommends have
no ambiguity to begin with. MGE sends version 3 only when `distant_land.per_pixel_light_fade` is on and the
renderer reports `CAP_PPL_DRAW_V3`; otherwise the version 2 prefix goes out alone. The
legacy effect takes the same values as `lightFadeInvRadius` in `XE FixedFuncEmu.fx`.

The fade alone does not stop pop-in. `game_dynamicLightTest` (0x4D2F40) attaches a light to
an object only when some geometry bound comes within the light's record radius, and an actor's
lights, or a light it carries, are retested only after it moves 64 units
(`MobileObject::updateDynamicLightSource`, 0x5616A0). Anything between that radius and `R` would
gain or lose a still visible light at a retest. `MWPatches::patchLightAttachRadius` therefore
retargets the six calls that pass a record radius into the attach test
(`game_updateDynamicLightingForPointLight` and `game_updateLightHelper2` callers) so lights attach
at `R + 160`: the fade radius, two 64-unit retest steps for a moving actor and a moving light,
and a frame of fast movement. An unattached light then contributes nothing, so attaching or
detaching it is invisible. OpenMW gets the same property by testing the fade radius every frame.
The test stores the radius it used in the light's specular colour, and the retest in
`DataHandler::updateDynamicLightingForReference` reads it back, so that call needs no patch. The
wider radius is only used while every renderer drawing lit objects fades
(`FixedFunctionShader::lightAttachRadius`), and only for lights whose radius MGE can recover.
More lights reach each object, so the per-node limit matters more; `expanded_light_limit` is
recommended.

That test runs per attachment, but an attachment outlives it. The engine holds one until the
object moves 64 units, and a static never moves, so turning per-pixel lighting off at runtime
(`MGEAPI::lightingModeSet`, `MacroFunctions::ToggleLightingMode`) leaves lights attached out to
`R + 160` on draws the per-pixel shaders no longer take.

`MGEProxyDevice::uploadLight` covers those. It writes the same cutoff `R` into `D3DLIGHT8::Range`
for every point light it forwards, and the fork's fixed-function vertex shader fades over the last
quarter of `Range` rather than stepping at it, so a widened attachment is faded on the ordinary
path too and the two paths agree on where a light ends. That holds wherever the ordinary path
draws, so under interiors-only lighting an exterior light also ends at `R`. Its attachment is
left at the record radius, and the fade has not begun there, so the engine's own attach
boundary looks as it always did.

Range reaches only the ordinary path. The native packet is self-contained by design and never
reads device light state, which is why the fade also has to travel in the packet.

The Range side is gated on `CAP_SOFT_LIGHT_RANGE`. D3D9 specifies a step at `Range`, and the
step is reachable: the engine attaches by an object's bounds, so a room shell or a terrain
patch attached at the record radius has vertices well past `R`. A renderer without the soft
falloff, native D3D9 included, would draw a step of roughly a fifth of the light's strength
across them, which is worse than the unfaded light it replaces. There MGE leaves `Range` alone
and logs that it did, and a runtime switch away from per-pixel lighting leaves the widened
attachments drawn unfaded until the cell reloads: dimmer than at the record radius, and with
smaller steps than the engine's own.

What survives is slot pressure. A light faded to zero still occupies one of the node's effect
slots, so a wider attach radius costs slots whether or not the light contributes. That is the
other reason `expanded_light_limit` belongs with this.

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
