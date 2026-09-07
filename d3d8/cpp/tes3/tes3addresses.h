#pragma once

// Fixed addresses in Morrowind.exe v1.6.1820.
//
// Morrowind.exe is a fixed target -- these never move. Names come from MWSE
// where it has one and from the IDB otherwise; see docs/architecture/mwbridge.md.

#include <cstdint>

namespace TES3 {
namespace Address {

//-----------------------------------------------------------------------------
// Globals
//-----------------------------------------------------------------------------

// Total real time elapsed this session; does not advance in menus.
inline constexpr uintptr_t simulationTimestamp = 0x7C6708;

// Player eye height, pre-multiplied by 125.0f. Read-only from MGE's side.
inline constexpr uintptr_t playerHeight = 0x7D39F0;

// Set once both intro movies are done and the main menu is about to display.
inline constexpr uintptr_t isIntroDone = 0x7D5005;

// Set and cleared by ui_showLoadingMenu / ui_destroyLoadingMenu. This is the
// genuine "a loading bar is on screen" flag.
inline constexpr uintptr_t ui_MenuLoading_active = 0x7D4294;

inline constexpr uintptr_t ui_id_MenuLoading = 0x7D4238;
inline constexpr uintptr_t ui_id_MenuLoading_label = 0x7D4204;

inline constexpr uintptr_t ui_MenuBarter_haggleAmount = 0x7D287C;

//-----------------------------------------------------------------------------
// Statics and virtual tables
//-----------------------------------------------------------------------------

// The reference a script is currently targeted at.
inline constexpr uintptr_t scriptTargetRef = 0x7CEBEC;

// The engine's single global NI::Pick.
inline constexpr uintptr_t global_pick = 0x7D12E8;

inline constexpr uintptr_t vtable_NPCBase = 0x749DE8;

//-----------------------------------------------------------------------------
// Code patch sites
//-----------------------------------------------------------------------------

// Start of the ripple-spawn code that MGE overwrites to suppress ripples.
inline constexpr uintptr_t patch_ripples = 0x51C2D4;

// Raycast site patched to use the UI viewport size instead of the D3D one.
inline constexpr uintptr_t patch_uiScaleRaycast = 0x6F5157;

// `jz short` made unconditional, freeing the key for MGE's own screenshot.
inline constexpr uintptr_t patch_screenshotKey = 0x41B08A;

// `jz short` nopped out, disabling the engine sunglare.
inline constexpr uintptr_t patch_sunglare = 0x4404FB;

// Both made `jmp short`, skipping the Bethesda logo and the intro movie check.
inline constexpr uintptr_t patch_bethesdaLogoMovie = 0x418EF0;
inline constexpr uintptr_t patch_introMovieCheck = 0x5FC8F7;

// A call is inserted before the epilogue at the first; the second is an existing
// call retargeted. Together they cover game load and renderer restart.
inline constexpr uintptr_t patch_gameLoadingEnd = 0x41A052;
inline constexpr uintptr_t patch_afterRendererRestart = 0x41AA31;

// Call retargeted away from NI::Camera::click for the menu background.
inline constexpr uintptr_t patch_menuBackgroundCameraClick = 0x4589FB;

// Call in the main scene render loop, retargeted to split alpha accumulation.
inline constexpr uintptr_t patch_mainSceneRenderLoop = 0x41C654;

// Trampoline over the engine's UI configuration and scaling block.
inline constexpr uintptr_t patch_uiConfigure = 0x40E554;

// Splash-screen quad vertex coordinates, taking a half-pixel offset. The write
// offset within each instruction differs, so the sites are listed individually.
inline constexpr uintptr_t patch_splashQuad[8] = {
    0x458E89, 0x458E93, 0x458EA4, 0x458EAB, 0x458EB9, 0x458EC0, 0x458ECE, 0x458ED5,
};
inline constexpr uintptr_t patch_splashTextureWrapMode = 0x4595E1;

// `jmp short` over the code that affects the particle emissive material.
inline constexpr uintptr_t patch_particleEmissiveMaterial = 0x4D2789;

// timeGetTime call sites, retargeted to MGE's frame timer.
inline constexpr uintptr_t patch_frameTimer[4] = {
    0x403B52, 0x4535FD, 0x453615, 0x453638,
};

// Calls to WorldController::resolveScriptInternalIDs, retargeted to a shim.
inline constexpr uintptr_t patch_resolveDuringInit[4] = {
    0x419AC4, 0x4C601D, 0x5FB11A, 0x5FE929,
};

// The `cmp ebp, 7` guarding NiNode::PushLocalEffects's per-node light counter.
// Only the signed imm8 operand is rewritten, which caps the limit at 127.
inline constexpr uintptr_t patch_localEffectsLimit = 0x6C8FF0;

//-----------------------------------------------------------------------------
// Functions
//-----------------------------------------------------------------------------

inline constexpr uintptr_t NI_Camera_click = 0x6CC7B0;
inline constexpr uintptr_t WorldController_resolveScriptInternalIDs = 0x40FC40;

inline constexpr uintptr_t AudioController_setMusicVolume = 0x403A10;
inline constexpr uintptr_t Game_renderNextFrame = 0x41BE90;
inline constexpr uintptr_t WorldController_configUIScaling = 0x40F2A0;
inline constexpr uintptr_t CursorController_setCursorBounds = 0x408740;

inline constexpr uintptr_t NonDynamicData_resolveObject = 0x4B8B60;
inline constexpr uintptr_t NonDynamicData_findGlobalVariable = 0x4BA820;
inline constexpr uintptr_t NonDynamicData_findDialogue = 0x4BA8D0;
inline constexpr uintptr_t NonDynamicData_findFirstReferenceByObjectId = 0x4B8F50;

inline constexpr uintptr_t BaseObject_isClone = 0x4F0FF0;
inline constexpr uintptr_t Reference_getAttachment6ItemData = 0x4E5460;
inline constexpr uintptr_t Reference_getMobile = 0x4E5750;
inline constexpr uintptr_t getAssociatedReference = 0x4C3C40;
inline constexpr uintptr_t NI_Pick_pickObjects = 0x6F3050;
inline constexpr uintptr_t NI_Pick_clearResults = 0x6F2F80;

inline constexpr uintptr_t ui_updateLoadingMenu = 0x4C7D90;
inline constexpr uintptr_t ui_getText = 0x580BB0;
inline constexpr uintptr_t ui_findChildElement = 0x582DE0;
inline constexpr uintptr_t ui_findMenu = 0x595370;
inline constexpr uintptr_t ui_showLoadingMenu = 0x5DED20;
inline constexpr uintptr_t ui_destroyLoadingMenu = 0x5DEEA0;
inline constexpr uintptr_t ui_updateLoadingLabel = 0x5DEEF0;
inline constexpr uintptr_t ui_MenuBarter_updateHaggle = 0x5A74C0;

}  // namespace Address
}  // namespace TES3
