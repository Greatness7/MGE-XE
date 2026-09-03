
#include "proxydx/d3d8header.h"
#include "proxydx/d3d8device.h"
#include "support/log.h"
#include "configuration.h"
#include "distantland.h"
#include "distantshader.h"
#include "dlformat.h"
#include "dlmapping.h"
#include "postshaders.h"
#include "morrowindbsa.h"
#include "mwbridge.h"
#include "mwpatches.h"
#include "mgeversion.h"
#include "dxvk_morrowind_interop.h"

#include "statusoverlay.h"
#include "ipc/dlshare.h"
#include "support/timing.h"
#include <memory>
#include <optional>
#include <array>
#include <algorithm>
#include <cmath>
#include <cctype>
#include <cstdio>
#include <cstring>
#include <cstdlib>
#include <limits>



using std::string;
using std::string_view;
using std::vector;

namespace DistantLoaders {
    float staticsProgressRatio();
    bool queryStaticsSkipResult(bool& result);
    void releaseTerrainResources();
    void releaseStaticsResources();

    // Merged-static residency runtime (distantstatics.cpp).
    void beginResidency();
    void haltResidency();
    std::uint64_t totalMergedGeometryBytes();
    std::uint64_t logicalGpuMergedGeometryBytes();
    bool residencyFullDrainActive();
    bool stepResidencyFullDrain(double budgetMs, std::uint64_t budgetBytes, std::uint32_t budgetResources, bool& done);
    void wakeResidencyForCapDebt();
    bool residencyActive();
    bool residencyHasPendingEviction();
    bool residencyQuiescent();
    void noteResidencyPresent();
    void noteResidencyEvictionBoundary(bool stage0);
    void planResidency(const D3DXVECTOR3& center, std::uint32_t viewHeadingBin);
    void tickResidencyAdmission(double budgetMs, std::uint64_t budgetBytes, std::uint32_t budgetResources);
    bool tickResidencyEviction(double budgetMs, std::uint32_t budgetResources);
    void logResidencySummary();
}

DistantLand::InitState DistantLand::state = DistantLand::InitState::Uninitialized;
bool DistantLand::indexedSkinningShaderCheckComplete = false;
bool DistantLand::indexedSkinningShadersCompatible = false;
bool DistantLand::isRenderCached = false;
bool DistantLand::isPPLActive = false;
int DistantLand::numWaterVerts, DistantLand::numWaterTris;

IDirect3DDevice9* DistantLand::device;
ID3DXEffect* DistantLand::effect;
ID3DXEffect* DistantLand::effectShadow;
ID3DXEffect* DistantLand::effectDepth;
ID3DXEffectPool* DistantLand::effectPool;
IDirect3DVertexDeclaration9* DistantLand::TerrainDecl;
IDirect3DVertexDeclaration9* DistantLand::StaticDecl;
IDirect3DVertexDeclaration9* DistantLand::WaterDecl;
IDirect3DVertexDeclaration9* DistantLand::GrassDecl;

VendorSpecificRendering DistantLand::vsr;

IPC::Client DistantLand::ipcClient;
std::vector<DistantLand::DynamicVisGroup> DistantLand::dynamicVisGroups;
void* DistantLand::lastDistantVisCell;
std::string DistantLand::lastWorldSpaceKey;
bool DistantLand::lastWorldSpaceFound = false;
bool DistantLand::worldSpaceCacheValid = false;
bool DistantLand::isDistantLandLoaded = false;
bool DistantLand::staticsUploaded = false;

DistantLand::UploadPhase DistantLand::uploadPhase = DistantLand::UploadPhase::None;
bool DistantLand::pumpActive = false;
bool DistantLand::pumpDraining = false;
bool DistantLand::worldResolved = false;
bool DistantLand::uploadComplete = false;
bool DistantLand::streamingCapOverrideActive = false;
bool DistantLand::automaticStreamingCapActive = false;
std::uint64_t DistantLand::mergedStreamingCapBytes = 0;
int DistantLand::nextMergedBudgetSampleMs = 0;
std::uint32_t DistantLand::lowerMergedBudgetSampleCount = 0;
std::uint32_t DistantLand::mergedBudgetSampleCount = 0;
std::uint32_t DistantLand::mergedBudgetRatchetCount = 0;
std::uint64_t DistantLand::pendingMergedCandidateCapBytes = 0;
std::uint64_t DistantLand::pendingMergedPeakMemoryUsedBytes = 0;
std::uint64_t DistantLand::peakMergedMemoryUsedBytes = 0;
bool DistantLand::staticsPhaseStarted = false;
int DistantLand::residencyBootstrapStartedMs = 0;
bool DistantLand::outputStatusQueryPending = false;
int DistantLand::outputWaitStartedMs = 0;
int DistantLand::outputWaitNextLogMs = 30000;
IPC::VecId DistantLand::landscapeHostVecId = IPC::InvalidVector;
IPC::VecId DistantLand::staticsHostVecId = IPC::InvalidVector;
IPC::VecId DistantLand::subsetsHostVecId = IPC::InvalidVector;

VisibleSet DistantLand::visLandShared;
VisibleSet DistantLand::visDistantShared;
VisibleSet DistantLand::visGrassShared;
VisibleSet DistantLand::visExtraShared;
IPC::VecView<IPC::DynVisFlag> DistantLand::dynVisFlagsShared;
IPC::VecView<IPC::ResidencyPlan> DistantLand::residencyPlanShared;
IPC::VecView<IPC::ResidencyCommit> DistantLand::residencyCommitShared;

IPC::VecId DistantLand::visLandSharedId = IPC::InvalidVector;
IPC::VecId DistantLand::visDistantSharedId = IPC::InvalidVector;
IPC::VecId DistantLand::visGrassSharedId = IPC::InvalidVector;
IPC::VecId DistantLand::visExtraSharedId = IPC::InvalidVector;
IPC::VecId DistantLand::dynVisFlagsSharedId = IPC::InvalidVector;
IPC::VecId DistantLand::residencyPlanSharedId = IPC::InvalidVector;
IPC::VecId DistantLand::residencyCommitSharedId = IPC::InvalidVector;

vector<DistantLand::RecordedState> DistantLand::recordMW;
vector<DistantLand::RecordedState> DistantLand::recordSky;
vector<D3DXMATRIX> DistantLand::recordedSkinPalettes;
vector< std::pair<const RenderMesh*, int> > DistantLand::batchedGrass;
bool DistantLand::postPointLightsRequested = false;
vector<DistantLand::ObservedPointLight> DistantLand::observedPostLights;
D3DXVECTOR4 DistantLand::postLightPositions[DistantLand::kMaxPostPointLights];
D3DXVECTOR4 DistantLand::postLightColours[DistantLand::kMaxPostPointLights];
int DistantLand::postLightCount = 0;

IDirect3DTexture9* DistantLand::texTerrainAtlas, *DistantLand::texTerrainMaterial, *DistantLand::texTerrainMaterialFlags, *DistantLand::texTerrainPatchAlbedo, *DistantLand::texTerrainBlendPatterns;
IDirect3DTexture9* DistantLand::texDepthFrame;
IDirect3DSurface9* DistantLand::surfDepthDepth;
IDirect3DTexture9* DistantLand::texDepthStencil;
IDirect3DSurface9* DistantLand::surfDepthStencil;
IDirect3DSurface9* DistantLand::surfAutoDepthStencil;
IDirect3DTexture9* DistantLand::texDistantBlend;
IDirect3DTexture9* DistantLand::texReflection;
IDirect3DSurface9* DistantLand::surfReflectionZ;
IDirect3DVolumeTexture9* DistantLand::texWater;
IDirect3DVertexBuffer9* DistantLand::vbWater;
IDirect3DIndexBuffer9* DistantLand::ibWater;
IDirect3DVertexBuffer9* DistantLand::vbGrassInstances;

IDirect3DTexture9* DistantLand::texRain;
IDirect3DTexture9* DistantLand::texRipples;
IDirect3DTexture9* DistantLand::texRippleBuffer;
IDirect3DSurface9* DistantLand::surfRain;
IDirect3DSurface9* DistantLand::surfRipples;
IDirect3DSurface9* DistantLand::surfRippleBuffer;
IDirect3DVertexBuffer9* DistantLand::vbWaveSim;

IDirect3DTexture9* DistantLand::texShadow;
IDirect3DTexture9* DistantLand::texSoftShadow;
IDirect3DSurface9* DistantLand::surfShadowZ;
IDirect3DVertexBuffer9* DistantLand::vbFullFrame;

D3DXMATRIX DistantLand::mwView, DistantLand::mwProj;
D3DXMATRIX DistantLand::smView[kShadowCascades], DistantLand::smProj[kShadowCascades];
D3DXMATRIX DistantLand::smViewproj[kShadowCascades];
D3DXVECTOR4 DistantLand::eyeVec, DistantLand::eyePos;
D3DXVECTOR4 DistantLand::sunVec, DistantLand::sunPos;
float DistantLand::sunVis;
RGBVECTOR DistantLand::sunCol, DistantLand::sunAmb, DistantLand::ambCol;
RGBVECTOR DistantLand::nearFogCol, DistantLand::horizonCol;
RGBVECTOR DistantLand::atmOutscatter(0.07, 0.36, 0.76);
RGBVECTOR DistantLand::atmInscatter(0.25, 0.38, 0.48);
D3DXVECTOR4 DistantLand::atmSkylightScatter(0.4456, 0.6194, 1.0, 0.44);
float DistantLand::fogStart, DistantLand::fogEnd;
float DistantLand::fogExpStart, DistantLand::fogExpDivisor;
float DistantLand::fogNearStart, DistantLand::fogNearEnd;
float DistantLand::nearViewRange;
float DistantLand::windScaling, DistantLand::niceWeather;
float DistantLand::lightSunMult, DistantLand::lightAmbMult;
DistantLand::TerrainRuntimeConstants DistantLand::terrainConstants = {};

namespace {

bool readMergedStreamingCapOverride(std::uint64_t& capBytes) {
    char value[64] = {};
    const DWORD length = GetEnvironmentVariableA("MGE_DL_STREAMING_CAP_MB", value, static_cast<DWORD>(sizeof(value)));
    if (length == 0) {
        return false;
    }
    if (length >= sizeof(value)) {
        LOG::logline("!! MGE_DL_STREAMING_CAP_MB is too long; ignoring the development override");
        return false;
    }

    char* end = nullptr;
    const unsigned long long capMb = std::strtoull(value, &end, 10);
    if (end == value || *end != '\0' || capMb > (std::numeric_limits<std::uint64_t>::max() >> 20)) {
        LOG::logline("!! MGE_DL_STREAMING_CAP_MB=%s is invalid; ignoring the development override", value);
        return false;
    }
    capBytes = static_cast<std::uint64_t>(capMb) << 20;
    return true;
}

std::uint64_t saturatingAdd(std::uint64_t a, std::uint64_t b) {
    return b > std::numeric_limits<std::uint64_t>::max() - a
        ? std::numeric_limits<std::uint64_t>::max()
        : a + b;
}

std::uint64_t mergedCapCandidate(
    std::uint64_t heapBudget,
    std::uint64_t memoryUsed,
    std::uint64_t logicalGpuMergedBytes,
    std::uint64_t& headroom
) {
    headroom = std::clamp(
        heapBudget / 8,
        DistantLand::kMergedBudgetMinHeadroomBytes,
        DistantLand::kMergedBudgetMaxHeadroomBytes
    );
    const std::uint64_t available = heapBudget > memoryUsed
        ? heapBudget - memoryUsed
        : 0;
    const std::uint64_t availableAfterHeadroom = available > headroom
        ? available - headroom
        : 0;

    // logicalGpuMergedBytes and DXVK memoryUsed rise together when a streamed resource is
    // committed, so adding the former keeps the candidate stable. Do not reduce this to
    // budget - used: that would make MGE's own admissions shrink its cap.
    return saturatingAdd(logicalGpuMergedBytes, availableAfterHeadroom);
}

}

