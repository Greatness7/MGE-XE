#include "mge/mwpatches.h"
#include "support/log.h"
#include "tes3/nitypes.h"
#include "tes3/tes3addresses.h"
#include "tes3/tes3types.h"

#include <cstring>


//-----------------------------------------------------------------------------

namespace MWPatches {

void disableScreenshotFunc() {
    DWORD addr = TES3::Address::patch_screenshotKey;

    // Replace jz short with jmp (74 -> eb)
    VirtualMemWriteAccessor vw((void*)addr, 4);
    write_byte(addr, 0xeb);
}

//-----------------------------------------------------------------------------

void disableSunglare() {
    DWORD addr = TES3::Address::patch_sunglare;

    // Replace jz short with nop (74 xx -> 90 90)
    VirtualMemWriteAccessor vw((void*)addr, 4);
    write_byte(addr, 0x90);
    write_byte(addr+1, 0x90);
}

//-----------------------------------------------------------------------------

void disableIntroMovies() {
    DWORD addr = TES3::Address::patch_bethesdaLogoMovie;
    BYTE patch[] = { 0xeb, 0x16 };

    VirtualMemWriteAccessor vw0((void*)addr, 2);
    memcpy((void*)addr, patch, sizeof(patch));

    addr = TES3::Address::patch_introMovieCheck;
    VirtualMemWriteAccessor vw1((void*)addr, 2);
    memcpy((void*)addr, patch, sizeof(patch));
}

//-----------------------------------------------------------------------------

void patchGameLoading(void (__cdecl* newfunc)()) {
    // addr1 - At end of game loading and init function
    // addr2 - After renderer restart
    DWORD addr1 = TES3::Address::patch_gameLoadingEnd;
    DWORD addr2 = TES3::Address::patch_afterRendererRestart;

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
    DWORD addr = TES3::Address::patch_menuBackgroundCameraClick;

    // Reset to original if null is passed
    DWORD calladdr = func ? (DWORD)func : TES3::Address::NI_Camera_click;

    // Replace jump address
    VirtualMemWriteAccessor vw((void*)addr, 4);
    write_dword(addr, calladdr - (addr+4));
}

//-----------------------------------------------------------------------------

void patchUIConfigure(void (_stdcall* newfunc)()) {
    DWORD addr = TES3::Address::patch_uiConfigure;
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
    // The first two sites take the immediate at instruction offset 6, the rest at 3.
    const auto& quad = TES3::Address::patch_splashQuad;
    DWORD addr = quad[0];
    VirtualMemWriteAccessor vw((void*)addr, 0x5A);
    write_float(quad[0] + 6, dx);
    write_float(quad[1] + 6, dy);
    write_float(quad[2] + 3, 1.0 + dx);
    write_float(quad[3] + 3, dy);
    write_float(quad[4] + 3, 1.0 + dx);
    write_float(quad[5] + 3, 1.0 + dy);
    write_float(quad[6] + 3, dx);
    write_float(quad[7] + 3, 1.0 + dy);

    // Patch texture wrap mode to clamp
    DWORD addr2 = TES3::Address::patch_splashTextureWrapMode;
    VirtualMemWriteAccessor vw2((void*)addr2, 4);
    write_dword(addr2, 0);
}

//-----------------------------------------------------------------------------

static int (__cdecl* patchFrameTimerTarget)();

void patchFrameTimer(int (__cdecl* newfunc)()) {
    const auto& addrs = TES3::Address::patch_frameTimer;

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
    const auto resolveScriptInternalIDs = reinterpret_cast<void (__thiscall*)(void*)>(TES3::Address::WorldController_resolveScriptInternalIDs);
    resolveScriptInternalIDs(worldController);

    if (patchResolveDuringInitFunc) {
        patchResolveDuringInitFunc();
    }
}

void patchResolveDuringInit(void (__cdecl* newfunc)()) {
    const auto& addrs = TES3::Address::patch_resolveDuringInit;

    patchResolveDuringInitFunc = newfunc;

    for (int i = 0; i != sizeof(addrs)/sizeof(addrs[0]); ++i) {
        VirtualMemWriteAccessor vw((void*)addrs[i], 5);
        write_dword(addrs[i] + 1, reinterpret_cast<DWORD>(&patchResolveDuringInitShim) - addrs[i] - 5);
    }
}

//-----------------------------------------------------------------------------

void patchLightParticleMaterialModifier() {
    DWORD addr = TES3::Address::patch_particleEmissiveMaterial;

    // Jump over code that affects the particle emissive material
    VirtualMemWriteAccessor vw((void*)addr, 1);
    write_byte(addr, 0xEB);
}

//-----------------------------------------------------------------------------

static void __fastcall patchCameraClick(NI::Camera* camera, int edx, bool dontFinishAccumulating) {
    const auto NiCamera_Click = reinterpret_cast<void (__thiscall*)(NI::Camera*, bool)>(
        TES3::Address::NI_Camera_click);

    if (dontFinishAccumulating) {
        // Call original code.
        NiCamera_Click(camera, true);
    }
    else {
        // Captured before the first click, which may replace the camera's scene.
        NI::Node* const scene = camera->scene;

        // Render, but split accumulation to a new scene.
        NiCamera_Click(camera, true);

        // Hide scene and only render accumulator contents.
        auto previousFlags = scene->flags;
        scene->flags = 0x9; // AppCulled + IsVisual
        NiCamera_Click(camera, false);
        scene->flags = previousFlags;
    }
}

void patchWorldRenderingAccumulation() {
    DWORD addr = TES3::Address::patch_mainSceneRenderLoop;

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
    const DWORD addr = TES3::Address::patch_localEffectsLimit;
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
