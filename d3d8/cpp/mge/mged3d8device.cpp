
#include "mged3d8device.h"
#include "proxydx/d3d8texture.h"
#include "proxydx/d3d8surface.h"
#include "dxvk_morrowind_interop.h"

#include <algorithm>
#include <cstring>
#include "mgeversion.h"
#include "camerarelative.h"
#include "configuration.h"
#include "distantland.h"
#include "morrowindskinning.h"
#include "mwbridge.h"
#include "mwpatches.h"
#include "statusoverlay.h"
#include "userhud.h"
#include "videobackground.h"

static int sceneCount;
static bool rendertargetNormal, isHUDready;
static bool isMainView, isStencilScene, isAmbientWhite;
static DWORD stencilRef;
static bool stage0Complete, isFrameComplete, isHUDComplete;
static bool isWaterMaterial, waterDrawn, distantWater;

static bool zoomSensSaved;
static float zoomSensX, zoomSensY;
static D3DXMATRIX camEffectsMatrix;
static float crosshairTimeout;

static RenderedState rs;
static FragmentState frs;
static LightState lightrs;

// The world matrix being set was already made camera-relative by an engine
// hook in camerarelative.cpp (exact node or bone placement).
static bool worldAlreadyRelative;

static void initOnLoad();
static bool detectMenu(const D3DMATRIX* m);
static void captureRenderState(D3DRENDERSTATETYPE a, DWORD b);
static void captureFragmentRenderState(DWORD a, D3DTEXTURESTAGESTATETYPE b, DWORD c);
static void captureTransform(D3DTRANSFORMSTATETYPE a, const D3DMATRIX* b);
static void captureLight(DWORD a, const D3DLIGHT8* b);
static void captureMaterial(const D3DMATERIAL8* a);
static float calcFPS();



MGEProxyDevice::MGEProxyDevice(IDirect3DDevice9* real, ProxyD3D* d3d) : ProxyDevice(real, d3d) {
    // Initialize state here, as the device is released and recreated on fullscreen Alt-Tab
    sceneCount = -1;
    rendertargetNormal = true;
    isHUDready = false;
    isMainView = isStencilScene = isAmbientWhite = stage0Complete = isFrameComplete = isHUDComplete = false;
    stencilRef = 0;
    isWaterMaterial = waterDrawn = false;
    D3DXMatrixIdentity(&camEffectsMatrix);

    Configuration.CameraEffects.zoom = 1.0;
    Configuration.CameraEffects.zoomRate = 0;
    Configuration.CameraEffects.zoomRateTarget = 0;

    // Initialize state recorder to D3D defaults
    memset(&rs, 0, sizeof(rs));
    FixedFunctionShader::resetSkinningTransforms();
    rs.zWrite = true;
    rs.diffuseMaterial.r = 1.0f;
    rs.diffuseMaterial.g = 1.0f;
    rs.diffuseMaterial.b = 1.0f;
    rs.diffuseMaterial.a = 1.0f;
    rs.cullMode = D3DCULL_CCW;
    rs.useLighting = true;

    rs.matSrcDiffuse = D3DMCS_COLOR1;
    rs.matSrcEmissive = D3DMCS_MATERIAL;

    memset(&frs, 0, sizeof(frs));
    for (FragmentState::Stage* s = &frs.stage[0]; s != &frs.stage[8]; ++s) {
        s->colorOp = D3DTOP_DISABLE;
        s->alphaOp = D3DTOP_DISABLE;
        s->colorArg1 = s->alphaArg1 = D3DTA_TEXTURE;
        s->colorArg2 = s->alphaArg2 = D3DTA_CURRENT;
        s->colorArg0 = s->alphaArg0 = s->resultArg = D3DTA_CURRENT;
        D3DXMatrixIdentity(&s->textureTransform);
    }
    lightrs.ambientWhite = false;
    frs.stage[0].colorOp = D3DTOP_MODULATE;
    frs.stage[0].alphaOp = D3DTOP_SELECTARG1;

    lightrs.lights.clear();
    lightrs.active.clear();

    // Probe the renderer for expanded light limit support. This asks the
    // dedicated capability rather than the packet version, because the engine
    // patch it authorizes is irreversible and the lighting mode is not: every
    // path reachable afterwards, including ordinary fixed-function, has to
    // consume the expanded limit. A renderer that only spoke the native packet
    // would not be enough. The interop object delegates reference counting to
    // the D3D9 device, so it is released immediately; only the result is kept.
    expandedLightLimitSupported = false;
    {
        IDxvkMorrowindPplInterop1* pplInterop = nullptr;
        if (SUCCEEDED(realDevice->QueryInterface(
                __uuidof(IDxvkMorrowindPplInterop1),
                reinterpret_cast<void**>(&pplInterop)))) {
            expandedLightLimitSupported =
                (pplInterop->GetCapabilities() & DXVK_MORROWIND_CAP_EXPANDED_LIGHT_LIMIT) != 0;
            pplInterop->Release();
        }
    }

    // Store active device in distant land, occurs on startup and after fullscreen alt-tab
    DistantLand::device = realDevice;

    // Patch splash screen minor issues
    D3DVIEWPORT9 vp;
    realDevice->GetViewport(&vp);
    MWPatches::patchSplashScreen(vp.Width, vp.Height);

    // Install the indexed-skinning engine hooks here rather than at first
    // Present: Morrowind can issue draws through this device before it ever
    // presents a frame. The installer is one-shot, so recreating the device on
    // fullscreen alt-tab does not re-patch.
    MorrowindIndexedSkinning::installHooks();
    CameraRelative::installHooks();
}