DistantLand::NativeDepthBackend DistantLand::nativeDepthBackend = DistantLand::NativeDepthBackend::None;
IDxvkMorrowindInterop* DistantLand::dxvkMorrowindInterop = nullptr;
IDxvkMorrowindMemoryInterop1* DistantLand::dxvkMorrowindMemoryInterop = nullptr;
bool DistantLand::stage1UsedNativeDepth = false;
bool DistantLand::nativeStage2Eligible = false;
bool DistantLand::dsvMayBeNoncanonical = true;
unsigned long long DistantLand::nativeStage1Captures = 0;
unsigned long long DistantLand::nativeStage2Captures = 0;
unsigned long long DistantLand::stage1LegacyFallbacks = 0;
unsigned long long DistantLand::stage2LegacyFallbacks = 0;
unsigned long long DistantLand::depthReplayDips = 0;

D3DXHANDLE DistantLand::ehRcpRes;
D3DXHANDLE DistantLand::ehShadowRcpRes;
D3DXHANDLE DistantLand::ehWorld;
D3DXHANDLE DistantLand::ehView;
D3DXHANDLE DistantLand::ehProj;
D3DXHANDLE DistantLand::ehShadowViewproj;
D3DXHANDLE DistantLand::ehVertexBlendState;
D3DXHANDLE DistantLand::ehVertexBlendPalette;
D3DXHANDLE DistantLand::ehAlphaRef;
D3DXHANDLE DistantLand::ehMaterialAlpha;
D3DXHANDLE DistantLand::ehHasAlpha;
D3DXHANDLE DistantLand::ehHasBones;
D3DXHANDLE DistantLand::ehHasVCol;
D3DXHANDLE DistantLand::ehUvBoundPalette;
D3DXHANDLE DistantLand::ehTex0;
D3DXHANDLE DistantLand::ehTex1;
D3DXHANDLE DistantLand::ehTex2;
D3DXHANDLE DistantLand::ehTex3;
D3DXHANDLE DistantLand::ehTex4;
D3DXHANDLE DistantLand::ehTex5;
D3DXHANDLE DistantLand::ehDepthSrc;
D3DXHANDLE DistantLand::ehSourceM33;
D3DXHANDLE DistantLand::ehSourceM43;
D3DXHANDLE DistantLand::ehEyePos;
D3DXHANDLE DistantLand::ehFootPos;
D3DXHANDLE DistantLand::ehSunCol;
D3DXHANDLE DistantLand::ehSunAmb;
D3DXHANDLE DistantLand::ehSunVec;
D3DXHANDLE DistantLand::ehSunVecView;
D3DXHANDLE DistantLand::ehSunPos;
D3DXHANDLE DistantLand::ehSunVis;
D3DXHANDLE DistantLand::ehOutscatter;
D3DXHANDLE DistantLand::ehInscatter;
D3DXHANDLE DistantLand::ehSkyScatterFar;
D3DXHANDLE DistantLand::ehSkyCol;
D3DXHANDLE DistantLand::ehFogColNear;
D3DXHANDLE DistantLand::ehFogColFar;
D3DXHANDLE DistantLand::ehFogStart;
D3DXHANDLE DistantLand::ehFogRange;
D3DXHANDLE DistantLand::ehFogNearStart;
D3DXHANDLE DistantLand::ehFogNearRange;
D3DXHANDLE DistantLand::ehNearViewRange;
D3DXHANDLE DistantLand::ehWindVec;
D3DXHANDLE DistantLand::ehNiceWeather;
D3DXHANDLE DistantLand::ehTime;
D3DXHANDLE DistantLand::ehTerrainAtlasTex;
D3DXHANDLE DistantLand::ehTerrainMaterialTex;
D3DXHANDLE DistantLand::ehTerrainMaterialFlagsTex;
D3DXHANDLE DistantLand::ehTerrainPatchAlbedoTex;
D3DXHANDLE DistantLand::ehTerrainBlendPatternsTex;
D3DXHANDLE DistantLand::ehTerrainWorldOrigin;
D3DXHANDLE DistantLand::ehTerrainInvAtlasSize;
D3DXHANDLE DistantLand::ehTerrainInvMaterialSize;
D3DXHANDLE DistantLand::ehTerrainLogicalTileSize;
D3DXHANDLE DistantLand::ehTerrainGutterSize;
D3DXHANDLE DistantLand::ehTerrainPhysicalTileSize;
D3DXHANDLE DistantLand::ehTerrainTilesPerRow;
D3DXHANDLE DistantLand::ehTerrainAtlasMaxLod;
D3DXHANDLE DistantLand::ehTerrainPatternCount;
D3DXHANDLE DistantLand::ehTerrainPatternTileSize;
D3DXHANDLE DistantLand::ehTerrainPatternGutterSize;
D3DXHANDLE DistantLand::ehTerrainPatternPhysicalSize;
D3DXHANDLE DistantLand::ehTerrainPatternsPerRow;
D3DXHANDLE DistantLand::ehRippleOrigin;
D3DXHANDLE DistantLand::ehWaveHeight;

std::function<void(IDirect3DSurface9*)> DistantLand::captureScreenHandler = nullptr;
bool DistantLand::captureScreenWithUI;


namespace {
    constexpr const char* TerrainShaderLogPrefix = "!! Terrain shader:";
}

// Water plane vertex declaration
const D3DVERTEXELEMENT9 WaterElem[] = {
    {0, 0,  D3DDECLTYPE_FLOAT3, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_POSITION, 0},
    D3DDECL_END()
};

// Instanced grass vertex declaration
const D3DVERTEXELEMENT9 GrassElem[] = {
    {0, 0,  D3DDECLTYPE_FLOAT16_4, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_POSITION, 0},
    {0, 8,  D3DDECLTYPE_UBYTE4N,   D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_NORMAL,   0},
    {0, 12, D3DDECLTYPE_D3DCOLOR,  D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_COLOR,    0},
    {0, 16, D3DDECLTYPE_FLOAT16_2, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_TEXCOORD, 0},
    {1, 0,  D3DDECLTYPE_FLOAT4,    D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_TEXCOORD, 1},
    {1, 16, D3DDECLTYPE_FLOAT4,    D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_TEXCOORD, 2},
    {1, 32, D3DDECLTYPE_FLOAT4,    D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_TEXCOORD, 3},
    D3DDECL_END()
};



bool DistantLand::init() {
    if (state == InitState::RenderReady || state == InitState::DeviceResourcesReady) {
        return true;
    }
    if (state == InitState::FailedDisabled) {
        return false;
    }
    if (!device) {
        return false;
    }

    LOG::logline(">> Starting Distant Land init");

    // Reset upload-pump / readiness gate state for a fresh init.
    pumpActive = false;
    uploadPhase = UploadPhase::None;
    staticsPhaseStarted = false;
    outputStatusQueryPending = false;
    outputWaitStartedMs = 0;
    outputWaitNextLogMs = 30000;
    landscapeHostVecId = IPC::InvalidVector;
    staticsHostVecId = IPC::InvalidVector;
    subsetsHostVecId = IPC::InvalidVector;
    uploadComplete = false;
    mergedStreamingCapBytes = 0;
    streamingCapOverrideActive = readMergedStreamingCapOverride(mergedStreamingCapBytes);
    automaticStreamingCapActive = false;
    nextMergedBudgetSampleMs = 0;
    lowerMergedBudgetSampleCount = 0;
    mergedBudgetSampleCount = 0;
    mergedBudgetRatchetCount = 0;
    pendingMergedCandidateCapBytes = 0;
    pendingMergedPeakMemoryUsedBytes = 0;
    peakMergedMemoryUsedBytes = 0;
    if (streamingCapOverrideActive) {
        LOG::logline(
            "-- Merged-static streaming cap override: source=MGE_DL_STREAMING_CAP_MB cap_mb=%llu cap_bytes=%llu automatic_cap=bypassed",
            static_cast<unsigned long long>(mergedStreamingCapBytes >> 20),
            static_cast<unsigned long long>(mergedStreamingCapBytes)
        );
    } else {
        LOG::logline("-- Merged-static streaming cap: override=none selection=deferred_until_fixed_resources");
    }
    worldResolved = false;
    isDistantLandLoaded = false;
    staticsUploaded = false;

    // Claim resource ownership up front so a partial failure in either phase is
    // still cleaned up by release() (which gates on hasDeviceResources()).
    state = InitState::DeviceResourcesReady;

    if (!initDeviceResources()) {
        release();
        state = InitState::FailedDisabled;
        // Replacement rendering is disabled after an init failure, so the
        // underlying indexed fixed-function path remains safe to use.
        indexedSkinningShaderCheckComplete = true;
        indexedSkinningShadersCompatible = true;
        return false;
    }

    auto mwBridge = MWBridge::get();
    MWPatches::patchResolveDuringInit(&onResolveDuringInit);

    if (!(Configuration.MGEFlags & USE_DISTANT_LAND)) {
        LOG::logline("<< Completed Distant Land device-resource init");
        state = InitState::RenderReady;
        isRenderCached = false;
        uploadComplete = true;
        worldResolved = true;
        return true;
    }

    void* playerCell = mwBridge->getPlayerCell();
    if (!playerCell) {
        // Startup/menu path: arm the frame-budgeted upload pump instead of idling
        // until the resolve hook. Present ticks it across idle menu/load frames,
        // and the render path is enabled once the pump completes AND the world
        // has resolved (see onResolveDuringInit / finalizeUploadIfReady).
        LOG::logline("-- Distant land device resources ready; arming upload pump");
        LOG::logline("<< Completed Distant Land device-resource init");
        isRenderCached = false;
        armUploadPump();
        return true;
    }

    // In-world renderer restart: player cell already active. Drive the same pump the
    // menu path uses, but to completion here: the engine is blocked inside
    // restartRenderer, so there are no frames to spread the work across.
    LOG::logline("-- Player cell already active; loading distant land immediately");
    isRenderCached = false;
    armUploadPump();
    drainUploadPump();
    if (!uploadComplete) {
        // failUpload() has already released and set FailedDisabled.
        return false;
    }

    // Only now that the drain has finished. The pump's Done phase calls
    // finalizeUploadIfReady() itself, so setting this first would promote to
    // RenderReady mid-drain and let drainUploadPump's closing loading frame render
    // through the distant path. finalizeUploadIfReady also resolves vis-group object
    // pointers, which a restart needs because the resolve-during-init hook that
    // normally does it does not fire on this path.
    worldResolved = true;
    finalizeUploadIfReady();
    return state == InitState::RenderReady;
}

// Fast device-resource phase: shaders and render targets only, no heavy geometry.
// Must stay on the startup (createScene) hook; this is the cheap part of init.
bool DistantLand::initDeviceResources() {
    indexedSkinningShaderCheckComplete = false;
    indexedSkinningShadersCompatible = false;
    vsr.init(device);
    BSA::init();

    if ((Configuration.MGEFlags & USE_DISTANT_LAND) && !initIpc()) {
        return false;
    }

    if (!initShader()) {
        return false;
    }

    if (!FixedFunctionShader::init(device, effectPool)) {
        return false;
    }

    indexedSkinningShadersCompatible =
        indexedSkinningShadersCompatible && FixedFunctionShader::supportsIndexedSkinningShaders();
    indexedSkinningShaderCheckComplete = true;
    if (Configuration.EnableIndexedSkinning && !indexedSkinningShadersCompatible) {
        LOG::logline(
            "!! Indexed skinning disabled: installed core shaders do not expose the required "
            "8-matrix color, depth, and shadow paths. Reinstall matching MGE XE core shaders.");
    }

    if (!PostShaders::init(device)) {
        return false;
    }

    if (!initDepth()) {
        return false;
    }

    if (!initShadow()) {
        return false;
    }

    if (!initWater()) {
        return false;
    }

    if (Configuration.MGEFlags & USE_DISTANT_LAND) {
        const HRESULT memoryInteropHr = device->QueryInterface(
            __uuidof(IDxvkMorrowindMemoryInterop1),
            reinterpret_cast<void**>(&dxvkMorrowindMemoryInterop)
        );
        if (FAILED(memoryInteropHr)) {
            dxvkMorrowindMemoryInterop = nullptr;
        }
    }

    return true;
}

