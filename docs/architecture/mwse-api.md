# MWSE API contract

Scope: the versioned C++ vtable and script opcodes MGE XE exposes to MWSE. Cross-repo,
cross-binary; MWSE ships on its own schedule against a compiled copy of this interface.
State: both sides at API version 3, vtables in sync.
Next action: none. Treat `api.h` as frozen-by-slot; see "Append-only" below.
Inspected 2026-08-18:
  MGE-XE  `rust-rewrite`
  MWSE    `MWSE/MWSE` @ 54c3041

## Direction

MGE XE is the provider of the API *object* but the consumer of MWSE's *exports*. MWSE
exports `MGEInterface`, `MWSEGetVM`, `MWSEAddInstruction` (`MWSE/main.cpp:194-199`,
`extern "C"`, cdecl). MGE XE `GetProcAddress`es all three off MWSE.dll, allocates
`new api::MGEAPI_ExportVersion()` and pushes it in (`mwse/mgebridge.cpp:71-73`).

MGE XE loads MWSE.dll itself from `main.cpp:66`, before distant land exists.

## File map

- `mge/api.h`: the live interface, namespace `api`, `supported_api_version = 3`
- `mge/api.cpp`: implementations; `getAPIVersion` / `getMGEVersion` at `:60-66`
- `mwse/mgebridge.cpp`: export resolution, opcode registration, 0.9.4a fallback
- `mwse/mwseinstruction.h`: instruction base; hardcoded TES3MACHINE vtable offsets
- MWSE `MWSE/MGEApi.h`: MWSE's copy of the interface, namespace `mge`
- MWSE `MWSE/main.cpp:201-210`: `MGEInterface`, the version gate
- MWSE `MWSE/MGEApiLua.cpp:25`: the only consumer of `getMGEVersion`

## Version negotiation

Two version numbers exist and they do different jobs. Only one is a compatibility gate.

`getAPIVersion()` returns `api::supported_api_version` (3). This is the gate. MWSE
accepts anything `>= 1` and stores it as `mge::apiVersion`, then feature-gates on it
(`MGEUtilLua.cpp:77` and `:180` for v2, `:156` for v3).

`getMGEVersion()` returns `MGE_MWSE_VERSION`. MWSE never compares it. Its only use is
display, in `mge.getVersion()` for Lua.

That display path is a real interlock. MWSE computes:

```cpp
int ver = api->getMGEVersion() - 0x40000;   // MGEApiLua.cpp:25
```

The `0x40000` hardcodes `MGE_MAJOR_VERSION == 4` in MWSE's source. Bumping
`MGE_MAJOR_VERSION` in `mgeversion.h` does not break the API. It silently shifts the
version MWSE reports to users by one major. That, not a comparison, is the reason to
leave `MGE_MAJOR_VERSION` alone.

There is no hard failure keyed to the vtable API. MWSE's only hard gate is a file check
on `MGEXEgui.exe`'s version resource (`main.cpp:77-93`): missing or below 0.10.0.0 ->
message box and `exit(0)`.

## Append-only

`MGEAPI` (2 methods) <- `MGEAPIv1` <- `MGEAPIv2` <- `MGEAPIv3`, with
`typedef MGEAPIv3 MGEAPI_ExportVersion` (`api.h:228`). Each version subclasses the last
and adds only tail slots, so layout is append-only by construction:

- v2 adds `saveScreenshot`, `weatherScatteringSkylightGet/Set`
- v3 adds `nearRenderDistanceGet/Set` and six `shaderGet/Set{Int,Bool,Vector}Array`

Inserting, reordering, or changing the signature of any existing virtual shifts vtable
slots against MWSE's already-compiled copy. Nothing catches it at MGE build time; it
fails as a wrong-function call in a user's game.

## One live declaration

`api.h` is the only interface declaration in the tree. A second, older `mge/MGEAPI.h`
(namespace `mge`, `supported_api_version = 1`, a different v1 ABI) survived unreferenced
until it was deleted in 2026-08. If a stale copy resurfaces, it is not the interface.

## Opcodes

102 registrations in `mwse/mgebridge.cpp:85-196`. The range is not contiguous and is
wider than the `0x3Axx` block: `0x3700-0x3777`, `0x3A00-0x3A8D`, `0x3AB0-0x3AB4`,
`0x3AC0-0x3AC2`, `0x3AE0-0x3AEB`, plus a single outlier `0x3f11` (`mwseSetOwner`).

By handler unit: gmst `0x3A00-01`; entity `0x3A02-09` + `0x3f11`; mwui `0x3A10-11`;
weather `0x3A80-8D`; general `0x3700-05,07`; hud `0x370A-1E` + `0x3AC0-C2`; input
`0x3733-3B`; camera `0x3756-5A`, `0x3768-77`; shader `0x3AB0-B4`; physics `0x3AE0-EB`.

Opcode numbers are a hard compatibility surface. They are the integers baked into
compiled Morrowind script bytecode and, through the save system, into savegames.
Renumbering breaks installed mods and existing saves. Nothing checks this.

## MWSE 0.9.4a fallback is live code

If `MWSEGetVM` and `MWSEAddInstruction` are absent, MGE XE assumes MWSE 0.9.4a and
steals the VM global at `dll+0x595cc` and `AddInstruction` at `dll+0x38950`
(`mgebridge.cpp:74-75`), substitutes a thiscall shim, then runs `fixMWSE94Problems`,
which reinterprets a breakpoint array at `dll+0x56900` and re-protects nine patch pages
(`mgebridge.cpp:36-54`).

This is reachable in current builds. It triggers on which MWSE is installed, not on a
build flag. `mwseinstruction.h` additionally carries hardcoded TES3MACHINE vtable offsets
and a target-ref global at `0x7CEBEC`.

## What breaks silently

Every hazard here is an MGE-XE edit read by MWSE's precompiled header, so none of it can
be caught at MGE build time:

- Insert/reorder/re-sign any virtual in v1/v2/v3 -> vtable slot drift.
- Change `DistantLandRenderConfig` layout -> MWSE reads wrong offsets
  (`MGEUtilLua.cpp:44-62` binds by member). MGE's own `static_assert` at `api.cpp:173`
  checks parity with `Configuration.DL` only, not with MWSE.
- Insert into the middle of `RenderFeature` -> the enum indexes `featureToFlagMap`
  positionally (`api.cpp:100-123`); every later feature toggle changes meaning.
- Reorder/remove `MacroFunctions` members -> MWSE binds them by offset.
- Bump `supported_api_version` without adding a real `vN` subclass. MWSE's
  `MGEPostShaders.cpp` static_casts to `MGEAPIv3` *unconditionally* at `:187`, `:202`,
  `:290` with no `apiVersion` check. Safe today only because MGE always constructs a
  full v3.

The reverse direction, MGE newer than MWSE, is safe because v3 is a superset and older
MWSE never touches the new slots. There is no cross-binary layout test anywhere.

## Writable fields that do nothing

`getDistantLandRenderConfig()` hands out a raw pointer to the whole `Configuration.DL`
struct, so every field in it looks writable from Lua. Some are only read during init and
ignore later writes.

`ShadowResolution` is the known case. `DistantLand::initShadow` reads it once to size
`texShadow`, `texSoftShadow`, and `surfShadowZ`, and `ehShadowRcpRes` is pushed once in
`initShader`. Writing it after startup changes neither, so the setting takes effect on
the next renderer restart. See [shadows.md](shadows.md).

Narrowing the exposed struct would fix the whole class, but it is an ABI change MWSE
reads by member offset, so it is not a local edit.