HRESULT _stdcall MGEProxyDevice::QueryInterface(REFIID a, LPVOID* b) {
    if (!b) {
        return E_POINTER;
    }

    if (std::memcmp(&a, &IID_IMgeIndexedSkinningCaps, sizeof(GUID)) == 0) {
        *b = static_cast<IMgeIndexedSkinningCaps*>(this);
        AddRef();
        return S_OK;
    }

    return ProxyDevice::QueryInterface(a, b);
}

ULONG _stdcall MGEProxyDevice::AddRef() {
    return ProxyDevice::AddRef();
}

// The reported palette size is the feature's composite authorization token.
// Reporting zero is what keeps the engine hooks on their stock paths, so every
// gate the feature depends on is checked here in one place.
HRESULT _stdcall MGEProxyDevice::GetIndexedSkinningCaps(MgeIndexedSkinningCaps* caps) {
    if (!caps) {
        return E_POINTER;
    }

    caps->structVersion = MGE_INDEXED_SKINNING_CAPS_VERSION;
    caps->maxPaletteBones = 0;

    // Partial hook installation must never authorize indexed partitions.
    if (!MorrowindIndexedSkinning::hooksInstalled()) {
        return S_OK;
    }

    // Restart-required setting; it is not re-read once partitions are built.
    if (!Configuration.EnableIndexedSkinning) {
        return S_OK;
    }

    // Skinned draws can occur before the core effects are initialized. Defer
    // negotiation instead of permanently caching a false startup result.
    if (!(Configuration.MGEFlags & MGE_DISABLED) && !Configuration.OnlyProxyD3D8To9) {
        if (!DistantLand::hasCheckedIndexedSkinningShaders()) {
            return E_PENDING;
        }
        if (!DistantLand::supportsIndexedSkinningShaders()) {
            return S_OK;
        }
    }

    D3DCAPS9 deviceCaps = {};
    const HRESULT hr = realDevice->GetDeviceCaps(&deviceCaps);
    if (FAILED(hr)) {
        return hr;
    }

    caps->maxPaletteBones = std::min<std::uint32_t>(
        MGE_INDEXED_SKINNING_PALETTE_SIZE,
        deviceCaps.MaxVertexBlendMatrixIndex + 1
    );
    return S_OK;
}