bool DistantLand::initIpc() {
    if (!IPC::initImports()) {
        LOG::logline("!! Distant land requires memory mapping APIs (MapViewOfFile3) that are not available on this system");
        return false;
    }
    return true;
}

bool DistantLand::initIpcVectors() {
    auto maybeLandVec = ipcClient.allocVecBlocking<RenderMesh>(1, 200000, 1);
    if (!maybeLandVec.has_value()) return false;
    auto& landVec = maybeLandVec.value();
    visLandSharedId = landVec.id();
    visLandShared.SetVector((IpcClientVector(landVec)));

    auto maybeDistantVec = ipcClient.allocVecBlocking<RenderMesh>(1, 200000, 1);
    if (!maybeDistantVec.has_value()) return false;
    auto& distantVec = maybeDistantVec.value();
    visDistantSharedId = distantVec.id();
    visDistantShared.SetVector((IpcClientVector(distantVec)));

    auto maybeGrassVec = ipcClient.allocVecBlocking<RenderMesh>(MaxGrassElements, MaxGrassElements, MaxGrassElements);
    if (!maybeGrassVec.has_value()) return false;
    auto& grassVec = maybeGrassVec.value();
    visGrassSharedId = grassVec.id();
    visGrassShared.SetVector((IpcClientVector(grassVec)));

    auto maybeExtraVec = ipcClient.allocVecBlocking<RenderMesh>(1, 200000, 1);
    if (!maybeExtraVec.has_value()) return false;
    auto& extraVec = maybeExtraVec.value();
    visExtraSharedId = extraVec.id();
    visExtraShared.SetVector((IpcClientVector(extraVec)));

    auto maybeDynVisVec = ipcClient.allocVecBlocking<IPC::DynVisFlag>(1, 1000, 1);
    if (!maybeDynVisVec.has_value()) return false;
    auto& dynVisVec = maybeDynVisVec.value();
    dynVisFlagsSharedId = dynVisVec.id();
    dynVisFlagsShared = dynVisVec;

    auto maybeResidencyPlanVec = ipcClient.allocVecBlocking<IPC::ResidencyPlan>(64, 128, 64);
    if (!maybeResidencyPlanVec.has_value()) return false;
    residencyPlanSharedId = maybeResidencyPlanVec->id();
    residencyPlanShared = *maybeResidencyPlanVec;

    auto maybeResidencyCommitVec = ipcClient.allocVecBlocking<IPC::ResidencyCommit>(64, 128, 64);
    if (!maybeResidencyCommitVec.has_value()) return false;
    residencyCommitSharedId = maybeResidencyCommitVec->id();
    residencyCommitShared = *maybeResidencyCommitVec;
    return true;
}

bool DistantLand::verifyResidencyProtocol() {
    residencyCommitShared.clear();
    if (!ipcClient.updateResidency(residencyCommitSharedId)) {
        return false;
    }
    return ipcClient.waitForCompletion() == IPC::Complete && ipcClient.lastUpdateResidencySucceeded();
}

// Consume the outstanding InitLandscape RPC result and release its staging vector.
//
// Deliberately deferred to just before the statics phase issues its first RPC: the terrain
// upload fires InitLandscape non-blocking, so the host builds the land quadtree while the
// client parses and uploads distant statics (the dominant phase). Waiting any earlier would
// serialize those. The wait itself costs nothing extra here because finishStaticsPhase's
// allocVecBlocking would block on the same completion regardless; doing it explicitly is what
// lets us read the result while Command::InitLandscape is still the active command.
bool DistantLand::finishLandscapeUpload() {
    if (landscapeHostVecId == IPC::InvalidVector) {
        // Terrain upload was skipped (no terrain.bin with distant land off); nothing in flight.
        return true;
    }

    if (ipcClient.waitForCompletion() != IPC::Complete || !ipcClient.lastInitLandscapeSucceeded()) {
        LOG::logline("!! 64-bit host failed to initialize the distant land terrain quadtree");
        return false;
    }
    if (!ipcClient.freeVecBlocking(landscapeHostVecId)) {
        return false;
    }

    landscapeHostVecId = IPC::InvalidVector;
    return true;
}

bool DistantLand::reloadShaders() {
    LOG::logline(">> Distant Land reloading");
    indexedSkinningShaderCheckComplete = false;
    indexedSkinningShadersCompatible = false;
    if (!initShader()) {
        return false;
    }

    FixedFunctionShader::release();
    if (!FixedFunctionShader::init(device, effectPool)) {
        return false;
    }

    indexedSkinningShadersCompatible =
        indexedSkinningShadersCompatible && FixedFunctionShader::supportsIndexedSkinningShaders();
    indexedSkinningShaderCheckComplete = true;
    if (Configuration.EnableIndexedSkinning && !indexedSkinningShadersCompatible) {
        LOG::logline(
            "!! Shader reload rejected for indexed skinning: the core shader contract does not match.");
        return false;
    }

    return true;
}

static const string shaderCoreModPrefix = "XE Mod";
static const string pathCoreShaders = "Data Files\\shaders\\core\\";
static const string pathCoreMods = "Data Files\\shaders\\core-mods\\";

struct CoreModInclude : public ID3DXInclude {
    vector<string> modsFound;
    std::optional<string> testSingleMod;

    STDMETHOD(Open)(D3DXINCLUDE_TYPE IncludeType, LPCSTR pFileName, LPCVOID pParentData, LPCVOID *ppData, UINT *pBytes) {
        string filename(pFileName), shaderPath = filename;
        bool isMod = false;
        char *buffer = nullptr;
        HANDLE h;

        // Check if it uses the core shader path prefix, if not, add the prefix
        if (filename.compare(0, pathCoreShaders.length(), pathCoreShaders) != 0) {
            shaderPath = pathCoreShaders + filename;
        }

        if (!testSingleMod) {
            // Check if this file is moddable, and if a core-mod exists, use its path
            if (filename.substr(0, shaderCoreModPrefix.length()) == shaderCoreModPrefix) {
                string modShaderPath = pathCoreMods + filename;
                if (GetFileAttributes(modShaderPath.c_str()) != INVALID_FILE_ATTRIBUTES) {
                    isMod = true;
                    shaderPath = modShaderPath;
                }
            }
        }
        else {
            // Only load the specified mod for testing, ignoring others
            if (testSingleMod.value() == filename) {
                isMod = true;
                shaderPath = pathCoreMods + filename;
            }
        }

        // Read file contents for the effect compiler
        h = CreateFile(shaderPath.c_str(), GENERIC_READ, FILE_SHARE_READ, 0, OPEN_EXISTING, 0, 0);
        if (h != INVALID_HANDLE_VALUE) {
            DWORD bytesRead, bufferSize = GetFileSize(h, NULL);

            buffer = new char[bufferSize];
            ReadFile(h, buffer, bufferSize, &bytesRead, 0);
            CloseHandle(h);

            if (isMod) {
                modsFound.push_back(filename);
            }

            *ppData = buffer;
            *pBytes = bufferSize;
            return S_OK;
        }
        return E_FAIL;
    }

    STDMETHOD(Close)(LPCVOID pData) {
        char *buffer = (char*)(pData);
        delete [] buffer;
        return S_OK;
    }
};

static void logShaderError(ID3DXBuffer* errors) {
    if (errors) {
        LOG::write("!! Shader compile errors:\n");
        LOG::write(reinterpret_cast<const char*>(errors->GetBufferPointer()));
        LOG::write("\n");
        errors->Release();
    }
    LOG::flush();
}

// See the equivalent note in ffeshader.cpp. Core effects also commit parameters per mesh
// (distant statics, depth, shadow receivers), so CPU-side preshader evaluation scales with
// draw count. Fold the uniform-only math back into the shaders.
static const DWORD coreShaderCompileFlags = D3DXSHADER_OPTIMIZATION_LEVEL3 | D3DXSHADER_NO_PRESHADER;

static bool createCoreEffectWithMods(const char *name, IDirect3DDevice9* device, vector<D3DXMACRO>& features, ID3DXEffectPool *effectPool, ID3DXEffect **pEffect, bool reportMods) {
    string path = pathCoreShaders + name;
    ID3DXBuffer* errors;
    CoreModInclude includer;
    HRESULT hr;

    // Try all core shader macros together first. On failure, test each one to report the
    // offending mod, then fall back to compiling without core mods.
    hr = D3DXCreateEffectFromFile(device, path.c_str(), &*features.begin(), &includer, coreShaderCompileFlags|D3DXFX_LARGEADDRESSAWARE, effectPool, pEffect, &errors);
    if (hr == D3D_OK) {
        if (reportMods) {
            for (auto& m : includer.modsFound) {
                LOG::logline("-- Using core mod %s", m.c_str());
            }
        }
        return true;
    } else {
        LOG::logline("!! Core shader %s failed to compile with core-mods. All core-mods are disabled. Checking for errors...", name);
        StatusOverlay::setStatus("Shader core mod error. Core mods are disabled for this session. Check mgeXE.log for error details.", StatusOverlay::PriorityError);
        if (errors) {
            errors->Release();
        }
    }

    auto modsFound = includer.modsFound;
    for(const auto& mod : modsFound) {
        ID3DXEffect *testEffect;
        includer.testSingleMod = mod;

        hr = D3DXCreateEffectFromFile(device, path.c_str(), &*features.begin(), &includer, D3DXSHADER_OPTIMIZATION_LEVEL0|D3DXSHADER_NO_PRESHADER|D3DXFX_LARGEADDRESSAWARE, effectPool, &testEffect, &errors);
        if (hr == D3D_OK) {
            testEffect->Release();
        }
        else {
            LOG::logline("!! Shader core mod %s%s failed to compile. Disable or remove it until it is fixed.", pathCoreMods.c_str(), mod.c_str());
            logShaderError(errors);
        }
    }
    hr = D3DXCreateEffectFromFile(device, path.c_str(), &*features.begin(), 0, coreShaderCompileFlags|D3DXFX_LARGEADDRESSAWARE, effectPool, pEffect, &errors);
    if (hr == D3D_OK) {
        return true;
    } else {
        LOG::logline("!! Core shader %s failed to compile. Do not replace core shaders. Reinstall MGE XE.", name);
        logShaderError(errors);
    }
    return false;
}

static bool supportsIndexedSkinningPass(
    ID3DXEffect* shader,
    const char* shaderName,
    UINT requiredPass) {
    D3DXHANDLE palette = shader ? shader->GetParameterByName(0, "vertexBlendPalette") : 0;
    D3DXPARAMETER_DESC paletteDesc = {};
    if (!palette
        || FAILED(shader->GetParameterDesc(palette, &paletteDesc))
        || (paletteDesc.Class != D3DXPC_MATRIX_ROWS && paletteDesc.Class != D3DXPC_MATRIX_COLUMNS)
        || paletteDesc.Type != D3DXPT_FLOAT
        || paletteDesc.Rows != 4
        || paletteDesc.Columns != 4
        || paletteDesc.Elements < MGE_INDEXED_SKINNING_PALETTE_SIZE) {
        LOG::logline(
            "!! Indexed skinning shader check failed: %s has no compatible 8-matrix vertexBlendPalette.",
            shaderName);
        return false;
    }

    D3DXHANDLE technique = shader->GetTechniqueByName("T0");
    D3DXTECHNIQUE_DESC techniqueDesc = {};
    if (!technique
        || FAILED(shader->GetTechniqueDesc(technique, &techniqueDesc))
        || techniqueDesc.Passes <= requiredPass) {
        LOG::logline(
            "!! Indexed skinning shader check failed: %s is missing indexed pass %u.",
            shaderName,
            requiredPass);
        return false;
    }

    return true;
}

