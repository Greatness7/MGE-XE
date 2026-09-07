# Morrowind engine memory and patches

How MGE XE reaches into Morrowind's process: named engine struct layouts, fixed-address
executable patches, and the NetImmerse DDS hooks. Every address below is for `Morrowind.exe`
FileVersion 1.6.1820 at imagebase `0x400000`, and was verified against an IDA database of that
binary.

## File map

| Path | Ownership |
| --- | --- |
| `d3d8/cpp/tes3/tes3types.h` | `TES3::` engine struct layouts |
| `d3d8/cpp/tes3/nitypes.h` | `NI::` NetImmerse layouts, including the owning `NI::Pointer<T>` |
| `d3d8/cpp/tes3/tes3addresses.h` | Every absolute address, grouped by kind |
| `d3d8/cpp/mge/mwbridge.*` | Engine-state access, node tagging, loading-bar calls |
| `d3d8/cpp/mge/mwpatches.*` | Fixed-address executable patches and their callbacks/trampolines |
| `d3d8/cpp/mge/mwtextureloader.*` | NetImmerse DDS/BC7 hooks and staging-to-default-pool texture upload |

## Where addresses live

Absolute addresses are `inline constexpr uintptr_t` constants in `tes3addresses.h`, in four
sections: globals, statics and virtual tables, code patch sites, and functions. Nothing outside
that header should contain an address literal.

Everything else is structure layout, expressed as real C++ structs in `tes3types.h` and
`nitypes.h` rather than pointer arithmetic. Naming authority is MWSE (`C:\Users\Admin\Projects\MWSE\`,
`MWSE/TES3*.h` and `SharedSE/NI*.h`); only the members MGE touches are named, and the rest is
explicit padding sized so the assertions stay honest.

Every struct carries `static_assert(sizeof(...))` and `static_assert(offsetof(...))`. **Those
assertions are the transcription test** — they are what catches a mis-sized padding array at
compile time, so a new member always arrives with one. They prove the header is self-consistent,
not that it matches the engine; that still takes the IDB.

`NI::Pointer<T>` is an owning intrusive-refcount handle, not a view. Its ownership is
load-bearing in `morrowindskinning.cpp`, where assigning `nullptr` to a skin partition is what
releases it back to the engine. `NI::PickRecord` and `TES3::Game::worldRoot` deliberately use
raw pointers where MWSE uses `Pointer<T>`, because MGE never owns pick results or the scene, and
`funcphysics.cpp` copies a whole `PickRecord` by value into a static on every raycast.

### Checking a new address

With the IDB open and `ida-pro-mcp` running: `lookup_funcs` a code address (it returns the
containing function), `idc.get_name` via `py_eval` for a data address. To confirm an IDB is the
right binary at all, `get_bytes` at `0x6C8FF0` must read `83 FD 07 77 0A`, the `expected[]` array
in `MWPatches::patchExpandedLightLimit`.

## Two-phase patch install

**Phase 1, pre-device.** `MWInitPatch::patch()` runs during DLL attach, before any D3D device
exists. It may only touch static addresses, since nothing has been allocated yet: UI-scale patch
(only when MGE is disabled or in proxy-only mode), intro-movie skip,
`MWTextureLoader::patchLoadTexture2D`, `MWPatches::patchFrameTimer`, and
`MWPatches::patchLightParticleMaterialModifier`.

**Phase 2, first usable Present.** `MGEProxyDevice::Present` tests `!IsLoaded() && CanLoad()`,
then calls `Load()` followed by `MWPatches::patchGameLoading`,
`MWPatches::patchWorldRenderingAccumulation`, `MWPatches::disableScreenshotFunc`,
`MWBridge::markWaterNode(99999.0f)`, conditionally `MWPatches::patchExpandedLightLimit`, and
finally `DistantLand::init()`.

`CanLoad()` is `TES3::DataHandler::get() != nullptr` — the readiness test, safe to call before
the game has loaded anything because it reads a static address. `IsLoaded()` is a separate
one-shot latch that `Load()` sets. **They must not be collapsed into the same predicate**: the
phase-2 edge above fires exactly once, on the frame where `CanLoad()` first becomes true and the
latch is still clear. Making `IsLoaded()` a live probe of the same pointer makes that edge
unreachable and distant land never initialises.

`MWPatches::patchSplashScreen` is separate. It runs from device creation because it needs the
viewport size. The texture hooks install in phase 1, while device creation later calls
`MWTextureLoader::setBC7TextureSupport` before textures load.

## Notes

**No version check, and that is fine.** `CanLoad()` tests a pointer for non-null; it does not
identify the binary. Against a game that has not shipped a patch in twenty years and never will,
a version gate would guard a condition that cannot occur.

**Only one executable installer validates before writing.**
`MWPatches::patchExpandedLightLimit` compares against an expected signature first and detects the
already-patched case. The other patch writes are blind. Since the target bytes are fixed, this
catches our mistakes, such as a mistyped address or a double-apply, rather than a foreign binary.
Worth copying only if a patch is conditional or can run twice.

**Scenegraph tagging by magic value.** Water is marked by material `Power == 99999`
(`MWBridge::markWaterNode`) and moons by emissive alpha `88888` (`MWBridge::markMoonNodes`),
which the render path recognizes. Sentinel values in float fields, not flags. A mod setting the
same value would be misclassified.

**Two script opcodes dereference an unchecked target.** `mwseModelBounds` and `mwseTransformVec`
in `d3d8/cpp/mwse/funcphysics.cpp` use `vmGetTargetRef()` without a null check, where `RayTest`
in the same file guards. Unreachable from well-formed MWSE scripts, crashes on a malformed one.

`MWPatches::VirtualMemWriteAccessor` (`mwpatches.h`) is the shared RAII wrapper that flips page
protection to `PAGE_EXECUTE_READWRITE` and restores it in the destructor.

## Frustum member names

`NI::Frustum` spells its near and far planes `nearPlane` and `farPlane`, where MWSE uses `near`
and `far`. `minwindef.h` defines both of those as empty macros for 16-bit source compatibility,
and `#undef`ing them is not an escape either: it also defines `FAR` *as* `far`, so the undef
breaks `DEFINE_GUID` throughout the DirectX headers. This is the one place the MWSE naming
authority is deliberately overridden.