HRESULT _stdcall MGEProxyDevice::Present(const RECT* a, const RECT* b, HWND c, const RGNDATA* d) {
    CameraRelative::probeFrameEnd();

    auto mwBridge = MWBridge::get();

    // Load Morrowind's dynamic memory pointers
    if (!mwBridge->IsLoaded() && mwBridge->CanLoad()) {
        mwBridge->Load();

        // Apply patch to load distant land before the main menu, and on renderer restart
        MWPatches::patchGameLoading(&initOnLoad);
        // Patch world rendering (on a branch without the water) to split alphas to their own scene
        MWPatches::patchWorldRenderingAccumulation();
        // Disable MW screenshot function to allow MGE to use the same key
        MWPatches::disableScreenshotFunc();
        // Mark water material to allow MGEProxyDevice to detect it
        mwBridge->markWaterNode(99999.0f);

        // Raise the engine's per-node light limit, on request and only when the
        // renderer reports it can consume the extra lights. This is one
        // decision per device and it is not reversible: the lighting mode is
        // runtime-mutable (ToggleLightingMode, MGEAPI::lightingModeSet,
        // interiors-only stepping outdoors), but no later re-evaluation can
        // unpatch the executable. Every path the engine can reach afterwards
        // must therefore handle 32 lights, which is why the renderer probe
        // covers the ordinary fixed-function path and not just native packets.
        if (Configuration.ExpandedLightLimit && expandedLightLimitSupported) {
            MWPatches::patchExpandedLightLimit();
        }

        // Start distant-land host/output preparation and upload pumping at the earliest
        // Present where Morrowind's environment pointer is known to be valid.
        if (!(Configuration.MGEFlags & MGE_DISABLED)
            && !Configuration.OnlyProxyD3D8To9
            && (Configuration.MGEFlags & USE_DISTANT_LAND)) {
            DistantLand::init();
        }
    }

    if (mwBridge->IsLoaded()) {
        if (Configuration.Force3rdPerson && DistantLand::canRenderDistantLand()) {
            // Set 3rd person camera
            D3DXVECTOR3* camera = mwBridge->PCam3Offset();
            if (camera) {
                camera->x = Configuration.Offset3rdPerson.x;
                camera->y = Configuration.Offset3rdPerson.y;
                camera->z = Configuration.Offset3rdPerson.z;
            }
        }

        if ((Configuration.MGEFlags & CROSSHAIR_AUTOHIDE) && !mwBridge->IsLoadScreen()) {
            // Update crosshair visibility
            float t = mwBridge->simulationTime();

            // Turn on if Morrowind ray cast picks up a target
            if (mwBridge->getPlayerTarget()) {
                crosshairTimeout = t + 1.5f;
            }

            // Turn on short duration if the player requires aim
            if (mwBridge->isPlayerCasting() || mwBridge->isPlayerAimingWeapon()) {
                crosshairTimeout = t + 0.5f;
            }

            // Turn off in menu mode
            if (mwBridge->IsMenu()) {
                crosshairTimeout = t;
            }

            // Allow manual toggle of crosshair to work again from 0.5 seconds after timeout
            if (t < crosshairTimeout + 0.5) {
                mwBridge->SetCrosshairEnabled(t < crosshairTimeout);
            }
        }

        if (Configuration.CameraEffects.zoomRateTarget != 0 && !mwBridge->IsMenu()) {
            // Update zoom controller
            Configuration.CameraEffects.zoomRate += 0.25f * Configuration.CameraEffects.zoomRateTarget * mwBridge->frameTime();
            if (Configuration.CameraEffects.zoomRate / Configuration.CameraEffects.zoomRateTarget > 1.0) {
                Configuration.CameraEffects.zoomRate = Configuration.CameraEffects.zoomRateTarget;
            }

            Configuration.CameraEffects.zoom += Configuration.CameraEffects.zoomRate * mwBridge->frameTime();
            Configuration.CameraEffects.zoom = std::max(1.0f, Configuration.CameraEffects.zoom);
            Configuration.CameraEffects.zoom = std::min(Configuration.CameraEffects.zoom, 8.0f);
        }

        float* mwSens = mwBridge->getMouseSensitivityYX();
        if ((Configuration.MGEFlags & ZOOM_ASPECT) && !mwBridge->IsMenu()) {
            // Adjust sensitivity to accommodate zoom level
            if (!zoomSensSaved) {
                zoomSensY = mwSens[0];
                zoomSensX = mwSens[1];
                zoomSensSaved = true;
            }
            mwSens[0] = zoomSensY / Configuration.CameraEffects.zoom;
            mwSens[1] = zoomSensX / Configuration.CameraEffects.zoom;
        } else if (zoomSensSaved) {
            // Restore unzoomed sensitivity
            mwSens[0] = zoomSensY;
            mwSens[1] = zoomSensX;
            zoomSensSaved = false;
        }

        if (Configuration.CameraEffects.rotateUpdate) {
            Configuration.CameraEffects.rotation += Configuration.CameraEffects.rotationRate * mwBridge->frameTime();
            D3DXMatrixRotationZ(&camEffectsMatrix, Configuration.CameraEffects.rotation);
            if (Configuration.CameraEffects.rotationRate == 0) {
                Configuration.CameraEffects.rotateUpdate = false;
            }
        }
        if (Configuration.CameraEffects.shake) {
            // Update screen shake controller
            Configuration.CameraEffects.shakeMagnitude += Configuration.CameraEffects.shakeAccel * mwBridge->frameTime();
            Configuration.CameraEffects.shakeMagnitude = std::max(0.0f, std::min(100.0f, Configuration.CameraEffects.shakeMagnitude));
            camEffectsMatrix._41 = Configuration.CameraEffects.shakeMagnitude * sin(0.001f*GetTickCount());
        }

        // Main menu background video
        VideoPatch::monitor(realDevice);
    }

    // Capture whether stage0 ran this frame before the per-frame reset below: menu and
    // load-screen frames never reach it, so residency eviction needs a fallback boundary.
    const bool stage0RanThisFrame = stage0Complete;

    // Reset scene identifiers
    sceneCount = -1;
    stage0Complete = false;
    waterDrawn = false;
    isFrameComplete = false;
    isHUDComplete = false;
    DistantLand::beginDepthFrame();

    // Tick the frame-budgeted distant-land upload pump across idle menu/load
    // frames (load-optimization Blocker 4). Runs after EndScene, so it is a safe
    // point to create D3D resources. The budget keeps menu frames responsive.
    if (DistantLand::pumpActive && !DistantLand::pumpDraining) {
        DistantLand::pumpUploadTick(DistantLand::kUploadPumpBudgetMs);
    } else if (!DistantLand::pumpDraining) {
        DistantLand::tickResidency(stage0RanThisFrame);
    }

    return ProxyDevice::Present(a, b, c, d);
}

HRESULT _stdcall MGEProxyDevice::SetRenderTarget(IDirect3DSurface8* a, IDirect3DSurface8* b) {
    // Track whether Morrowind is rendering to the back buffer; world-scene hooks use this target.
    if (a) {
        IDirect3DSurface9* back;
        realDevice->GetBackBuffer(0, 0, D3DBACKBUFFER_TYPE_MONO, &back);
        rendertargetNormal = (static_cast<ProxySurface*>(a)->realSurface == back);
        back->Release();
    }

    return ProxyDevice::SetRenderTarget(a, b);
}