static const D3DXMACRO macroExpFog = { "USE_EXPFOG", "" };
static const D3DXMACRO macroScattering = { "USE_SCATTERING", "" };
static const D3DXMACRO macroFilterReflection = { "FILTER_WATER_REFLECTION", "" };
static const D3DXMACRO macroDynamicRipples = { "DYNAMIC_RIPPLES", "" };
static const D3DXMACRO macroTerminator = { 0, 0 };

bool DistantLand::initShader() {
    indexedSkinningShadersCompatible = false;
    vector<D3DXMACRO> features;
    HRESULT hr;
    D3DCAPS9 caps = {};

    // Disable exponential fog if distant land is initially off
    if (~Configuration.MGEFlags & USE_DISTANT_LAND) {
        Configuration.MGEFlags &= ~(EXP_FOG | USE_ATM_SCATTER);
    }

    // Set shader defines corresponding to required features
    if (Configuration.MGEFlags & EXP_FOG) {
        features.push_back(macroExpFog);

        // Requires exp. fog
        if (Configuration.MGEFlags & USE_ATM_SCATTER) {
            features.push_back(macroScattering);
        }
    }
    if (Configuration.MGEFlags & BLUR_REFLECTIONS) {
        features.push_back(macroFilterReflection);
    }
    if (Configuration.MGEFlags & DYNAMIC_RIPPLES) {
        features.push_back(macroDynamicRipples);
    }
    features.push_back(macroTerminator);

    if (device->GetDeviceCaps(&caps) != D3D_OK) {
        LOG::logline("%s could not query device caps before compiling XE Main.fx.", TerrainShaderLogPrefix);
        LOG::flush();
        return false;
    }
    if (caps.PixelShaderVersion < D3DPS_VERSION(3, 0)) {
        LOG::logline(
            "%s terrain rendering requires pixel shader 3.0 and tex2Dgrad support (device reports ps_%u_%u).",
            TerrainShaderLogPrefix,
            D3DSHADER_VERSION_MAJOR(caps.PixelShaderVersion),
            D3DSHADER_VERSION_MINOR(caps.PixelShaderVersion)
        );
        LOG::flush();
        return false;
    }

    if (!effectPool) {
        hr = D3DXCreateEffectPool(&effectPool);
        if (hr != D3D_OK) {
            LOG::logline("!! Effect pool creation failure");
            return false;
        }
    }

    if (!createCoreEffectWithMods("XE Main.fx", device, features, effectPool, &effect, true)) {
        LOG::logline("%s failed to compile XE Main.fx. Terrain rendering requires the shipped ps_3_0 terrain shader path and tex2Dgrad support.", TerrainShaderLogPrefix);
        LOG::flush();
        return false;
    }

    ehRcpRes = effect->GetParameterByName(0, "rcpRes");
    ehShadowRcpRes = effect->GetParameterByName(0, "shadowRcpRes");
    ehWorld = effect->GetParameterByName(0, "world");
    ehView = effect->GetParameterByName(0, "view");
    ehProj = effect->GetParameterByName(0, "proj");
    ehShadowViewproj = effect->GetParameterByName(0, "shadowViewProj");
    ehVertexBlendState = effect->GetParameterByName(0, "vertexBlendState");
    ehVertexBlendPalette = effect->GetParameterByName(0, "vertexBlendPalette");
    ehAlphaRef = effect->GetParameterByName(0, "alphaRef");
    ehMaterialAlpha = effect->GetParameterByName(0, "materialAlpha");
    ehHasAlpha = effect->GetParameterByName(0, "hasAlpha");
    ehHasBones = effect->GetParameterByName(0, "hasBones");
    ehHasVCol = effect->GetParameterByName(0, "hasVCol");
    ehUvBoundPalette = effect->GetParameterByName(0, "uvBoundPalette");
    ehTex0 = effect->GetParameterByName(0, "tex0");
    ehTex1 = effect->GetParameterByName(0, "tex1");
    ehTex2 = effect->GetParameterByName(0, "tex2");
    ehTex3 = effect->GetParameterByName(0, "tex3");
    ehEyePos = effect->GetParameterByName(0, "eyePos");
    ehFootPos = effect->GetParameterByName(0, "footPos");
    ehSunCol = effect->GetParameterByName(0, "sunCol");
    ehSunAmb = effect->GetParameterByName(0, "sunAmb");
    ehSunVec = effect->GetParameterByName(0, "sunVec");
    ehSunVecView = effect->GetParameterByName(0, "sunVecView");
    ehSunPos = effect->GetParameterByName(0, "sunPos");
    ehSunVis = effect->GetParameterByName(0, "sunVis");
    ehSkyCol = effect->GetParameterByName(0, "skyCol");
    ehFogColNear = effect->GetParameterByName(0, "fogColNear");
    ehFogColFar = effect->GetParameterByName(0, "fogColFar");
    ehFogStart = effect->GetParameterByName(0, "fogStart");
    ehFogRange = effect->GetParameterByName(0, "fogRange");
    ehFogNearStart = effect->GetParameterByName(0, "nearFogStart");
    ehFogNearRange = effect->GetParameterByName(0, "nearFogRange");
    ehNearViewRange = effect->GetParameterByName(0, "nearViewRange");
    ehWindVec = effect->GetParameterByName(0, "windVec");
    ehNiceWeather = effect->GetParameterByName(0, "niceWeather");
    ehTime = effect->GetParameterByName(0, "time");
    ehTerrainAtlasTex = effect->GetParameterByName(0, "terrainAtlasTex");
    ehTerrainMaterialTex = effect->GetParameterByName(0, "terrainMaterialTex");
    ehTerrainMaterialFlagsTex = effect->GetParameterByName(0, "terrainMaterialFlagsTex");
    ehTerrainPatchAlbedoTex = effect->GetParameterByName(0, "terrainPatchAlbedoTex");
    ehTerrainBlendPatternsTex = effect->GetParameterByName(0, "terrainBlendPatternsTex");
    ehTerrainWorldOrigin = effect->GetParameterByName(0, "terrainWorldOrigin");
    ehTerrainInvAtlasSize = effect->GetParameterByName(0, "terrainInvAtlasSize");
    ehTerrainInvMaterialSize = effect->GetParameterByName(0, "terrainInvMaterialSize");
    ehTerrainLogicalTileSize = effect->GetParameterByName(0, "terrainLogicalTileSize");
    ehTerrainGutterSize = effect->GetParameterByName(0, "terrainGutterSize");
    ehTerrainPhysicalTileSize = effect->GetParameterByName(0, "terrainPhysicalTileSize");
    ehTerrainTilesPerRow = effect->GetParameterByName(0, "terrainTilesPerRow");
    ehTerrainAtlasMaxLod = effect->GetParameterByName(0, "terrainAtlasMaxLod");
    ehTerrainPatternCount = effect->GetParameterByName(0, "terrainPatternCount");
    ehTerrainPatternTileSize = effect->GetParameterByName(0, "terrainPatternTileSize");
    ehTerrainPatternGutterSize = effect->GetParameterByName(0, "terrainPatternGutterSize");
    ehTerrainPatternPhysicalSize = effect->GetParameterByName(0, "terrainPatternPhysicalSize");
    ehTerrainPatternsPerRow = effect->GetParameterByName(0, "terrainPatternsPerRow");

    D3DVIEWPORT9 vp;
    device->GetViewport(&vp);
    float rcpres[2] = { 1.0f / vp.Width, 1.0f / vp.Height };
    effect->SetFloatArray(ehRcpRes, rcpres, 2);
    effect->SetFloat(ehShadowRcpRes, 1.0f / Configuration.DL.ShadowResolution);

    if (!createCoreEffectWithMods("XE Shadowmap.fx", device, features, effectPool, &effectShadow, false)) {
        return false;
    }
    if (!createCoreEffectWithMods("XE Depth.fx", device, features, effectPool, &effectDepth, false)) {
        LOG::logline("%s failed to compile XE Depth.fx for the terrain depth path.", TerrainShaderLogPrefix);
        LOG::flush();
        return false;
    }
    ehDepthSrc = effectDepth->GetParameterByName(0, "texDepthSrc");
    ehSourceM33 = effectDepth->GetParameterByName(0, "sourceM33");
    ehSourceM43 = effectDepth->GetParameterByName(0, "sourceM43");

    const struct {
        D3DXHANDLE handle;
        const char* name;
    } requiredTerrainHandles[] = {
        { ehTerrainAtlasTex, "terrainAtlasTex" },
        { ehTerrainMaterialTex, "terrainMaterialTex" },
        { ehTerrainMaterialFlagsTex, "terrainMaterialFlagsTex" },
        { ehTerrainPatchAlbedoTex, "terrainPatchAlbedoTex" },
        { ehTerrainBlendPatternsTex, "terrainBlendPatternsTex" },
        { ehTerrainWorldOrigin, "terrainWorldOrigin" },
        { ehTerrainInvAtlasSize, "terrainInvAtlasSize" },
        { ehTerrainInvMaterialSize, "terrainInvMaterialSize" },
        { ehTerrainLogicalTileSize, "terrainLogicalTileSize" },
        { ehTerrainGutterSize, "terrainGutterSize" },
        { ehTerrainPhysicalTileSize, "terrainPhysicalTileSize" },
        { ehTerrainTilesPerRow, "terrainTilesPerRow" },
        { ehTerrainAtlasMaxLod, "terrainAtlasMaxLod" },
        { ehTerrainPatternCount, "terrainPatternCount" },
        { ehTerrainPatternTileSize, "terrainPatternTileSize" },
        { ehTerrainPatternGutterSize, "terrainPatternGutterSize" },
        { ehTerrainPatternPhysicalSize, "terrainPatternPhysicalSize" },
        { ehTerrainPatternsPerRow, "terrainPatternsPerRow" }
    };
    for (const auto& requiredHandle : requiredTerrainHandles) {
        if (!requiredHandle.handle) {
            LOG::logline("%s missing required parameter '%s' in XE Main.fx. Reinstall the core shaders.", TerrainShaderLogPrefix, requiredHandle.name);
            LOG::flush();
            return false;
        }
    }

    const auto validateTerrainTechnique = [](ID3DXEffect* terrainEffect, const char* effectName) {
        D3DXHANDLE technique = terrainEffect->GetTechniqueByName("T0");
        if (!technique) {
            LOG::logline("%s %s is missing terrain technique T0.", TerrainShaderLogPrefix, effectName);
            return false;
        }

        HRESULT validateHr = terrainEffect->ValidateTechnique(technique);
        if (validateHr != D3D_OK) {
            LOG::logline("%s %s cannot run terrain technique T0 on this device (HRESULT=0x%08lx).", TerrainShaderLogPrefix, effectName, validateHr);
            return false;
        }

        return true;
    };
    if (!validateTerrainTechnique(effect, "XE Main.fx")
        || !validateTerrainTechnique(effectDepth, "XE Depth.fx")) {
        LOG::flush();
        return false;
    }

    indexedSkinningShadersCompatible = !Configuration.EnableIndexedSkinning
        || (supportsIndexedSkinningPass(effect, "XE Main.fx", PASS_RENDERSHADOWFFE_INDEXED)
            && supportsIndexedSkinningPass(effectDepth, "XE Depth.fx", PASS_RENDERMWDEPTH_INDEXED));

    // Atmosphere scattering specific parameters
    if (Configuration.MGEFlags & USE_ATM_SCATTER) {

        ehOutscatter = effect->GetParameterByName(0, "outscatter");
        ehInscatter = effect->GetParameterByName(0, "inscatter");
        ehSkyScatterFar = effect->GetParameterByName(0, "skyScatterColFar");

        // Mark moon geometry for detection
        MWBridge::get()->markMoonNodes(kMoonTag);
    }
    else {
        ehOutscatter = 0;
        ehInscatter = 0;
        ehSkyScatterFar = 0;
    }

    // Dynamic ripples specific parameters
    if (Configuration.MGEFlags & DYNAMIC_RIPPLES) {
        ehTex4 = effect->GetParameterByName(0, "tex4");
        ehTex5 = effect->GetParameterByName(0, "tex5");
        ehRippleOrigin = effect->GetParameterByName(0, "rippleOrigin");
        ehWaveHeight = effect->GetParameterByName(0, "waveHeight");
    }

    return true;
}

