# Morrowind engine memory and patches

How MGE XE reaches into Morrowind's process: hardcoded engine-memory anchors, fixed-address
executable patches, and the NetImmerse DDS hooks. Every address below is for `Morrowind.exe`
FileVersion 1.6.1820 at imagebase `0x400000`, and was verified against an IDA database of that
binary.

## File map

| Path | Ownership |
| --- | --- |
| `d3d8/cpp/mge/mwbridge.*` | Dynamic pointer resolution, engine-state access, node tagging, loading-bar calls |
| `d3d8/cpp/mge/mwpatches.*` | Fixed-address executable patches and their callbacks/trampolines |
| `d3d8/cpp/mge/mwtextureloader.*` | NetImmerse DDS/BC7 hooks and staging-to-default-pool texture upload |

## Two classes of address

Only 20 absolute addresses are hardcoded as bridge anchors, all assigned in
`MWBridge::InitStaticMemory`, which runs from the constructor before the game has loaded
anything.

Everything else is derived. `MWBridge::Load` pointer-chases from `eMaster`:
`eMaster1 = read_dword(eMaster)`, then `eFPS = eMaster1 + 0x14`, `eTimer = eFPS + 0xC`,
and so on. Those offsets are structure layout, not addresses, so they survive a rebase but
not a struct change.

A third group of absolute addresses appears inline in the patch functions themselves
(function pointers and patch sites), in `mwpatches.cpp` and `mwtextureloader.cpp` rather
than `MWBridge::InitStaticMemory`.

`eMaster` is the root of the whole derivation: `0x7C67DC` = `worldController`.

## Anchor verification

Every anchor resolves inside a correctly-named function or global. `+N` is the offset
into the containing function; anchors without an offset sit on the function entry.

| Anchor | Address | Resolves to |
| --- | --- | --- |
| `eMaster` | `0x7C67DC` | `worldController` (data) |
| `eNoMusicBreak` | `0x403659` | `AudioController::MusicEvent` +0x9 |
| `eMusicVolFunc` | `0x403A10` | `AudioController::SetMusicVolume` |
| `eHaggleUpdate` | `0x5A74C0` | `ui_MenuBarter_updateHaggle` |
| `eHaggleAmount` | `0x7D287C` | `global_barterHaggleAmount` (data) |
| `eTruform` | `0x6E4FFC` | unnamed data |
| `eGetMouseState` | `0x406721` | `InputController::readKeyState` +0x141 |
| `eNoWorldFOV` | `0x4049FE` | `WorldControllerRenderCameraData::buildFrustum` +0x9e |
| `eXRotSpeed` | `0x5692B1` | `MACP::playerGameInputHandler` +0x9a1 |
| `eScrollScale` | `0x6139B6` | `ui_showScrollMenu` +0x116 |
| `eBookScale` | `0x5AC47B` | `ui_showBookMenu` +0x1db |
| `eRipplesSwitch` | `0x51C2D4` | `loc_51C2D4` |
| `eXMenuHudIn` | `0x5F43C4` | `ui_showMultiMenu` +0x24 |
| `eXMenuNoMouse` | `0x408740` | `CursorController::setCursorBounds` |
| `eXMenuNoFOV` | `0x404B38` | `…::updateFrustumForViewport` +0x108 |
| `eXMenuWnds` | `0x583072` | `UiElement::clampToViewport` +0x12 |
| `eXMenuPopups` | `0x5961AC` | `ui_repositionTooltipAboveCursor` +0x1ac |
| `eXMenuLoWnds` | `0x586985` | `UiElement::updateLayout_menuPositioning` +0x265 |
| `eXMenuSubtitles` | `0x5F980F` | `ui_MenuNotify_repositionAll` +0x3f |
| `eXMenuFPS` | `0x41BC5E` | `TES3Game::mainLoop` +0x116e |

`eXMenuNoMouse` does double duty: the same address is also called as a function pointer
(`ui_configureUIMouseArea`, `MWBridge::setUIScale`).

Patch-site and function-pointer addresses verified the same way, notably:
`MWPatches::patchGameLoading` -> `TES3Game::createScene` / `TES3Game::restartRenderer`;
`MWPatches::patchWorldRenderingAccumulation` -> `TES3Game_static::renderMainScene`;
`MWTextureLoader::patchLoadTexture2D` -> `NiDX8SourceTextureData_static::Create`;
`MWPatches::patchLightParticleMaterialModifier` -> `sg_stopParticlesAndSetEmissiveMaterialToBlack`;
`MWPatches::patchExpandedLightLimit` -> `NiNode::PushLocalEffects`;
`MWPatches::disableSunglare` -> `WeatherController::sunGlareTests`;
`MWPatches::disableIntroMovies` -> `TES3Game::initGame` and `ui_mainMenu`.

### Checking a new address

The table above is settled; you need this only when adding or changing an anchor. With
the IDB open and `ida-pro-mcp` running: `lookup_funcs` a code address (it returns the
containing function), `idc.get_name` via `py_eval` for a data address. To confirm an IDB
is the right binary at all, `get_bytes` at `0x6C8FF0` must read `83 FD 07 77 0A`, the
`expected[]` array in `MWPatches::patchExpandedLightLimit`.

## Two-phase patch install

**Phase 1, pre-device.** `MWInitPatch::patch()` runs during DLL attach, before any D3D
device exists. It may only touch static addresses, since nothing has been allocated yet:
UI-scale patch (only when MGE is disabled or in proxy-only mode), intro-movie skip,
`MWTextureLoader::patchLoadTexture2D`, `MWPatches::patchFrameTimer`, and
`MWPatches::patchLightParticleMaterialModifier`.

**Phase 2, first usable Present.** `MGEProxyDevice::Present` tests
`!IsLoaded() && CanLoad()`, then calls `Load()` followed by
`MWPatches::patchGameLoading`, `MWPatches::patchWorldRenderingAccumulation`,
`MWPatches::disableScreenshotFunc`, `MWBridge::markWaterNode(99999.0f)`, conditionally
`MWPatches::patchExpandedLightLimit`, and finally `DistantLand::init()`.

`CanLoad()` is one line: `read_dword(eEnviro) != 0`, where `eEnviro = eMaster + 4`.
That is the entire readiness test: the environment pointer is non-null, so the pointer
chase in `Load()` will not dereference null.

`MWPatches::patchSplashScreen` is separate. It runs from device creation because
it needs the viewport size. The texture hooks install in phase 1, while device creation
later calls `MWTextureLoader::setBC7TextureSupport` before textures load.

## Notes

**No version check, and that is fine.** `CanLoad()` tests a pointer for non-null; it does
not identify the binary. Against a game that has not shipped a patch in twenty years and
never will, a version gate would guard a condition that cannot occur.

**Only one executable installer validates before writing.**
`MWPatches::patchExpandedLightLimit` compares against an expected signature first and
detects the already-patched case. The other patch writes are blind. Since the target bytes
are fixed, this catches our mistakes, such as a mistyped address or a double-apply, rather than
a foreign binary. Worth copying only if a patch is conditional or can run twice.

**Scenegraph tagging by magic value.** Water is marked by material `Power == 99999`
(`MWBridge::markWaterNode`) and moons by emissive alpha `88888`
(`MWBridge::markMoonNodes`), which the render path recognizes. Sentinel values in float
fields, not flags. A mod setting the same value would be misclassified.

`MWPatches::VirtualMemWriteAccessor` (`mwpatches.h`) is the shared RAII wrapper that flips
page protection to `PAGE_EXECUTE_READWRITE` and restores it in the destructor.