// Morrowind uses multiple scenes per frame: non-alpha / 2x stencil / post-stencil redraw / alpha /
// 1st person / UI. Fog state must be set before its first world scene.
HRESULT _stdcall MGEProxyDevice::BeginScene() {
    auto mwBridge = MWBridge::get();

    HRESULT hr = ProxyDevice::BeginScene();
    if (hr != D3D_OK) {
        return hr;
    }

    if (mwBridge->IsLoaded() && rendertargetNormal) {
        if (!isHUDready) {
            StatusOverlay::init(realDevice);
            StatusOverlay::setStatus(XE_VERSION_STRING);
            MGEhud::init(realDevice);

            if (Configuration.UIScale != 1.0f) {
                mwBridge->setUIScale(Configuration.UIScale);
            }

            isHUDready = true;
        }

        if (isMainView) {
            // Track scene count here in BeginScene; isMainView is not always valid at EndScene if Morrowind draws sunglare
            ++sceneCount;

            if (sceneCount == 0) {
                if (Configuration.ScreenFOV > 0) {
                    mwBridge->SetFOV(Configuration.ScreenFOV);
                }
                distantWater = (Configuration.MGEFlags & USE_DISTANT_LAND) || (Configuration.MGEFlags & USE_DISTANT_WATER);
            }
        } else {
            // UI scene, apply post-process if there was anything drawn before it
            // The race menu can issue an extra scene; post-process and HUD are still one-shot.
            if (DistantLand::canRenderDistantLand() && sceneCount > 0 && !isFrameComplete) {
                DistantLand::postProcess();
            }

            // Draw the user HUD before Morrowind's HUD so the game UI remains on top.
            if (isHUDready && !isHUDComplete) {
                MGEhud::draw();
            }

            isFrameComplete = true;
        }
    }

    return D3D_OK;
}

// Multiple scenes per frame: non-alpha / 2x stencil / post-stencil redraw / alpha / 1st person / UI
// Intercepts first scene to draw distant land before it finishes; other scenes have shadows applied
HRESULT _stdcall MGEProxyDevice::EndScene() {
    if (DistantLand::canRenderDistantLand() && rendertargetNormal) {
        // The following Morrowind scenes get past the filters:
        // ~ Opaque meshes, plus alpha meshes with 'No Sorter' property (which should use alpha test)
        // ~ If stencil shadows are active, then shadow casters are deferred to be drawn in a scene after
        //    shadows are fully applied to avoid self-shadowing problems with simplified shadow meshes
        // ~ If any alpha meshes are visible, they are sorted and drawn in another scene (except those with 'No Sorter' property)
        // ~ If 1st person or sunglare is visible, they are drawn in another scene after a Z clear
        if (sceneCount == 0) {
            if (!stage0Complete) {
                // A fully culled Morrowind scene may produce no draw call, so stage 0 must also
                // be started from EndScene.
                DistantLand::renderStage0();
                stage0Complete = true;
            }

            DistantLand::renderStage1();
            DistantLand::renderStageBlend();
        } else if (!isFrameComplete) {
            DistantLand::renderStage2();

            if (distantWater && !waterDrawn && !isStencilScene) {
                // The Morrowind water grid can be out of view, or scene/stencil order can be
                // non-standard; draw replacement water once when that happens.
                DistantLand::renderStageWater();
                waterDrawn = true;
            }
        }
    }

    if (isFrameComplete && isHUDready && !isHUDComplete) {
        DistantLand::checkCaptureScreenshot(true);

        StatusOverlay::setFPS(calcFPS());
        StatusOverlay::show(realDevice);

        isHUDComplete = true;
    }

    return ProxyDevice::EndScene();
}

// Clear occurs at start of frame, and as a z-clear before rendering 1st person and sunglare
// Skybox mesh doesn't extend over whole background; cleared background colour is visible at horizon
HRESULT _stdcall MGEProxyDevice::Clear(DWORD a, const D3DRECT* b, DWORD c, D3DCOLOR d, float e, DWORD f) {
    DistantLand::setHorizonColour(d);
    const HRESULT hr = ProxyDevice::Clear(a, b, c, d, e, f);
    if (SUCCEEDED(hr) && (c & D3DCLEAR_ZBUFFER)) {
        DistantLand::depthBufferCleared();
    }
    return hr;
}