bool DistantLand::initDepth() {
    HRESULT hr;
    D3DVIEWPORT9 vp;

    nativeDepthBackend = NativeDepthBackend::None;
    if (dxvkMorrowindInterop) {
        dxvkMorrowindInterop->Release();
        dxvkMorrowindInterop = nullptr;
    }
    stage1UsedNativeDepth = false;
    nativeStage2Eligible = false;
    dsvMayBeNoncanonical = true;
    ProxyDevice::setDepthStencilSubstitute(nullptr, nullptr);

    // Set up the legacy depth-frame outputs used by both replay and native conversion.
    device->GetViewport(&vp);

    hr = device->CreateTexture(vp.Width, vp.Height, 1, D3DUSAGE_RENDERTARGET, D3DFMT_R32F, D3DPOOL_DEFAULT, &texDepthFrame, NULL);
    if (hr != D3D_OK) {
        LOG::logline("!! Failed to create depth frame render target");
        return false;
    }

    hr = device->CreateDepthStencilSurface(vp.Width, vp.Height, D3DFMT_D24X8, D3DMULTISAMPLE_NONE, 0, FALSE, &surfDepthDepth, NULL);
    if (hr != D3D_OK) {
        LOG::logline("!! Failed to create depth target z-buffer");
        return false;
    }

    if (!Configuration.EnableNativeDepthCapture) {
        return true;
    }

    IDirect3DSurface9* mainTarget = nullptr;
    D3DSURFACE_DESC mainTargetDesc = {};
    hr = device->GetRenderTarget(0, &mainTarget);
    if (SUCCEEDED(hr)) {
        hr = mainTarget->GetDesc(&mainTargetDesc);
        mainTarget->Release();
    }
    if (FAILED(hr)) {
        LOG::logline("!! Native depth capture could not inspect the main render target (hr=0x%08lx); using geometry replay", static_cast<unsigned long>(hr));
        return true;
    }
    if (vp.Width != mainTargetDesc.Width || vp.Height != mainTargetDesc.Height) {
        LOG::logline(
            "-- Native depth capture requires matching viewport/backbuffer extents (viewport %ux%u, target %ux%u); using geometry replay",
            vp.Width,
            vp.Height,
            mainTargetDesc.Width,
            mainTargetDesc.Height
        );
        return true;
    }
    if (!ehDepthSrc || !ehSourceM33 || !ehSourceM43) {
        LOG::logline("!! Native depth capture shader parameters are unavailable; using geometry replay");
        return true;
    }

    auto nativeDepthUnavailable = [&](const char* step, HRESULT failure) {
        ProxyDevice::setDepthStencilSubstitute(nullptr, nullptr);
        if (surfAutoDepthStencil) {
            device->SetDepthStencilSurface(surfAutoDepthStencil);
        }
        nativeDepthBackend = NativeDepthBackend::None;
        if (dxvkMorrowindInterop) {
            dxvkMorrowindInterop->Release();
            dxvkMorrowindInterop = nullptr;
        }
        if (surfAutoDepthStencil) {
            surfAutoDepthStencil->Release();
            surfAutoDepthStencil = nullptr;
        }
        if (surfDepthStencil) {
            surfDepthStencil->Release();
            surfDepthStencil = nullptr;
        }
        if (texDepthStencil) {
            texDepthStencil->Release();
            texDepthStencil = nullptr;
        }
        LOG::logline(
            "!! Native depth capture failed at %s (hr=0x%08lx); using geometry replay",
            step,
            static_cast<unsigned long>(failure)
        );
    };

    hr = device->GetDepthStencilSurface(&surfAutoDepthStencil);
    if (FAILED(hr)) {
        nativeDepthUnavailable("automatic depth-stencil retention", hr);
        return true;
    }

    D3DSURFACE_DESC autoDepthDesc = {};
    hr = surfAutoDepthStencil->GetDesc(&autoDepthDesc);
    if (FAILED(hr)) {
        nativeDepthUnavailable("automatic depth-stencil inspection", hr);
        return true;
    }

    if (static_cast<UINT>(autoDepthDesc.MultiSampleType) != static_cast<UINT>(Configuration.AALevel)) {
        LOG::logline(
            "-- Native depth capture AA setting differs from the active depth-stencil (configured %u, active sample type %u); using the active surface",
            static_cast<unsigned>(Configuration.AALevel),
            static_cast<unsigned>(autoDepthDesc.MultiSampleType)
        );
    }

    hr = device->CreateTexture(
        vp.Width,
        vp.Height,
        1,
        D3DUSAGE_DEPTHSTENCIL,
        kFormatIntz,
        D3DPOOL_DEFAULT,
        &texDepthStencil,
        nullptr
    );
    if (FAILED(hr)) {
        nativeDepthUnavailable("INTZ texture creation", hr);
        return true;
    }

    hr = texDepthStencil->GetSurfaceLevel(0, &surfDepthStencil);
    if (FAILED(hr)) {
        nativeDepthUnavailable("INTZ surface acquisition", hr);
        return true;
    }

    if (autoDepthDesc.MultiSampleType == D3DMULTISAMPLE_NONE) {
        hr = device->SetDepthStencilSurface(surfDepthStencil);
        if (FAILED(hr)) {
            nativeDepthUnavailable("INTZ depth-stencil binding", hr);
            return true;
        }

        ProxyDevice::setDepthStencilSubstitute(surfAutoDepthStencil, surfDepthStencil);
        nativeDepthBackend = NativeDepthBackend::IntzMainDsv;
        LOG::logline("-- Native depth capture armed with an INTZ main depth-stencil");
        return true;
    }

    hr = device->QueryInterface(
        __uuidof(IDxvkMorrowindInterop),
        reinterpret_cast<void**>(&dxvkMorrowindInterop)
    );
    if (FAILED(hr)) {
        nativeDepthUnavailable("DXVK Morrowind interop query", hr);
        return true;
    }

    const uint32_t interfaceVersion = dxvkMorrowindInterop->GetInterfaceVersion();
    if (interfaceVersion != DXVK_MORROWIND_INTEROP_VERSION) {
        LOG::logline(
            "!! Native depth capture found unsupported DXVK Morrowind interop version %u (expected %u)",
            static_cast<unsigned>(interfaceVersion),
            static_cast<unsigned>(DXVK_MORROWIND_INTEROP_VERSION)
        );
        nativeDepthUnavailable("DXVK Morrowind interop version check", E_NOINTERFACE);
        return true;
    }

    const uint64_t capabilities = dxvkMorrowindInterop->GetCapabilities();
    if (!(capabilities & DXVK_MORROWIND_CAP_MSAA_DEPTH_RESOLVE)) {
        nativeDepthUnavailable("DXVK MSAA depth-resolve capability check", D3DERR_NOTAVAILABLE);
        return true;
    }

    nativeDepthBackend = NativeDepthBackend::DxvkMsaaResolve;
    LOG::logline(
        "-- Native depth capture armed with DXVK MSAA resolve (format 0x%08lx, sample type %u, interop v%u, caps 0x%08lx%08lx)",
        static_cast<unsigned long>(autoDepthDesc.Format),
        static_cast<unsigned>(autoDepthDesc.MultiSampleType),
        static_cast<unsigned>(interfaceVersion),
        static_cast<unsigned long>(capabilities >> 32),
        static_cast<unsigned long>(capabilities & 0xffffffffu)
    );

    return true;
}

bool DistantLand::initWater() {
    HRESULT hr;
    const UINT reflRes = 1024;

    // Reflection render target
    hr = device->CreateTexture(reflRes, reflRes, 1, D3DUSAGE_RENDERTARGET, D3DFMT_A8R8G8B8, D3DPOOL_DEFAULT, &texReflection, NULL);
    if (hr != D3D_OK) {
        LOG::logline("!! Failed to create reflection render target");
        return false;
    }

    // Reflection Z-buffer
    hr = device->CreateDepthStencilSurface(reflRes, reflRes, D3DFMT_D24X8, D3DMULTISAMPLE_NONE, 0, TRUE, &surfReflectionZ, NULL);
    if (hr != D3D_OK) {
        LOG::logline("!! Failed to create reflection Z buffer");
        return false;
    }

    // Water normals and geometry
    const int resS = (Configuration.MGEFlags & DYNAMIC_RIPPLES) ? 150 : 16;
    const int resT = (Configuration.MGEFlags & DYNAMIC_RIPPLES) ? 120 : 15;
    numWaterVerts = resS * resT + 1;
    numWaterTris = 2 * resS * resT - resS;

    hr = D3DXCreateVolumeTextureFromFile(device, "Data Files\\textures\\MGE\\water_NRM.dds", &texWater);
    if (hr != D3D_OK) {
        LOG::logline("!! Failed to load water texture");
        return false;
    }
    hr = device->CreateVertexDeclaration(WaterElem, &WaterDecl);
    if (hr != D3D_OK) {
        LOG::logline("!! Failed to create water decl");
        return false;
    }
    hr = device->CreateVertexBuffer(numWaterVerts * 12, 0, 0, D3DPOOL_MANAGED, &vbWater, 0);
    if (hr != D3D_OK) {
        LOG::logline("!! Failed to create water verts");
        return false;
    }
    hr = device->CreateIndexBuffer(numWaterTris * 6, 0, D3DFMT_INDEX16, D3DPOOL_MANAGED, &ibWater, 0);
    if (hr != D3D_OK) {
        LOG::logline("!! Failed to create water indices");
        return false;
    }

    // Build radial water mesh
    D3DXVECTOR3* v;
    vbWater->Lock(0, 0, (void**)&v, 0);

    // Water plane lies at water level - 1.0 (not -4.0, which is the fog transition)
    const float dS = float(6.28318530717958647692 / resS);
    int s, t;
    float r, w = -1.0f;

    *v++ = D3DXVECTOR3(0, 0, w);
    for (t = 0; t < resT; ++t) {
        if (Configuration.MGEFlags & DYNAMIC_RIPPLES) {
            // Higher mesh density near player
            // The mesh requires density past 8192 units to cover the z discontinuity at distant land
            r = float(t) / float(resT);
            r = 9600.0f * (0.9f * powf(r, 3) + 0.1f * r);
            // Extend last ring past horizon
            if ((t+1) == resT) {
                r = 500000.0f;
            }
        } else {
            r = 4096.0f * (1.0f + t * t);
        }

        for (s = 0; s < resS; ++s) {
            *v++ = D3DXVECTOR3(r * cos(dS * s), r * sin(dS * s), w);
        }
    }

    vbWater->Unlock();

    USHORT* i;
    ibWater->Lock(0, 0, (void**)&i, 0);

    // Centre triangles
    for (s = 0; s < resS; ++s) {
        *i++ = 0;
        *i++ = 1 + s;
        *i++ = 1 + (s+1) % resS;
    }
    // Rings
    for (t = 1; t < resT; ++t) {
        for (s = 0; s < resS; ++s) {
            USHORT tbase = 1 + resS*(t-1), s2 = (s+1) % resS;
            *i++ = tbase + s;
            *i++ = resS + tbase + s;
            *i++ = tbase + s2;
            *i++ = resS + tbase+ s;
            *i++ = resS + tbase + s2;
            *i++ = tbase + s2;
        }
    }

    ibWater->Unlock();

    if (Configuration.MGEFlags & DYNAMIC_RIPPLES) {
        // Setup water simulation
        if (!initDynamicWaves()) {
            return false;
        }

        // Disable Morrowind generated ripples
        MWBridge::get()->toggleRipples(false);
    }

    return true;
}

