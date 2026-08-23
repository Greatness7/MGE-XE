#include "mge/mwpatches.h"
#include "support/log.h"

#include <cstring>


//-----------------------------------------------------------------------------

namespace MWPatches {

void disableScreenshotFunc() {
    DWORD addr = 0x41b08a;

    // Replace jz short with jmp (74 -> eb)
    VirtualMemWriteAccessor vw((void*)addr, 4);
    write_byte(addr, 0xeb);
}

//-----------------------------------------------------------------------------

void disableSunglare() {
    DWORD addr = 0x4404fb;

    // Replace jz short with nop (74 xx -> 90 90)
    VirtualMemWriteAccessor vw((void*)addr, 4);
    write_byte(addr, 0x90);
    write_byte(addr+1, 0x90);
}

//-----------------------------------------------------------------------------

void disableIntroMovies() {
    DWORD addr = 0x418ef0;
    BYTE patch[] = { 0xeb, 0x16 };

    VirtualMemWriteAccessor vw0((void*)addr, 2);
    memcpy((void*)addr, patch, sizeof(patch));

    addr = 0x5fc8f7;
    VirtualMemWriteAccessor vw1((void*)addr, 2);
    memcpy((void*)addr, patch, sizeof(patch));
}

//-----------------------------------------------------------------------------

void patchGameLoading(void (__cdecl* newfunc)()) {
    // addr1 - At end of game loading and init function
    // addr2 - After renderer restart
    DWORD addr1 = 0x41A052;
    DWORD addr2 = 0x41AA31;

    // Insert call before function epilogue
    VirtualMemWriteAccessor vw1((void*)addr1, 0x1E);
    memmove((void*)(addr1 + 5), (void*)addr1, 0x18);
    write_byte(addr1, 0xE8);
    write_dword(addr1 + 1, (DWORD)newfunc - (addr1+5));

    // Replace existing function call
    VirtualMemWriteAccessor vw2((void*)addr2, 5);
    write_dword(addr2 + 1, (DWORD)newfunc - (addr2+5));
}

//-----------------------------------------------------------------------------

void redirectMenuBackground(void (_stdcall* func)(int)) {
    DWORD addr = 0x04589fb;

    // Reset to original if null is passed
    DWORD calladdr = func ? (DWORD)func : 0x6cc7b0;

    // Replace jump address
    VirtualMemWriteAccessor vw((void*)addr, 4);
    write_dword(addr, calladdr - (addr+4));
}

//-----------------------------------------------------------------------------

void patchUIConfigure(void (_stdcall* newfunc)()) {
    DWORD addr = 0x40e554;
    BYTE patch[] = {
        0xb8, 0xff, 0xff, 0xff, 0xff,       // mov eax, newfunc
        0xff, 0xd0,                         // call eax
        0xeb, 0x06                          // jmp past rest of block
    };

    VirtualMemWriteAccessor vw((void*)addr, sizeof(patch));
    memcpy((void*)addr, patch, sizeof(patch));
    write_ptr(addr + 1, reinterpret_cast<void*>(newfunc));
}

//-----------------------------------------------------------------------------

void patchSplashScreen(unsigned int width, unsigned int height) {
    const float dx = -0.5 / width, dy = 0.5 / height;

    // Patch screen quad vertex coordinates with half pixel offset
    DWORD addr = 0x458E89;
    VirtualMemWriteAccessor vw((void*)addr, 0x5A);
    write_float(0x458E89 + 6, dx);
    write_float(0x458E93 + 6, dy);
    write_float(0x458EA4 + 3, 1.0 + dx);
    write_float(0x458EAB + 3, dy);
    write_float(0x458EB9 + 3, 1.0 + dx);
    write_float(0x458EC0 + 3, 1.0 + dy);
    write_float(0x458ECE + 3, dx);
    write_float(0x458ED5 + 3, 1.0 + dy);

    // Patch texture wrap mode to clamp
    DWORD addr2 = 0x4595E1;
    VirtualMemWriteAccessor vw2((void*)addr2, 4);
    write_dword(addr2, 0);
}

//-----------------------------------------------------------------------------

static int (__cdecl* patchFrameTimerTarget)();

void patchFrameTimer(int (__cdecl* newfunc)()) {
    DWORD addrs[] = { 0x403b52, 0x4535fd, 0x453615, 0x453638 };

    patchFrameTimerTarget = newfunc;

    for (int i = 0; i != sizeof(addrs)/sizeof(addrs[0]); ++i) {
        VirtualMemWriteAccessor vw((void*)addrs[i], sizeof(&patchFrameTimerTarget));
        write_dword(addrs[i], reinterpret_cast<DWORD>(&patchFrameTimerTarget));
    }
}

//-----------------------------------------------------------------------------

static void (__cdecl* patchResolveDuringInitFunc)();

static void __fastcall patchResolveDuringInitShim(void* worldController) {
    // Call original function.
    const auto resolveScriptInternalIDs = reinterpret_cast<void (__thiscall*)(void*)>(0x40FC40);
    resolveScriptInternalIDs(worldController);

    if (patchResolveDuringInitFunc) {
        patchResolveDuringInitFunc();
    }
}

void patchResolveDuringInit(void (__cdecl* newfunc)()) {
    DWORD addrs[] = { 0x419AC4, 0x4C601D, 0x5FB11A, 0x5FE929 };

    patchResolveDuringInitFunc = newfunc;

    for (int i = 0; i != sizeof(addrs)/sizeof(addrs[0]); ++i) {
        VirtualMemWriteAccessor vw((void*)addrs[i], 5);
        write_dword(addrs[i] + 1, reinterpret_cast<DWORD>(&patchResolveDuringInitShim) - addrs[i] - 5);
    }
}

//-----------------------------------------------------------------------------

void patchLightParticleMaterialModifier() {
    DWORD addr = 0x4D2789;

    // Jump over code that affects the particle emissive material
    VirtualMemWriteAccessor vw((void*)addr, 1);
    write_byte(addr, 0xEB);
}

//-----------------------------------------------------------------------------

static void __fastcall patchCameraClick(void* camera, int edx, bool dontFinishAccumulating) {
    const auto NiCamera_Click = reinterpret_cast<void (__thiscall*)(void*, bool)>(0x6CC7B0);

    if (dontFinishAccumulating) {
        // Call original code.
        NiCamera_Click(camera, true);
    }
    else {
        auto scenePtr = *reinterpret_cast<char**>(reinterpret_cast<char*>(camera) + 0x128);
        WORD *flagsPtr = reinterpret_cast<WORD*>(scenePtr + 0x14);

        // Render, but split accumulation to a new scene.
        NiCamera_Click(camera, true);

        // Hide scene and only render accumulator contents.
        auto previousFlags = *flagsPtr;
        *flagsPtr = 0x9; // AppCulled + IsVisual
        NiCamera_Click(camera, false);
        *flagsPtr = previousFlags;
    }
}

void patchWorldRenderingAccumulation() {
    DWORD addr = 0x41C654;

    // Patch main scene rendering function.
    VirtualMemWriteAccessor vw((void*)addr, 4);
    write_dword(addr + 1, reinterpret_cast<DWORD>(&patchCameraClick) - addr - 5);
}

//-----------------------------------------------------------------------------

// NiNode::PushLocalEffects (0x6c8f00) counts local light-type effects and skips
// NiDynamicEffectState::AddEffect (0x6c8ffa) once the counter passes the limit, so any
// effects past the seventh never reach the merged state. Only the signed imm8 operand is
// rewritten; the cmp/ja structure is untouched, which caps this patch form at 127.
// The caller decides whether the active render path can consume the extra lights.
void patchExpandedLightLimit() {
    const DWORD addr = 0x6c8ff0;
    const BYTE expected[] = { 0x83, 0xfd, 0x07, 0x77, 0x0a };    // cmp ebp, 7; ja 0x6c8fff
    const BYTE patched[] = { 0x83, 0xfd, 0x20, 0x77, 0x0a };     // cmp ebp, 32; ja 0x6c8fff

    BYTE actual[sizeof(expected)];
    for (size_t i = 0; i != sizeof(actual); ++i) {
        actual[i] = read_byte(addr + i);
    }

    if (memcmp(actual, patched, sizeof(actual)) == 0) {
        // Already applied; applying it twice is harmless
        return;
    }

    if (memcmp(actual, expected, sizeof(actual)) != 0) {
        LOG::logline(
            "!! Expanded light limit patch skipped, unexpected code at 0x%lx: %02x %02x %02x %02x %02x",
            addr, actual[0], actual[1], actual[2], actual[3], actual[4]);
        return;
    }

    VirtualMemWriteAccessor vw((void*)(addr + 2), 1);
    write_byte(addr + 2, 32);
    LOG::logline("-- Expanded per-node light limit to 32");
}

} // namespace MWPatches