HRESULT _stdcall MGEProxyDevice::SetTransform(D3DTRANSFORMSTATETYPE a, const D3DMATRIX* b) {
    worldAlreadyRelative = CameraRelative::takeWorldRelative();

    if (a == D3DTS_VIEW) {
        // Decide the space of this scene before the recorder sees the view, so every
        // world matrix captured afterwards is combined with the same view.
        CameraRelative::onViewTransform(b, rendertargetNormal && !detectMenu(b));
    }

    captureTransform(a, b);

    if (rendertargetNormal) {
        if (a == D3DTS_VIEW) {
            isMainView = !detectMenu(b);

            if (isMainView) {
                D3DXMATRIX view;
                if (CameraRelative::active()) {
                    CameraRelative::deviceView(&camEffectsMatrix, &view);
                } else {
                    view = *b;
                    view *= camEffectsMatrix;
                }
                return ProxyDevice::SetTransform(a, &view);
            }
        } else if (a == D3DTS_PROJECTION) {
            if (isMainView) {
                CameraRelative::probeProjection(b);

                // Expand only the world projection; UI and load-bar projections must stay intact.
                D3DXMATRIX proj = *b;
                DistantLand::setProjection(&proj);

                if (Configuration.MGEFlags & ZOOM_ASPECT) {
                    proj._11 *= Configuration.CameraEffects.zoom;
                    proj._22 *= Configuration.CameraEffects.zoom;
                }

                DistantLand::trackDepthProjection(&proj);
                return ProxyDevice::SetTransform(a, &proj);
            }
        }
    }

    if (a == D3DTS_PROJECTION) {
        DistantLand::trackDepthProjection(b);
    }

    if (CameraRelative::active() && !worldAlreadyRelative
        && a >= D3DTS_WORLDMATRIX(0) && a < D3DTS_WORLDMATRIX(MGE_INDEXED_SKINNING_PALETTE_SIZE)) {
        // The real device only ever sees camera-relative world matrices in this
        // space, so its own fixed-function world-view product is small-number math.
        D3DXMATRIX world;
        CameraRelative::relativeWorld(b, &world);
        return ProxyDevice::SetTransform(a, &world);
    }
    return ProxyDevice::SetTransform(a, b);
}

HRESULT _stdcall MGEProxyDevice::SetMaterial(const D3DMATERIAL8* a) {
    captureMaterial(a);
    isWaterMaterial = (a->Power == 99999.0f);

    return ProxyDevice::SetMaterial(a);
}

HRESULT _stdcall MGEProxyDevice::SetLight(DWORD a, const D3DLIGHT8* b) {
    captureLight(a, b);

    // Exterior sunlight/interior "sun" appears to always be light 6
    if (a == 6 && DistantLand::canRenderDistantLand()) {
        DistantLand::setSunLight(b);
    }

    if (CameraRelative::active() && b->Type == D3DLIGHT_POINT) {
        // Keep the fixed-function path's lights in the same space as its geometry.
        D3DLIGHT8 light = *b;
        CameraRelative::relativePosition(&b->Position, &light.Position);
        return ProxyDevice::SetLight(a, &light);
    }
    return ProxyDevice::SetLight(a, b);
}

HRESULT _stdcall MGEProxyDevice::SetRenderState(D3DRENDERSTATETYPE a, DWORD b) {
    captureRenderState(a, b);

    if (a == D3DRS_FOGVERTEXMODE || a == D3DRS_FOGTABLEMODE) {
        return D3D_OK;
    }
    if ((Configuration.MGEFlags & USE_DISTANT_LAND) && (a == D3DRS_FOGSTART || a == D3DRS_FOGEND)) {
        return D3D_OK;
    }
    if (a == D3DRS_STENCILENABLE) {
        isStencilScene = b;
    }
    else if (a == D3DRS_STENCILREF) {
        stencilRef = b;
    }

    if (a == D3DRS_AMBIENT) {
        // Pure white ambient occurs with skydome and menu mode rendering
        // Ambient is also never set properly when high enough outside that Morrowind renders nothing
        isAmbientWhite = (b == 0xffffffff);
        lightrs.ambientWhite = isAmbientWhite;

        if (!isAmbientWhite) {
            // Preserve the last real ambient value for frames where Morrowind emits no draw calls.
            RGBVECTOR amb = D3DCOLOR(b);
            DistantLand::setAmbientColour(amb);
            lightrs.globalAmbient.r = amb.r;
            lightrs.globalAmbient.g = amb.g;
            lightrs.globalAmbient.b = amb.b;
        }
    }

    return ProxyDevice::SetRenderState(a, b);
}

HRESULT _stdcall MGEProxyDevice::SetTextureStageState(DWORD a, D3DTEXTURESTAGESTATETYPE b, DWORD c) {
    captureFragmentRenderState(a, b, c);

    // Sampler overrides to ensure trilinear/anisotropic filtering works
    // Note that DX8 had sampling state bound to texture stages instead of samplers
    if (b == D3DTSS_MINFILTER) {
        DWORD filter = (c != D3DTEXF_NONE) ? Configuration.ScaleFilter : D3DTEXF_NONE;
        return realDevice->SetSamplerState(a, D3DSAMP_MINFILTER, filter);
    } else if (b == D3DTSS_MIPFILTER) {
        DWORD filter = (c != D3DTEXF_NONE) ? D3DTEXF_LINEAR : D3DTEXF_NONE;
        return realDevice->SetSamplerState(a, D3DSAMP_MIPFILTER, filter);
    }

    return ProxyDevice::SetTextureStageState(a, b, c);
}