bool DistantLand::initDynamicWaves() {
    HRESULT hr;

    hr = device->CreateTexture(waveTexResolution, waveTexResolution, 1, D3DUSAGE_RENDERTARGET, D3DFMT_A16B16G16R16F, D3DPOOL_DEFAULT, &texRain, NULL);
    if (hr != D3D_OK) {
        LOG::logline("!! Failed to create rain simulation texture");
        return false;
    }
    texRain->GetSurfaceLevel(0, &surfRain);
    device->ColorFill(surfRain, 0, 0);

    hr = device->CreateTexture(waveTexResolution, waveTexResolution, 1, D3DUSAGE_RENDERTARGET, D3DFMT_A16B16G16R16F, D3DPOOL_DEFAULT, &texRipples, NULL);
    if (hr != D3D_OK) {
        LOG::logline("!! Failed to create ripple simulation texture");
        return false;
    }
    texRipples->GetSurfaceLevel(0, &surfRipples);
    device->ColorFill(surfRipples, 0, 0);

    hr = device->CreateTexture(waveTexResolution, waveTexResolution, 1, D3DUSAGE_RENDERTARGET, D3DFMT_A16B16G16R16F, D3DPOOL_DEFAULT, &texRippleBuffer, NULL);
    if (hr != D3D_OK) {
        LOG::logline("!! Failed to create ripple simulation texture");
        return false;
    }
    texRippleBuffer->GetSurfaceLevel(0, &surfRippleBuffer);
    device->ColorFill(surfRippleBuffer, 0, 0);

    // Vertex buffer for wave texture
    static float waveVertices[] = {
        /*     -0.5f,                    -0.5f,                                               0,1,   0,0,0,0,
                -0.5f,                    waveTexResolution-0.5f,                 0,1,   0,1,0,1,
                waveTexResolution-0.5f,    -0.5f,                                  0,1,   1,0,1,0,
                waveTexResolution-0.5f,    waveTexResolution-0.5f,    0,1,   1,1,1,1 */

        // Use only one tri over the whole texture to prevent simulation seams at tri edges
        // Rendering to a surface that is bound as a source texture updates the texture after
        // each primitive, causing artifacts to appear at primitive boundaries
        -waveTexResolution/2  -0.5f,    waveTexResolution/2  -0.5f,  0,  1,     -0.5, 0.5,     0,0,
        waveTexResolution        -0.5f,    2*waveTexResolution  -0.5f,  0,  1,      1.0, 2.0,      0,1,
        waveTexResolution        -0.5f,    -waveTexResolution    -0.5f,  0,  1,     1.0, -1.0,     1,1
    };

    void* vp;
    hr = device->CreateVertexBuffer(3 * 32, D3DUSAGE_WRITEONLY, fvfWave, D3DPOOL_DEFAULT, &vbWaveSim, 0);
    if (hr != D3D_OK) {
        LOG::logline("!! Failed to create wave simulation vb");
        return false;
    }
    if (vbWaveSim->Lock(0, 0, (void**)&vp, 0) != D3D_OK) {
        LOG::logline("!! Failed to lock wave simulation vb");
        return false;
    }
    memcpy(vp, waveVertices, sizeof(waveVertices));
    vbWaveSim->Unlock();

    return true;
}

bool DistantLand::initShadow() {
    const D3DFORMAT shadowFormat = D3DFMT_R16F, shadowZFormat = D3DFMT_D24S8;
    const UINT shadowSize = Configuration.DL.ShadowResolution, cascades = kShadowCascades;
    HRESULT hr;

    // The shadow texture holds a horizontal-packed shadow atlas
    hr = device->CreateTexture(cascades * shadowSize, shadowSize, 1, D3DUSAGE_RENDERTARGET, shadowFormat, D3DPOOL_DEFAULT, &texShadow, NULL);
    if (hr != D3D_OK) {
        LOG::logline("!! Failed to create shadow render target");
        return false;
    }
    hr = device->CreateTexture(cascades * shadowSize, shadowSize, 1, D3DUSAGE_RENDERTARGET, shadowFormat, D3DPOOL_DEFAULT, &texSoftShadow, NULL);
    if (hr != D3D_OK) {
        LOG::logline("!! Failed to create shadow render target");
        return false;
    }
    hr = device->CreateDepthStencilSurface(cascades * shadowSize, shadowSize, shadowZFormat, D3DMULTISAMPLE_NONE, 0, TRUE, &surfShadowZ, NULL);
    if (hr != D3D_OK) {
        LOG::logline("!! Failed to create shadow Z buffer");
        return false;
    }
    hr = device->CreateVertexBuffer(4 * 12, D3DUSAGE_WRITEONLY, 0, D3DPOOL_DEFAULT, &vbFullFrame, 0);
    if (hr != D3D_OK) {
        LOG::logline("!! Failed to create shadow processing verts");
        return false;
    }

    // Used to cover an entire render target of any dimension
    D3DXVECTOR3* v;
    vbFullFrame->Lock(0, 0, (void**)&v, 0);
    v[0] = D3DXVECTOR3( -1.0f, 1.0f,  1.0f);
    v[1] = D3DXVECTOR3(-1.0f, -1.0f,  1.0f);
    v[2] = D3DXVECTOR3( 1.0f,  1.0f,  1.0f);
    v[3] = D3DXVECTOR3( 1.0f, -1.0f,  1.0f);
    vbFullFrame->Unlock();

    return true;
}

// Resumable distant-statics loader. All loop state lives here so the per-subset
// upload can yield (when its frame budget is spent) and resume on a later frame.
// The output records are buffered into plain std::vectors; on the shared-memory
// path they are streamed into IPC vectors once at phase completion (the ~2.5 GiB
// of geometry never enters these vectors. It is copied straight into D3D buffers).
void DistantLand::armUploadPump() {
    pumpActive = true;
    uploadPhase = UploadPhase::HostWait;
    staticsPhaseStarted = false;
    outputStatusQueryPending = false;
    outputWaitStartedMs = 0;
    outputWaitNextLogMs = 30000;
    landscapeHostVecId = IPC::InvalidVector;
    staticsHostVecId = IPC::InvalidVector;
    subsetsHostVecId = IPC::InvalidVector;
    LOG::logline("-- Distant land upload pump armed");
}

float DistantLand::drainProgressPct() {
    switch (uploadPhase) {
    case UploadPhase::HostWait:
    case UploadPhase::OutputWait:
    case UploadPhase::IpcSetup:
        return 0.0f;

    case UploadPhase::Landscape:
        return 5.0f;

    case UploadPhase::Statics:
        return 10.0f + 85.0f * DistantLoaders::staticsProgressRatio();

    case UploadPhase::Grass:
    case UploadPhase::StaticsHostWait:
        return 95.0f;

    case UploadPhase::ResidencyFullDrain:
    case UploadPhase::ResidencyBootstrap:
        return 98.0f;

    case UploadPhase::Done:
        return 100.0f;

    case UploadPhase::None:
    default:
        return 0.0f;
    }
}

// Drain the upload pump synchronously from inside the engine's load path, so the
// render path is ready before the player enters gameplay. Loading-screen updates use
// the native throttled cadence; the first slice may create the bar.
void DistantLand::drainUploadPump() {
    auto mwBridge = MWBridge::get();
    const bool hadLoadingBar = mwBridge->isLoadingBar();

    std::string savedLabel;
    const bool snapshotOk = hadLoadingBar && mwBridge->getLoadingBarLabel(savedLabel);
    const bool mayOwnLabel = !hadLoadingBar || snapshotOk;

    char buffer[64];
    const char* loadingMessage = *(const char**)mwBridge->getGMSTPointer(602);
    int firstWordLength = 0;
    for (const char* c = loadingMessage; *c; ++c) {
        if (*c == ' ') { break; }
        ++firstWordLength;
    }
    std::snprintf(buffer, sizeof(buffer), "%.*s MGE XE distant land...", firstWordLength, loadingMessage);

    bool pumpRan = false;
    auto restoreLoadingBar = [&]() {
        if (!pumpRan) {
            return;
        }
        if (!hadLoadingBar) {
            mwBridge->destroyLoadingBar();
        } else if (snapshotOk) {
            mwBridge->setLoadingBarLabel(savedLabel.c_str());
        }
    };

    pumpDraining = true;
    char cleanupToken = 0;
    auto cleanup = [&](char*) {
        restoreLoadingBar();
        pumpDraining = false;
    };
    std::unique_ptr<char, decltype(cleanup)> cleanupGuard(&cleanupToken, cleanup);

    const char* lastLoadingText = nullptr;
    while (pumpActive) {
        pumpRan = true;
        pumpUploadTick(kDrainBudgetMs);
        const bool waitingForGeneration = uploadPhase == UploadPhase::HostWait || uploadPhase == UploadPhase::OutputWait;
        // The bootstrap phase is I/O-bound on the reader thread, so yield rather than spinning.
        const bool waitingForHost = waitingForGeneration
            || uploadPhase == UploadPhase::StaticsHostWait
            || uploadPhase == UploadPhase::ResidencyBootstrap;
        const char* loadingText = waitingForGeneration ? "MGE XE generating..." : buffer;
        const float progress = drainProgressPct();

        if (!lastLoadingText) {
            if (mayOwnLabel) {
                mwBridge->showLoadingBar(loadingText, progress);
            } else {
                // The defensive active-flag/no-menu case is unreachable here; the native
                // updater may skip its fill and present, so do not add an unconditional frame.
                mwBridge->updateLoadingScreen(progress);
            }
        } else {
            if (mayOwnLabel && std::strcmp(loadingText, lastLoadingText) != 0) {
                mwBridge->setLoadingBarLabel(loadingText);
            }
            mwBridge->updateLoadingScreen(progress);
        }
        lastLoadingText = loadingText;

        if (waitingForHost) {
            Sleep(10);
        }
    }

    const bool labelRestored = pumpRan && snapshotOk;
    restoreLoadingBar();
    if (labelRestored) {
        mwBridge->renderLoadingFrame();
    }
    pumpDraining = false;
    cleanupGuard.release();
}

// Abort an in-flight pump and drop any partial statics-loader state.
void DistantLand::abortUploadPump() {
    // Join the reader before anything it reads from can go away. Idempotent.
    DistantLoaders::haltResidency();
    pumpActive = false;
    uploadPhase = UploadPhase::None;
    staticsPhaseStarted = false;
    outputStatusQueryPending = false;
    outputWaitStartedMs = 0;
    outputWaitNextLogMs = 30000;
    landscapeHostVecId = IPC::InvalidVector;
    staticsHostVecId = IPC::InvalidVector;
    subsetsHostVecId = IPC::InvalidVector;
    abortStaticsPhase();
}

// Upload failure during the pump: tear everything down and disable distant land,
// mirroring the synchronous failure path.
void DistantLand::failUpload() {
    LOG::logline("!! Distant land upload failed; disabling distant land");
    release();   // releases device resources and (via abortUploadPump) the in-flight pump
    state = InitState::FailedDisabled;
    Configuration.MGEFlags &= ~USE_DISTANT_LAND;
    StatusOverlay::setStatus("MGE XE serious error condition. Exit Morrowind and check mgeXE.log for details.", StatusOverlay::PriorityError);
}

