#pragma once

#include <windows.h>
#include <cstddef>




namespace MWPatches {

class VirtualMemWriteAccessor {
    void* address;
    size_t length;
    DWORD oldProtect;

public:
    VirtualMemWriteAccessor(void* addr, size_t len, DWORD protect = PAGE_EXECUTE_READWRITE) : address(addr), length(len) {
        VirtualProtect(address, length, protect, &oldProtect);
    }
    ~VirtualMemWriteAccessor() {
        VirtualProtect(address, length, oldProtect, &oldProtect);
    }
};

inline DWORD read_dword(const DWORD dwAddress) {
    return *reinterpret_cast<DWORD*>(dwAddress);
}

inline BYTE read_byte(const DWORD dwAddress) {
    return *reinterpret_cast<BYTE*>(dwAddress);
}

inline void write_dword(const DWORD dwAddress, DWORD dword) {
    *reinterpret_cast<DWORD*>(dwAddress) = dword;
}

inline void write_byte(const DWORD dwAddress, BYTE byte) {
    *reinterpret_cast<BYTE*>(dwAddress) = byte;
}

inline void write_float(const DWORD dwAddress, float f) {
    *reinterpret_cast<float*>(dwAddress) = f;
}

inline void write_ptr(const DWORD dwAddress, void* ptr) {
    *reinterpret_cast<void**>(dwAddress) = ptr;
}




// Stops Morrowind from taking its own screenshots, or displaying an error message, when PrtScr is pressed
void disableScreenshotFunc();

// Turns off the sunglare billboard and fullscreen glare that appears when looking at the sun
void disableSunglare();

// Skips playing both intro movies
void disableIntroMovies();

// Patch in a callback to allow MGE to load before the first frame of world rendering
void patchGameLoading(void (__cdecl* newfunc)());

// Redirects splash screen scenegraph draw call to another function
void redirectMenuBackground(void (_stdcall* func)(int));

// Patches the normal call to ui_configureUIScale to redirect to a new function.
// MWBridge is not required to be loaded for this function.
void patchUIConfigure(void (_stdcall* newfunc)());

// Patches the splash screen quad so that it renders without
// gaps at the screen edge when multisampling is on.
void patchSplashScreen(unsigned int width, unsigned int height);

// Patches certain calls to timeGetTime to redirect to a new function.
void patchFrameTimer(int (__cdecl* newfunc)());

// Inserts a callback where game data is initialized or re-initialized.
void patchResolveDuringInit(void (__cdecl* newfunc)());

// Fix code that quenches light particles to stop affecting shared emissive materials
// In conjunction with per-pixel lighting, an engine bug causes fire particles from lights to be weak and transparent
void patchLightParticleMaterialModifier();

// Alter rendering of cells without water, so that alphas are deferred to a new scene, enabling detection
void patchWorldRenderingAccumulation();

// Raises the per-node local light effect limit from 7 to 32
void patchExpandedLightLimit();

} // namespace MWPatches