HRESULT _stdcall MGEProxyDevice::DrawIndexedPrimitive(D3DPRIMITIVETYPE a, UINT b, UINT c, UINT d, UINT e) {
    const bool indexedSkinning =
        rs.vertexBlendState != D3DVBF_DISABLE &&
        (rs.fvf & D3DFVF_LASTBETA_UBYTE4) != 0;

    bool isShadowStencil = isStencilScene && stencilRef <= 1;
    if (rendertargetNormal && isMainView && sceneCount == 0 && !isShadowStencil) {
        // Measures the captured matrices only, so it does not need distant land.
        CameraRelative::probeDraw(&rs);
    }

    if (DistantLand::canRenderDistantLand() && rendertargetNormal && isMainView && !isShadowStencil) {
        rs.primType = a;
        rs.baseIndex = baseVertexIndex;
        rs.minIndex = b;
        rs.vertCount = c;
        rs.startIndex = d;
        rs.primCount = e;

        if (!stage0Complete && !isAmbientWhite) {
            // In an exterior this is normally the first world draw after the sky; interiors may
            // have no preceding draw, so stage 0 is guarded here as well as in EndScene.
            DistantLand::renderStage0();
            stage0Complete = true;
        }

        if (isWaterMaterial) {
            if (distantWater) {
                // Replacement water suppresses the original Morrowind grid.
                if (!waterDrawn) {
                    DistantLand::renderStageWater();
                    waterDrawn = true;
                }
                return D3D_OK;
            }
        } else {
            // DistantLand may record the draw for a later pass and signal that the original is
            // already represented; in that case suppress the game draw.
            if (!DistantLand::inspectIndexedPrimitive(sceneCount, &rs, &frs, &lightrs)) {
                return D3D_OK;
            }
        }
    }

    if (!indexedSkinning) {
        return ProxyDevice::DrawIndexedPrimitive(a, b, c, d, e);
    }

    realDevice->SetRenderState(D3DRS_INDEXEDVERTEXBLENDENABLE, TRUE);
    const HRESULT hr = ProxyDevice::DrawIndexedPrimitive(a, b, c, d, e);
    realDevice->SetRenderState(D3DRS_INDEXEDVERTEXBLENDENABLE, FALSE);
    return hr;
}

ULONG _stdcall MGEProxyDevice::Release() {
    ULONG r = ProxyDevice::Release();

    if (r == 0) {
        DistantLand::release();
        MGEhud::release();
        StatusOverlay::release();
        // Drops the negotiated device and the fallback cache's engine
        // references. The executable patches stay installed; they are
        // process-lifetime and the next device renegotiates against them.
        MorrowindIndexedSkinning::onDeviceReleased();
    }

    return r;
}

void initOnLoad() {
    auto mwBridge = MWBridge::get();
    auto device = DistantLand::device;

    // Compose loading message from translated string
    char buffer[64];
    const char* loadingMessage = *(const char**)mwBridge->getGMSTPointer(602);
    int firstWordLength = 0;

    for (const char *c = loadingMessage; *c; ++c) {
        if (*c == ' ') { break; }
        ++firstWordLength;
    }

    std::snprintf(buffer, sizeof(buffer), "%.*s MGE XE...", firstWordLength, loadingMessage);
    mwBridge->showLoadingBar(buffer, 95.0);

    // Initialize distant land
    if (DistantLand::init()) {
        // Initially force view distance to max, required for full extent shadows and grass
        if (Configuration.MGEFlags & USE_DISTANT_LAND) {
            mwBridge->SetViewDistance(7168.0);
        }
    } else {
        Configuration.MGEFlags &= ~USE_DISTANT_LAND;
        StatusOverlay::setStatus("MGE XE serious error condition. Exit Morrowind and check mgeXE.log for details.", StatusOverlay::PriorityError);
    }

    // Clean up loading bar menu, otherwise it persists in the background
    mwBridge->destroyLoadingBar();

    VideoPatch::start(device);
}

// detectMenu
// detects if view matrix is for UI / load bars
// the projection matrix is never set to ortho, making it unusable for detection
bool detectMenu(const D3DMATRIX* m) {
    if (m->_41 != 0.0f || !(m->_42 == 0.0f || m->_42 == -600.0f) || m->_43 != 0.0f) {
        return false;
    }

    if ((m->_11 == 0.0f || m->_11 == 1.0f) && m->_12 == 0.0f && (m->_13 == 0.0f || m->_13 == 1.0f) &&
            m->_21 == 0.0f && (m->_22 == 0.0f || m->_22 == 1.0f) && (m->_23 == 0.0f || m->_23 == 1.0f) &&
            (m->_31 == 0.0f || m->_31 == 1.0f) && (m->_32 == 0.0f || m->_32 == 1.0f) && m->_33 == 0.0f) {
        return true;
    }

    return false;
}

// --------------------------------------------------------
// State recording

HRESULT _stdcall MGEProxyDevice::SetTexture(DWORD a, IDirect3DBaseTexture8* b) {
    if (a < 8) {
        IDirect3DTexture9* tex = b ? static_cast<ProxyTexture*>(b)->realTexture : NULL;
        frs.stage[a].texture = tex;
        if (a == 0) {
            rs.texture = tex;
        }
    }
    return ProxyDevice::SetTexture(a, b);
}