bool DistantLand::selectInitialMergedStreamingCap() {
    const std::uint64_t totalMergedBytes = DistantLoaders::totalMergedGeometryBytes();
    if (streamingCapOverrideActive) {
        LOG::logline(
            "-- Merged-static cap selected: source=override cap=%llu B merged_total=%llu B binds=%d",
            static_cast<unsigned long long>(mergedStreamingCapBytes),
            static_cast<unsigned long long>(totalMergedBytes),
            totalMergedBytes > mergedStreamingCapBytes ? 1 : 0
        );
        return true;
    }

    if (!dxvkMorrowindMemoryInterop) {
        mergedStreamingCapBytes = std::numeric_limits<std::uint64_t>::max();
        LOG::logline(
            "-- Merged-static cap selected: source=unsupported_or_native_d3d9 cap=infinite merged_total=%llu B schedule=full_drain",
            static_cast<unsigned long long>(totalMergedBytes)
        );
        return true;
    }

    std::uint64_t heapBudget = 0;
    std::uint64_t memoryUsed = 0;
    const HRESULT hr = dxvkMorrowindMemoryInterop->GetDeviceLocalMemoryBudgetV1(&heapBudget, &memoryUsed);
    if (FAILED(hr) || heapBudget == 0) {
        mergedStreamingCapBytes = std::numeric_limits<std::uint64_t>::max();
        LOG::logline(
            "!! DXVK merged-static budget query unavailable or invalid: hr=0x%08lx heap_budget=%llu B memory_used=%llu B; using infinite cap and full drain",
            static_cast<unsigned long>(hr),
            static_cast<unsigned long long>(heapBudget),
            static_cast<unsigned long long>(memoryUsed)
        );
        return true;
    }

    const std::uint64_t logicalGpuMergedBytes = DistantLoaders::logicalGpuMergedGeometryBytes();
    std::uint64_t headroom = 0;
    const std::uint64_t candidate = mergedCapCandidate(
        heapBudget,
        memoryUsed,
        logicalGpuMergedBytes,
        headroom
    );
    mergedStreamingCapBytes = candidate;
    automaticStreamingCapActive = true;
    mergedBudgetSampleCount = 1;
    peakMergedMemoryUsedBytes = memoryUsed;
    nextMergedBudgetSampleMs = HighResolutionTimer::getMilliseconds() + kMergedBudgetSampleIntervalMs;
    LOG::logline(
        "-- Merged-static cap sample: initial=1 heap_budget=%llu B memory_used=%llu B headroom=%llu B logical_gpu_merged=%llu B candidate_cap=%llu B merged_total=%llu B binds=%d",
        static_cast<unsigned long long>(heapBudget),
        static_cast<unsigned long long>(memoryUsed),
        static_cast<unsigned long long>(headroom),
        static_cast<unsigned long long>(logicalGpuMergedBytes),
        static_cast<unsigned long long>(candidate),
        static_cast<unsigned long long>(totalMergedBytes),
        totalMergedBytes > candidate ? 1 : 0
    );
    return true;
}

void DistantLand::sampleMergedStreamingCap() {
    if (streamingCapOverrideActive || !automaticStreamingCapActive || !dxvkMorrowindMemoryInterop) {
        return;
    }

    const int nowMs = HighResolutionTimer::getMilliseconds();
    if (nowMs < nextMergedBudgetSampleMs) {
        return;
    }
    nextMergedBudgetSampleMs = nowMs + kMergedBudgetSampleIntervalMs;

    std::uint64_t heapBudget = 0;
    std::uint64_t memoryUsed = 0;
    const HRESULT hr = dxvkMorrowindMemoryInterop->GetDeviceLocalMemoryBudgetV1(&heapBudget, &memoryUsed);
    if (FAILED(hr) || heapBudget == 0) {
        lowerMergedBudgetSampleCount = 0;
        pendingMergedCandidateCapBytes = 0;
        pendingMergedPeakMemoryUsedBytes = 0;
        LOG::logline(
            "!! DXVK merged-static budget resample ignored: hr=0x%08lx heap_budget=%llu B memory_used=%llu B",
            static_cast<unsigned long>(hr),
            static_cast<unsigned long long>(heapBudget),
            static_cast<unsigned long long>(memoryUsed)
        );
        return;
    }

    ++mergedBudgetSampleCount;
    peakMergedMemoryUsedBytes = std::max(peakMergedMemoryUsedBytes, memoryUsed);
    const std::uint64_t logicalGpuMergedBytes = DistantLoaders::logicalGpuMergedGeometryBytes();
    std::uint64_t headroom = 0;
    const std::uint64_t candidate = mergedCapCandidate(
        heapBudget,
        memoryUsed,
        logicalGpuMergedBytes,
        headroom
    );
    if (candidate >= mergedStreamingCapBytes) {
        lowerMergedBudgetSampleCount = 0;
        pendingMergedCandidateCapBytes = 0;
        pendingMergedPeakMemoryUsedBytes = 0;
        return;
    }

    if (lowerMergedBudgetSampleCount == 0) {
        pendingMergedPeakMemoryUsedBytes = memoryUsed;
    } else {
        pendingMergedPeakMemoryUsedBytes = std::max(pendingMergedPeakMemoryUsedBytes, memoryUsed);
    }
    // Apply the fourth consecutive lower candidate, not the minimum of the window. A single
    // transient spike may begin confirmation, but it must not pin the session to that low point.
    pendingMergedCandidateCapBytes = candidate;
    ++lowerMergedBudgetSampleCount;
    LOG::logline(
        "-- Merged-static cap sample: initial=0 heap_budget=%llu B memory_used=%llu B headroom=%llu B logical_gpu_merged=%llu B candidate_cap=%llu B active_cap=%llu B lower_confirmation=%lu/%lu",
        static_cast<unsigned long long>(heapBudget),
        static_cast<unsigned long long>(memoryUsed),
        static_cast<unsigned long long>(headroom),
        static_cast<unsigned long long>(logicalGpuMergedBytes),
        static_cast<unsigned long long>(candidate),
        static_cast<unsigned long long>(mergedStreamingCapBytes),
        lowerMergedBudgetSampleCount,
        kMergedBudgetRatchetSamples
    );

    if (lowerMergedBudgetSampleCount < kMergedBudgetRatchetSamples) {
        return;
    }

    const std::uint64_t previousCap = mergedStreamingCapBytes;
    mergedStreamingCapBytes = std::min(mergedStreamingCapBytes, pendingMergedCandidateCapBytes);
    ++mergedBudgetRatchetCount;
    LOG::logline(
        "-- Merged-static cap ratchet: previous=%llu B active=%llu B confirmation_peak_memory_used=%llu B samples=%lu",
        static_cast<unsigned long long>(previousCap),
        static_cast<unsigned long long>(mergedStreamingCapBytes),
        static_cast<unsigned long long>(pendingMergedPeakMemoryUsedBytes),
        kMergedBudgetRatchetSamples
    );
    lowerMergedBudgetSampleCount = 0;
    pendingMergedCandidateCapBytes = 0;
    pendingMergedPeakMemoryUsedBytes = 0;
    DistantLoaders::wakeResidencyForCapDebt();
}

void DistantLand::logMergedStreamingBudgetSummary() {
    if (!automaticStreamingCapActive) {
        return;
    }
    LOG::logline(
        "-- Merged-static budget summary: samples=%lu ratchets=%lu active_cap=%llu B peak_memory_used=%llu B logical_gpu_merged=%llu B",
        mergedBudgetSampleCount,
        mergedBudgetRatchetCount,
        static_cast<unsigned long long>(mergedStreamingCapBytes),
        static_cast<unsigned long long>(peakMergedMemoryUsedBytes),
        static_cast<unsigned long long>(DistantLoaders::logicalGpuMergedGeometryBytes())
    );
}

// Enable the render path once both gates are satisfied: the upload pump is
// complete AND the save/new-game world data has resolved. Whichever event
// happens last triggers the transition.
void DistantLand::finalizeUploadIfReady() {
    if (state != InitState::DeviceResourcesReady) {
        return;
    }
    if (!uploadComplete || !worldResolved) {
        return;
    }

    resolveDynamicVisGroups();
    state = InitState::RenderReady;
    isRenderCached = false;
    LOG::logline("<< Completed Distant Land init");
    LOG::logline("-- Distant land render path enabled");
}

// Eviction boundary at the entry of renderStage0, before this frame's shadow/cull queries.
// Both persistent visible vectors are cleared first, so no RenderMesh copy can reference a
// buffer that is about to be released.
void DistantLand::evictResidencyAtStage0() {
    if (!DistantLoaders::residencyHasPendingEviction()) {
        return;
    }
    // The previous frame's parallel-read shadow path can still hold an RPC. Never clear a
    // shared vector the host may be writing; defer the whole batch to the next boundary.
    if (ipcClient.pollRpcCompletion() != IPC::Complete) {
        return;
    }

    visDistantShared.RemoveAll();
    visExtraShared.RemoveAll();
    if (DistantLoaders::tickResidencyEviction(kResidencyEvictBudgetMs, kResidencyEvictBudgetResources)) {
        DistantLoaders::noteResidencyEvictionBoundary(true);
    }
}

namespace {

// Quantizes the horizontal camera view vector into 32 heading bins [0..=31], encoded as wire values 1..=32.
// Bin b covers [b * 2pi/32, (b+1) * 2pi/32). Wire value 0 means no valid hint.
// Paired with Rust decode_heading_bin / heading_vector in mgeHost64/src/state/distant_land.rs.
std::uint32_t quantizeViewHeadingBin(const D3DXVECTOR4& eye) {
    const float horizSq = eye.x * eye.x + eye.y * eye.y;
    constexpr float kHorizEpsilonSq = 1e-4f;
    if (horizSq <= kHorizEpsilonSq) {
        return 0;
    }

    constexpr float kTwoPi = 6.28318530717958647692f;
    float angle = std::atan2(eye.y, eye.x);
    if (angle < 0.0f) {
        angle += kTwoPi;
    }
    if (!(angle >= 0.0f && angle < kTwoPi)) {
        // Rounding at the wrap point, or a non-finite view vector that slipped the gate above.
        return 0;
    }

    const std::uint32_t bin = std::min(static_cast<std::uint32_t>(angle * (32.0f / kTwoPi)), 31u);
    return bin + 1;
}

} // namespace

// End-of-frame residency tick, after rendering has ended. Admission is always safe here; the
// eviction fallback runs only on frames that never reached renderStage0 (menu/load screens).
void DistantLand::tickResidency(bool stage0RanThisFrame) {
    if (!canRenderDistantLand()) {
        return;
    }
    sampleMergedStreamingCap();
    if (!DistantLoaders::residencyActive()) {
        return;
    }
    DistantLoaders::noteResidencyPresent();
    // Every residency RPC below begins with a blocking wait on any outstanding command. Skip
    // the whole tick instead, so a still-running render query can never stall a frame.
    if (ipcClient.pollRpcCompletion() != IPC::Complete) {
        return;
    }

    if (!stage0RanThisFrame && DistantLoaders::residencyHasPendingEviction()) {
        // Eviction runs before admission here so the freed headroom is available to the
        // same tick.
        visDistantShared.RemoveAll();
        visExtraShared.RemoveAll();
        if (DistantLoaders::tickResidencyEviction(kResidencyEvictBudgetMs, kResidencyEvictBudgetResources)) {
            DistantLoaders::noteResidencyEvictionBoundary(false);
        }
    }

    float position[3] = {};
    if (MWBridge::get()->tryGetPlayerPosition(position)) {
        const std::uint32_t viewHeadingBin = stage0RanThisFrame ? quantizeViewHeadingBin(eyeVec) : 0;
        DistantLoaders::planResidency(D3DXVECTOR3(position[0], position[1], position[2]), viewHeadingBin);
    }

    DistantLoaders::tickResidencyAdmission(
        kResidencyAdmitBudgetMs,
        kResidencyAdmitBudgetBytes,
        kResidencyAdmitBudgetResources
    );
}

