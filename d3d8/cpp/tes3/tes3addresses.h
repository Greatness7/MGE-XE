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

// NOT a load-screen flag despite MGE having used it as one: every xref is
// VFXManager::*, which sets it on any VFX add/remove and clears it in
// VFXManager::update.
inline constexpr uintptr_t global_VFXManager_updateRequired = 0x7C85B8;

//-----------------------------------------------------------------------------
// Code patch sites
//-----------------------------------------------------------------------------

// Start of the ripple-spawn code that MGE overwrites to suppress ripples.
inline constexpr uintptr_t patch_ripples = 0x51C2D4;

// Raycast site patched to use the UI viewport size instead of the D3D one.
inline constexpr uintptr_t patch_uiScaleRaycast = 0x6F5157;

//-----------------------------------------------------------------------------
// Functions
//-----------------------------------------------------------------------------

inline constexpr uintptr_t AudioController_setMusicVolume = 0x403A10;
inline constexpr uintptr_t Game_renderNextFrame = 0x41BE90;
inline constexpr uintptr_t WorldController_configUIScaling = 0x40F2A0;
inline constexpr uintptr_t CursorController_setCursorBounds = 0x408740;

inline constexpr uintptr_t NonDynamicData_findGlobalVariable = 0x4BA820;
inline constexpr uintptr_t NonDynamicData_findDialogue = 0x4BA8D0;
inline constexpr uintptr_t NonDynamicData_findFirstReferenceByObjectId = 0x4B8F50;

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