HRESULT _stdcall MGEProxyDevice::SetVertexShader(DWORD a) {
    rs.fvf = a;
    return ProxyDevice::SetVertexShader(a);
}

HRESULT _stdcall MGEProxyDevice::SetStreamSource(UINT a, IDirect3DVertexBuffer8* b, UINT c) {
    if (a == 0) {
        rs.vb = (IDirect3DVertexBuffer9*)b;
        rs.vbOffset = 0;
        rs.vbStride = c;
    }
    return ProxyDevice::SetStreamSource(a, b, c);
}

HRESULT _stdcall MGEProxyDevice::SetIndices(IDirect3DIndexBuffer8* a, UINT b) {
    rs.ib = (IDirect3DIndexBuffer9*)a;
    return ProxyDevice::SetIndices(a, b);
}

HRESULT _stdcall MGEProxyDevice::LightEnable(DWORD a, BOOL b) {
    if (b) {
        if (std::find(lightrs.active.begin(), lightrs.active.end(), a) == lightrs.active.end()) {
            lightrs.active.push_back(a);
        }
    } else {
        if (std::remove(lightrs.active.begin(), lightrs.active.end(), a) != lightrs.active.end()) {
            lightrs.active.pop_back();
        }
    }
    return ProxyDevice::LightEnable(a, b);
}

void captureRenderState(D3DRENDERSTATETYPE a, DWORD b) {
    switch (a) {
    case D3DRS_VERTEXBLEND:
        rs.vertexBlendState = b;
        break;
    case D3DRS_ZWRITEENABLE:
        rs.zWrite = b;
        break;
    case D3DRS_CULLMODE:
        rs.cullMode = b;
        break;
    case D3DRS_ALPHABLENDENABLE:
        rs.blendEnable = (BYTE)b;
        break;
    case D3DRS_SRCBLEND:
        rs.srcBlend = (BYTE)b;
        break;
    case D3DRS_DESTBLEND:
        rs.destBlend = (BYTE)b;
        break;
    case D3DRS_ALPHATESTENABLE:
        rs.alphaTest = (BYTE)b;
        break;
    case D3DRS_ALPHAFUNC:
        rs.alphaFunc = (BYTE)b;
        break;
    case D3DRS_ALPHAREF:
        rs.alphaRef = (BYTE)b;
        break;
    case D3DRS_LIGHTING:
        rs.useLighting = (BYTE)b;
        break;
    case D3DRS_FOGENABLE:
        rs.useFog = (BYTE)b;
        break;
    case D3DRS_DIFFUSEMATERIALSOURCE:
        rs.matSrcDiffuse = (BYTE)b;
        break;
    case D3DRS_EMISSIVEMATERIALSOURCE:
        rs.matSrcEmissive = (BYTE)b;
        break;
    }
}

void captureFragmentRenderState(DWORD a, D3DTEXTURESTAGESTATETYPE b, DWORD c) {
    FragmentState::Stage* s = &frs.stage[a];

    switch (b) {
    case D3DTSS_COLOROP:
        s->colorOp = (BYTE)c;
        break;
    case D3DTSS_COLORARG1:
        s->colorArg1 = (BYTE)c;
        break;
    case D3DTSS_COLORARG2:
        s->colorArg2 = (BYTE)c;
        break;
    case D3DTSS_ALPHAOP:
        s->alphaOp = (BYTE)c;
        break;
    case D3DTSS_ALPHAARG1:
        s->alphaArg1 = (BYTE)c;
        break;
    case D3DTSS_ALPHAARG2:
        s->alphaArg2 = (BYTE)c;
        break;
    case D3DTSS_BUMPENVMAT00:
        s->bumpEnvMat[0][0] = reinterpret_cast<float&>(c);
        break;
    case D3DTSS_BUMPENVMAT01:
        s->bumpEnvMat[0][1] = reinterpret_cast<float&>(c);
        break;
    case D3DTSS_BUMPENVMAT10:
        s->bumpEnvMat[1][0] = reinterpret_cast<float&>(c);
        break;
    case D3DTSS_BUMPENVMAT11:
        s->bumpEnvMat[1][1] = reinterpret_cast<float&>(c);
        break;
    case D3DTSS_TEXCOORDINDEX:
        s->texcoordIndex = c;
        break;
    case D3DTSS_BUMPENVLSCALE:
        s->bumpLumiScale = reinterpret_cast<float&>(c);
        break;
    case D3DTSS_BUMPENVLOFFSET:
        s->bumpLumiBias = reinterpret_cast<float&>(c);
        break;
    case D3DTSS_TEXTURETRANSFORMFLAGS:
        s->texTransformFlags = c;
        break;
    case D3DTSS_COLORARG0:
        s->colorArg0 = (BYTE)c;
        break;
    case D3DTSS_ALPHAARG0:
        s->alphaArg0 = (BYTE)c;
        break;
    case D3DTSS_RESULTARG:
        s->resultArg = (BYTE)c;
        break;
    }
}