// Advance the upload pump by one budgeted slice. Driven from Present across idle
// menu / save-selection / save-load frames. Terrain and grass run as single
// slices; the dominant statics phase is resumable across frames.
void DistantLand::pumpUploadTick(int budgetMs) {
    if (!pumpActive) {
        return;
    }

    switch (uploadPhase) {
    case UploadPhase::HostWait:
        if (ipcClient.launchState() == IPC::Client::Inactive) {
            invalidateWorldSpaceCache();
            if (!ipcClient.launchServer("mgeHost64.exe")) {
                failUpload();
                return;
            }
        }
        if (ipcClient.launchState() == IPC::Client::LaunchedPendingBootstrap) {
            const auto wake = ipcClient.pollBootstrapReady();
            if (wake == IPC::Timeout) {
                return;
            }
            if (wake != IPC::Complete) {
                failUpload();
                return;
            }
            ipcClient.markRuntimeActive();
        }
        uploadPhase = UploadPhase::OutputWait;
        break;

    case UploadPhase::OutputWait:
        {
            const int nowMs = HighResolutionTimer::getMilliseconds();
            if (outputWaitStartedMs == 0) {
                outputWaitStartedMs = nowMs;
                outputWaitNextLogMs = 30000;
            } else {
                const int elapsedMs = nowMs - outputWaitStartedMs;
                if (elapsedMs >= outputWaitNextLogMs) {
                    LOG::logline("Still waiting for distant-land generation output (%d seconds elapsed)", elapsedMs / 1000);
                    outputWaitNextLogMs += 30000;
                }
            }
        }
        if (!outputStatusQueryPending) {
            if (!ipcClient.queryOutputStatus()) {
                failUpload();
                return;
            }
            outputStatusQueryPending = true;
            return;
        }
        {
            const auto wake = ipcClient.pollRpcCompletion();
            if (wake == IPC::Timeout) {
                return;
            }
            outputStatusQueryPending = false;
            if (wake != IPC::Complete) {
                failUpload();
                return;
            }
            switch (ipcClient.outputStatus()) {
            case IPC::OutputReady:
                uploadPhase = UploadPhase::IpcSetup;
                break;
            case IPC::OutputFailed:
                failUpload();
                return;
            case IPC::OutputPending:
            default:
                break;
            }
        }
        break;

    case UploadPhase::IpcSetup:
        if (!initIpcVectors()) {
            failUpload();
            return;
        }
        uploadPhase = UploadPhase::Landscape;
        break;

    case UploadPhase::Landscape:
        if (!initLandscape()) {
            failUpload();
            return;
        }
        // InitLandscape stays in flight: the host builds the land quadtree while the statics
        // phase parses and uploads. finishLandscapeUpload() collects the result below.
        uploadPhase = UploadPhase::Statics;
        break;

    case UploadPhase::Statics:
        if (!staticsPhaseStarted) {
            if (!beginStaticsPhase()) {
                failUpload();
                return;
            }
            staticsPhaseStarted = true;
            bool skipResult = false;
            if (DistantLoaders::queryStaticsSkipResult(skipResult)) {
                abortStaticsPhase();
                if (!skipResult || !finishLandscapeUpload()) {
                    failUpload();
                    return;
                }
                uploadPhase = UploadPhase::Grass;
                break;
            }
        }
        {
            bool done = false;
            if (!stepStaticsPhase(budgetMs, done)) {
                failUpload();
                return;
            }
            if (done) {
                // Collect the terrain result before finishStaticsPhase issues the next RPC.
                if (!finishLandscapeUpload()) {
                    abortStaticsPhase();
                    failUpload();
                    return;
                }
                const bool ok = finishStaticsPhase();
                abortStaticsPhase();
                if (!ok) {
                    failUpload();
                    return;
                }
                uploadPhase = UploadPhase::Grass;
            }
        }
        break;

    case UploadPhase::Grass:
        if (!initGrass()) {
            failUpload();
            return;
        }
        uploadPhase = staticsHostVecId == IPC::InvalidVector
            ? UploadPhase::Done
            : UploadPhase::StaticsHostWait;
        break;

    case UploadPhase::StaticsHostWait:
        {
            const auto wake = ipcClient.pollRpcCompletion();
            if (wake == IPC::Timeout) {
                return;
            }
            if (wake != IPC::Complete || !ipcClient.lastInitDistantStaticsSucceeded()) {
                failUpload();
                return;
            }
            if (!ipcClient.freeVecBlocking(staticsHostVecId)
                || !ipcClient.freeVecBlocking(subsetsHostVecId)) {
                failUpload();
                return;
            }
            staticsHostVecId = IPC::InvalidVector;
            subsetsHostVecId = IPC::InvalidVector;
            if (!verifyResidencyProtocol()) {
                failUpload();
                return;
            }
            if (!selectInitialMergedStreamingCap()) {
                failUpload();
                return;
            }
            DistantLoaders::beginResidency();
            residencyBootstrapStartedMs = 0;
            uploadPhase = DistantLoaders::residencyFullDrainActive()
                ? UploadPhase::ResidencyFullDrain
                : UploadPhase::ResidencyBootstrap;
        }
        break;

    case UploadPhase::ResidencyFullDrain:
        {
            bool done = false;
            if (!DistantLoaders::stepResidencyFullDrain(
                    static_cast<double>(budgetMs),
                    kResidencyDrainBudgetBytes,
                    kResidencyDrainBudgetResources,
                    done)) {
                failUpload();
                return;
            }
            if (done) {
                uploadPhase = UploadPhase::Done;
            }
        }
        break;

    case UploadPhase::ResidencyBootstrap:
        // A dataset with no merged resources leaves the planner idle; fitting and fallback
        // datasets took the separate ordered full-drain phase above.
        if (!DistantLoaders::residencyActive()) {
            uploadPhase = UploadPhase::Done;
            break;
        }
        {
            // Existing accessors that return zero on an invalid player pointer are forbidden
            // as planner inputs: (0,0,0) is a valid-looking centre in the Bitter Coast.
            float position[3] = {};
            if (!MWBridge::get()->tryGetPlayerPosition(position)) {
                // Cold-start menu frames have no valid destination. Keep the pump armed so
                // onResolveDuringInit can supply the first safe centre and drain only its ring.
                // If the resolve hook still cannot produce one, keep the no-hang
                // fallback and let gameplay admission recover with temporary pop-in.
                if (worldResolved) {
                    LOG::logline("-- Merged-static residency bootstrap skipped: resolved world has no valid player position");
                    uploadPhase = UploadPhase::Done;
                }
                break;
            }

            if (residencyBootstrapStartedMs == 0) {
                residencyBootstrapStartedMs = HighResolutionTimer::getMilliseconds();
            }

            const D3DXVECTOR3 center(position[0], position[1], position[2]);
            DistantLoaders::planResidency(center, 0);
            DistantLoaders::tickResidencyAdmission(
                static_cast<double>(budgetMs),
                kResidencyDrainBudgetBytes,
                kResidencyDrainBudgetResources
            );
            DistantLoaders::tickResidencyEviction(static_cast<double>(budgetMs), kResidencyDrainBudgetResources);
            if (DistantLoaders::residencyQuiescent()) {
                uploadPhase = UploadPhase::Done;
                break;
            }

            // A centre exists but the nearest ring is still draining. Cap the prefetch so a
            // large or slow dataset cannot hold the load screen open indefinitely.
            const int waitedMs = HighResolutionTimer::getMilliseconds() - residencyBootstrapStartedMs;
            if (waitedMs >= kResidencyBootstrapTimeoutMs) {
                LOG::logline(
                    "-- Merged-static residency bootstrap capped at %dms; entering with pop-in rather than stalling",
                    waitedMs
                );
                uploadPhase = UploadPhase::Done;
            }
        }
        break;

    case UploadPhase::Done:
        pumpActive = false;
        uploadComplete = true;
        isDistantLandLoaded = true;
        LOG::logline("-- Distant land upload pump complete");
        finalizeUploadIfReady();
        break;

    case UploadPhase::None:
        pumpActive = false;
        break;
    }
}

bool DistantLand::initGrass() {
    HRESULT hr;

    hr = device->CreateVertexDeclaration(GrassElem, &GrassDecl);
    if (hr != D3D_OK) {
        LOG::logline("!! Failed to create grass decl");
        return false;
    }

    hr = device->CreateVertexBuffer(MaxGrassElements * GrassInstStride, D3DUSAGE_DYNAMIC|D3DUSAGE_WRITEONLY, 0, D3DPOOL_DEFAULT, &vbGrassInstances, NULL);
    if (hr != D3D_OK) {
        LOG::logline("!! Failed to create grass instance buffer");
        return false;
    }

    return true;
}

void DistantLand::release() {
    // Abort any in-flight upload pump first: free partial per-subset resources,
    // the mapped file, and the IPC stream handle before the main teardown.
    abortUploadPump();

    invalidateWorldSpaceCache();
    ipcClient.stopServer();

    if (!hasDeviceResources()) {
        return;
    }

    // Free only resources that exist: release() must be safe from any partial
    // init state (e.g. a device-resources-only state, or a mid-init failure).
    auto safeRelease = [](auto*& p) { if (p) { p->Release(); p = nullptr; } };

    LOG::logline("-- Renderer unloading");
    LOG::logline(
        "-- Depth capture stats: native stage1=%llu, native stage2=%llu, legacy stage1=%llu, legacy stage2=%llu, replay DIPs=%llu",
        nativeStage1Captures,
        nativeStage2Captures,
        stage1LegacyFallbacks,
        stage2LegacyFallbacks,
        depthReplayDips
    );
    logMergedStreamingBudgetSummary();

    recordMW.clear();
    recordSky.clear();
    recordedSkinPalettes.clear();
    observedPostLights.clear();
    postPointLightsRequested = false;
    postLightCount = 0;

    FixedFunctionShader::release();
    PostShaders::release();

    dynamicVisGroups.clear();
    lastDistantVisCell = nullptr;
    isDistantLandLoaded = false;
    staticsUploaded = false;

    DistantLoaders::releaseStaticsResources();

    DistantLoaders::releaseTerrainResources();

    BSA::clearTextureCache();

    if (Configuration.MGEFlags & DYNAMIC_RIPPLES) {
        safeRelease(surfRain);
        safeRelease(texRain);
        safeRelease(surfRipples);
        safeRelease(texRipples);
        safeRelease(surfRippleBuffer);
        safeRelease(texRippleBuffer);
        safeRelease(vbWaveSim);
    }

    safeRelease(TerrainDecl);
    safeRelease(StaticDecl);
    safeRelease(WaterDecl);
    safeRelease(GrassDecl);

    safeRelease(texShadow);
    safeRelease(texSoftShadow);
    safeRelease(surfShadowZ);

    safeRelease(texWater);
    safeRelease(texReflection);
    safeRelease(surfReflectionZ);
    safeRelease(vbWater);
    safeRelease(ibWater);
    safeRelease(vbGrassInstances);
    safeRelease(vbFullFrame);

    ProxyDevice::setDepthStencilSubstitute(nullptr, nullptr);
    if (nativeDepthBackend == NativeDepthBackend::IntzMainDsv && surfAutoDepthStencil) {
        device->SetDepthStencilSurface(surfAutoDepthStencil);
    }
    nativeDepthBackend = NativeDepthBackend::None;
    safeRelease(dxvkMorrowindInterop);
    safeRelease(dxvkMorrowindMemoryInterop);
    safeRelease(surfAutoDepthStencil);
    safeRelease(surfDepthStencil);
    safeRelease(texDepthStencil);
    stage1UsedNativeDepth = false;
    nativeStage2Eligible = false;
    dsvMayBeNoncanonical = true;

    safeRelease(texDepthFrame);
    safeRelease(surfDepthDepth);

    safeRelease(effectPool);
    safeRelease(effectShadow);
    safeRelease(effectDepth);
    safeRelease(effect);

    LOG::logline("-- Renderer unloaded");
    LOG::flush();

    fogNearEnd = 0;
    nativeStage1Captures = 0;
    nativeStage2Captures = 0;
    stage1LegacyFallbacks = 0;
    stage2LegacyFallbacks = 0;
    depthReplayDips = 0;
    indexedSkinningShaderCheckComplete = false;
    indexedSkinningShadersCompatible = false;
    device = nullptr;
    state = InitState::Uninitialized;
}