void captureTransform(D3DTRANSFORMSTATETYPE a, const D3DMATRIX* b) {
    if (a >= D3DTS_WORLDMATRIX(0) && a < D3DTS_WORLDMATRIX(MGE_INDEXED_SKINNING_PALETTE_SIZE)) {
        const UINT index = a - D3DTS_WORLDMATRIX(0);

        if (CameraRelative::active()) {
            // The engine's world translation is exact; the rounding happens in the
            // world-view product. Subtract the camera first, then multiply in double.
            // A matrix an engine hook already placed relative to the camera is used
            // as is, and the recorder's absolute copy is rebuilt from it.
            D3DXMATRIX world;
            D3DXMATRIX absolute;
            if (worldAlreadyRelative) {
                world = *b;
                CameraRelative::absoluteFromRelative(b, &absolute);
            } else {
                CameraRelative::relativeWorld(b, &world);
                absolute = *b;
            }
            FixedFunctionShader::setSkinningWorldTransform(index, &world, &rs.viewTransform);

            if (index < 4) {
                rs.worldTransforms[index] = absolute;
                CameraRelative::multiplyWorldView(&world, &rs.viewTransform, &rs.worldViewTransforms[index]);
            }
            return;
        }

        const auto world = static_cast<const D3DXMATRIX*>(b);
        FixedFunctionShader::setSkinningWorldTransform(index, world, &rs.viewTransform);

        if (index < 4) {
            rs.worldTransforms[index] = *b;
            D3DXMatrixMultiply(&rs.worldViewTransforms[index], world, &rs.viewTransform);
        }
        return;
    }

    if (a == D3DTS_VIEW) {
        // While camera-relative space is active the recorder works against a
        // rotation-only view; world matrices carry the camera offset instead.
        rs.viewTransform = CameraRelative::active() ? *CameraRelative::recorderView() : *static_cast<const D3DXMATRIX*>(b);
        FixedFunctionShader::setSkinningViewTransform(&rs.viewTransform);
        lightrs.lightsTransformed.clear();
        return;
    }

    if (a >= D3DTS_TEXTURE0 && a <= D3DTS_TEXTURE7) {
        frs.stage[a - D3DTS_TEXTURE0].textureTransform = *b;
    }
}

void captureLight(DWORD a, const D3DLIGHT8* b) {
    // Morrowind uses non-contigous light IDs up to a large number (>512)
    auto iLight = lightrs.lights.find(a);
    const bool existed = iLight != lightrs.lights.end();
    LightState::Light* light = existed ? &iLight->second : &lightrs.lights[a];

    // Copy values relevant to Morrowind
    // i.e. Morrowind has no spotlights and always sets range to FLT_MAX
    // The only light source with ambient is sunlight
    const D3DLIGHTTYPE previousType = light->type;
    const D3DVECTOR previousPosition = light->position;

    light->type = b->Type;
    light->diffuse = b->Diffuse;

    if (b->Type == D3DLIGHT_POINT) {
        light->position = b->Position;
        light->falloff.x = b->Attenuation0;
        light->falloff.y = b->Attenuation1;
        light->falloff.z = b->Attenuation2;
    } else {
        D3DXVec3Normalize((D3DXVECTOR3*)&light->position, (D3DXVECTOR3*)&b->Direction);
        light->ambient.x = b->Ambient.r;
        light->ambient.y = b->Ambient.g;
        light->ambient.z = b->Ambient.b;
    }

    // The cached view-space position is only stale if the source-space position
    // or direction moved, or the light changed type. Morrowind resubmits
    // unchanged lights for every object, so invalidating on every SetLight
    // would defeat the cross-object cache; not invalidating at all leaves a
    // light that moves twice under one view transform with a stale position.
    if (existed
     && (previousType != light->type
      || previousPosition.x != light->position.x
      || previousPosition.y != light->position.y
      || previousPosition.z != light->position.z)) {
        lightrs.lightsTransformed.erase(a);
    }
}

void captureMaterial(const D3DMATERIAL8* a) {
    // Morrowind does not use specular lighting
    rs.diffuseMaterial = a->Diffuse;
    frs.material.diffuse = a->Diffuse;
    frs.material.ambient = a->Ambient;
    frs.material.emissive = a->Emissive;
    frs.material.emissive.a = a->Power;
}

// --------------------------------------------------------
// FPS meter - Updates every 500ms. Morrowind's internal meter changes too fast and falsely clamps the fps.

float calcFPS() {
    static int lastMillis, framesSinceUpdate;
    static float fps = 0.0f;

    ++framesSinceUpdate;
    int millis = MWBridge::get()->getFrameBeginMillis();
    int diff = millis - lastMillis;

    if (diff >= 500) {
        fps = 1000.0f * framesSinceUpdate / diff;
        lastMillis = millis;
        framesSinceUpdate = 0;
    } else if (diff < 0) {
        lastMillis = millis;
    }

    return fps;
}
