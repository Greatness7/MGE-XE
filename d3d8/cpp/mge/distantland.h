#pragma once

#include "visibleset.h"
#include "ffeshader.h"
#include "mwbridge.h"
#include "specificrender.h"
#include "ipc/client.h"
#include "ipc/vecwrap.h"
#include "ipc/dlshare.h"

#include <string>
#include <vector>
#include <unordered_map>
#include <functional>
#include <memory>



struct MGEShader;
struct IDxvkMorrowindInterop;
struct IDxvkMorrowindMemoryInterop1;

class DistantLand {
public:
    struct DynamicVisGroup {
        enum class DataSource : uint8_t {
            Journal = 1,
            Global = 2,
            UniqueObject = 3
        };
        struct Range {
            int begin, end;
        };

        DataSource source;
        bool enabled;
        const void *gameObject;
        std::string id;
        std::vector<Range> ranges;

    };

    struct RecordedState : RenderedState {
        std::uint32_t skinPaletteOffset;
        std::uint32_t skinPaletteCount;

        RecordedState(const RenderedState&);
        ~RecordedState();
        RecordedState(const RecordedState&) = delete;
        RecordedState(RecordedState&&) noexcept;
    };

    struct ObservedPointLight {
        DWORD id;
        D3DVECTOR position;
        D3DCOLORVALUE diffuse;
        D3DVECTOR falloff;
    };

    static constexpr DWORD fvfWave = D3DFVF_XYZRHW | D3DFVF_TEX2;
    static constexpr int waveTexResolution = 512;
    static constexpr float waveTexWorldRes = 2.5f;
    static constexpr int GrassInstStride = 48;
    static constexpr int MaxGrassElements = 16384;
    static constexpr float kCellSize = 8192.0f;
    static constexpr float kDistantZBias = 5e-6f;
    static constexpr float kDistantNearPlane = 4.0f;
    static constexpr float kMoonTag = 88888.0f;
    static constexpr int kMaxPostPointLights = 32;
    static constexpr D3DFORMAT kFormatIntz =
        static_cast<D3DFORMAT>(MAKEFOURCC('I', 'N', 'T', 'Z'));
    // Render target that allocates no memory, for depth-only passes
    static constexpr D3DFORMAT kFormatNull =
        static_cast<D3DFORMAT>(MAKEFOURCC('N', 'U', 'L', 'L'));

    struct TerrainRuntimeConstants {
        D3DXVECTOR2 worldOrigin;
        D3DXVECTOR2 invAtlasSize;
        D3DXVECTOR2 invMaterialSize;
        float logicalTileSize;
        float gutterSize;
        float physicalTileSize;
        float tilesPerRow;
        float atlasMaxLod;
        float patternCount;
        float patternTileSize;
        float patternGutterSize;
        float patternPhysicalSize;
        float patternsPerRow;
    };

    // Distant-land initialization lifecycle, replacing the old single `ready` bool.
    enum class InitState {
        Uninitialized,         // no resources created
        DeviceResourcesReady,  // owns D3D resources (shaders/RTs/decls); geometry not yet uploaded
        RenderReady,           // geometry uploaded; fully renderable (old `ready == true`)
        FailedDisabled,        // init failed; partial resources have been torn down
    };
    static InitState state;
    static bool indexedSkinningShaderCheckComplete;
    static bool indexedSkinningShadersCompatible;

    // True once D3D resources exist and must be released (covers partial init).
    static bool hasDeviceResources() { return state == InitState::DeviceResourcesReady || state == InitState::RenderReady; }
    // True once distant land is fully initialized and the render path may run (old `ready`).
    static bool canRenderDistantLand() { return state == InitState::RenderReady; }
    static bool hasCheckedIndexedSkinningShaders() { return indexedSkinningShaderCheckComplete; }
    static bool supportsIndexedSkinningShaders() { return indexedSkinningShadersCompatible; }

    // Frame-budgeted upload pump. The heavy terrain/statics/grass upload is armed at createScene and
    // ticked across idle menu/load frames from Present, instead of blocking in one synchronous call.
    enum class UploadPhase : uint8_t {
        None,             // pump idle / not armed
        HostWait,         // 64-bit host launched, waiting for its RPC loop to come up
        OutputWait,       // polling the host until distant-land generation/output resolves
        IpcSetup,         // allocate the long-lived shared vectors
        Landscape,        // single-slice terrain upload; leaves InitLandscape in flight
        Statics,          // resumable distant-statics upload (the dominant phase)
        Grass,            // single-slice grass setup, overlapping the host's statics build
        StaticsHostWait,  // collect InitDistantStatics and free its staging vectors
        ResidencyFullDrain,  // fitting/fallback session: ordered merged upload before rendering
        ResidencyBootstrap,  // capped session only: drain the nearest merged ring before rendering
        Done,             // all phases uploaded
    };
    static constexpr int kUploadPumpBudgetMs = 8;
    static constexpr int kDrainBudgetMs = 40;

    // Provisional residency budgets. The byte and record limits are mandatory because a
    // wall-clock check cannot interrupt one D3D allocation; an oversize resource is processed
    // alone and logged. Revisit from measured p99/max crossings, not average inflow.
    static constexpr double kResidencyAdmitBudgetMs = 2.0;
    static constexpr std::uint64_t kResidencyAdmitBudgetBytes = 2ull * 1024 * 1024;
    static constexpr std::uint32_t kResidencyAdmitBudgetResources = 16;
    static constexpr double kResidencyEvictBudgetMs = 1.0;
    static constexpr std::uint32_t kResidencyEvictBudgetResources = 2;
    static constexpr std::uint64_t kResidencyDrainBudgetBytes = 10ull * 1024 * 1024;
    static constexpr std::uint32_t kResidencyDrainBudgetResources = 14;
    // Once a valid bootstrap centre exists, bound the synchronous nearest-ring drain rather
    // than holding the load screen indefinitely. Waiting in menu frames does not start it.
    static constexpr int kResidencyBootstrapTimeoutMs = 5000;
    static constexpr int kMergedBudgetSampleIntervalMs = 500;
    static constexpr std::uint32_t kMergedBudgetRatchetSamples = 4;
    static constexpr std::uint64_t kMergedBudgetMinHeadroomBytes = 256ull * 1024 * 1024;
    static constexpr std::uint64_t kMergedBudgetMaxHeadroomBytes = 1024ull * 1024 * 1024;
    static UploadPhase uploadPhase;
    static bool pumpActive;        // pump armed and ticking from Present
    static bool pumpDraining;      // pump is being drained synchronously during load
    static bool worldResolved;     // save/new-game world data has resolved
    static bool uploadComplete;    // all upload phases finished
    // Active merged-static cap. DXVK supplies an allocator-policy-adjusted budget
    // after fixed resources have been created.
    static bool automaticStreamingCapActive;
    static std::uint64_t mergedStreamingCapBytes;
    static int nextMergedBudgetSampleMs;
    static std::uint32_t lowerMergedBudgetSampleCount;
    static std::uint32_t mergedBudgetSampleCount;
    static std::uint32_t mergedBudgetRatchetCount;
    static std::uint64_t pendingMergedCandidateCapBytes;
    static std::uint64_t pendingMergedPeakMemoryUsedBytes;
    static std::uint64_t peakMergedMemoryUsedBytes;
    static bool staticsPhaseStarted;  // beginStaticsPhase() has run for the current pump
    static int residencyBootstrapStartedMs;
    static bool outputStatusQueryPending;
    static int outputWaitStartedMs;
    static int outputWaitNextLogMs;
    static IPC::VecId landscapeHostVecId;
    static IPC::VecId staticsHostVecId;
    static IPC::VecId subsetsHostVecId;

    // Resumable distant-statics loader state, hoisted out of the load loop so it
    // can yield and resume across frames. Defined in distantstatics.cpp.
    struct StaticsLoader;
    static std::unique_ptr<StaticsLoader> staticsLoader;

    static bool isRenderCached;
    static bool isPPLActive;
    static int numWaterVerts, numWaterTris;

    static IDirect3DDevice9* device;
    static ID3DXEffect* effect;
    static ID3DXEffect* effectShadow;
    static ID3DXEffect* effectDepth;
    static ID3DXEffectPool* effectPool;
    static IDirect3DVertexDeclaration9* TerrainDecl;
    static IDirect3DVertexDeclaration9* StaticDecl;
    static IDirect3DVertexDeclaration9* WaterDecl;
    static IDirect3DVertexDeclaration9* GrassDecl;

    static VendorSpecificRendering vsr;

    static IPC::Client ipcClient;
    static std::vector<DynamicVisGroup> dynamicVisGroups;
    static void* lastDistantVisCell;
    // Cached SetWorldSpace result; invalidated on every host (re)start.
    static std::string lastWorldSpaceKey;
    static bool lastWorldSpaceFound;
    static bool worldSpaceCacheValid;
    static bool isDistantLandLoaded;
    static bool staticsUploaded;

    static VisibleSet visLandShared;
    static VisibleSet visDistantShared;
    static VisibleSet visGrassShared;
    static VisibleSet visExtraShared;
    static IPC::VecView<IPC::DynVisFlag> dynVisFlagsShared;
    static IPC::VecView<IPC::ResidencyPlan> residencyPlanShared;
    static IPC::VecView<IPC::ResidencyCommit> residencyCommitShared;

    static IPC::VecId visLandSharedId;
    static IPC::VecId visDistantSharedId;
    static IPC::VecId visGrassSharedId;
    static IPC::VecId visExtraSharedId;
    static IPC::VecId dynVisFlagsSharedId;
    static IPC::VecId residencyPlanSharedId;
    static IPC::VecId residencyCommitSharedId;

    static std::vector<RecordedState> recordMW;
    static std::vector<RecordedState> recordSky;
    static std::vector<D3DXMATRIX> recordedSkinPalettes;
    static std::vector< std::pair<const RenderMesh*, int> > batchedGrass;
    static bool postPointLightsRequested;
    static std::vector<ObservedPointLight> observedPostLights;
    static D3DXVECTOR4 postLightPositions[kMaxPostPointLights];
    static D3DXVECTOR4 postLightColours[kMaxPostPointLights];
    static int postLightCount;

    static IDirect3DTexture9* texTerrainAtlas, *texTerrainMaterial, *texTerrainMaterialFlags, *texTerrainPatchAlbedo, *texTerrainBlendPatterns;
    static IDirect3DTexture9* texDepthFrame;
    static IDirect3DSurface9* surfDepthDepth;
    static IDirect3DTexture9* texDepthStencil;
    static IDirect3DSurface9* surfDepthStencil;
    static IDirect3DSurface9* surfAutoDepthStencil;
    static IDirect3DTexture9* texDistantBlend;
    static IDirect3DTexture9* texReflection;
    static IDirect3DSurface9* surfReflectionZ;
    static IDirect3DVolumeTexture9* texWater;
    static IDirect3DVertexBuffer9* vbWater;
    static IDirect3DIndexBuffer9* ibWater;
    static IDirect3DVertexBuffer9* vbGrassInstances;

    static IDirect3DTexture9* texRain, *texRipples, *texRippleBuffer;
    static IDirect3DSurface9* surfRain, *surfRipples, *surfRippleBuffer;
    static IDirect3DVertexBuffer9* vbWaveSim;

    // Two atlases: the one receivers sample, and the next, built a cascade per frame and
    // cross-faded in
    static IDirect3DTexture9* texShadow[2];
    static IDirect3DSurface9* surfShadow[2], *surfShadowColor;
    static int shadowCurrent, shadowBuilding, shadowBuildLayer;
    static bool shadowCurrentValid, shadowBuildComplete;
    static DWORD shadowBlendStart;
    static float shadowBlend;
    static IDirect3DVertexBuffer9* vbFullFrame;

    // Must match shadowCascades in "XE Mod Shadow Data.fx". That file is user-replaceable
    // through shaders/core-mods, so a stale copy silently disagrees with this value.
    static constexpr int kShadowCascades = 4;

    static D3DXMATRIX mwView, mwProj;
    static D3DXMATRIX smView[kShadowCascades], smProj[kShadowCascades], smViewproj[2][kShadowCascades];
    static D3DXVECTOR4 eyeVec, eyePos, sunVec, sunPos;
    static float sunVis;
    static RGBVECTOR sunCol, sunAmb, ambCol;
    static RGBVECTOR nearFogCol, horizonCol;
    static RGBVECTOR atmOutscatter, atmInscatter;
    static D3DXVECTOR4 atmSkylightScatter;
    static float fogStart, fogEnd;
    static float fogExpStart, fogExpDivisor;
    static float fogNearStart, fogNearEnd;
    static float nearViewRange;
    static float windScaling, niceWeather;
    static float lightSunMult, lightAmbMult;
    static TerrainRuntimeConstants terrainConstants;

    static D3DXHANDLE ehRcpRes, ehShadowRcpRes;
    static D3DXHANDLE ehWorld, ehView, ehProj;
    static D3DXHANDLE ehShadowViewproj, ehShadowCascade, ehShadowDistant, ehShadowBlend, ehTexShadowNext;
    static D3DXHANDLE ehVertexBlendState, ehVertexBlendPalette;
    static D3DXHANDLE ehAlphaRef, ehMaterialAlpha;
    static D3DXHANDLE ehHasAlpha, ehHasBones, ehHasVCol;
    static D3DXHANDLE ehUvBoundPalette;
    static D3DXHANDLE ehTex0, ehTex1, ehTex2, ehTex3, ehTex4, ehTex5;
    static D3DXHANDLE ehDepthSrc, ehSourceM33, ehSourceM43;
    static D3DXHANDLE ehEyePos, ehFootPos;
    static D3DXHANDLE ehSunCol, ehSunAmb, ehSunVec, ehSunVecView;
    static D3DXHANDLE ehSkyCol, ehFogColNear, ehFogColFar;
    static D3DXHANDLE ehSunPos, ehSunVis;
    static D3DXHANDLE ehOutscatter, ehInscatter, ehSkyScatterFar;
    static D3DXHANDLE ehFogStart, ehFogRange;
    static D3DXHANDLE ehFogNearStart, ehFogNearRange;
    static D3DXHANDLE ehNearViewRange;
    static D3DXHANDLE ehWindVec;
    static D3DXHANDLE ehNiceWeather;
    static D3DXHANDLE ehTime;
    static D3DXHANDLE ehTerrainAtlasTex, ehTerrainMaterialTex, ehTerrainMaterialFlagsTex, ehTerrainPatchAlbedoTex, ehTerrainBlendPatternsTex;
    static D3DXHANDLE ehTerrainWorldOrigin, ehTerrainInvAtlasSize, ehTerrainInvMaterialSize;
    static D3DXHANDLE ehTerrainLogicalTileSize, ehTerrainGutterSize, ehTerrainPhysicalTileSize, ehTerrainTilesPerRow;
    static D3DXHANDLE ehTerrainAtlasMaxLod;
    static D3DXHANDLE ehTerrainPatternCount, ehTerrainPatternTileSize, ehTerrainPatternGutterSize, ehTerrainPatternPhysicalSize, ehTerrainPatternsPerRow;
    static D3DXHANDLE ehRippleOrigin;
    static D3DXHANDLE ehWaveHeight;

    static std::function<void(IDirect3DSurface9*)> captureScreenHandler;
    static bool captureScreenWithUI;

    enum class NativeDepthMode {
        Replace,
        MergeNearest
    };
    enum class NativeDepthBackend {
        None,
        IntzMainDsv,
        DxvkMsaaResolve
    };
    static NativeDepthBackend nativeDepthBackend;
    static IDxvkMorrowindInterop* dxvkMorrowindInterop;
    static IDxvkMorrowindMemoryInterop1* dxvkMorrowindMemoryInterop;
    static bool stage1UsedNativeDepth;
    static bool nativeStage2Eligible;
    static bool dsvMayBeNoncanonical;
    static unsigned long long nativeStage1Captures;
    static unsigned long long nativeStage2Captures;
    static unsigned long long stage1LegacyFallbacks;
    static unsigned long long stage2LegacyFallbacks;
    static unsigned long long depthReplayDips;

    static bool init();
    static bool initDeviceResources();
    static bool initIpc();
    static bool initIpcVectors();
    static bool verifyResidencyProtocol();
    static bool finishLandscapeUpload();
    static bool initShader();
    static bool initDepth();
    static bool initWater();
    static bool initDynamicWaves();
    static bool initLandscapeClient();
    static bool initLandscape();
    static bool initShadow();
    static bool initGrass();
    static bool loadVisGroupsClient(HANDLE h);
    static bool reloadShaders();
    static void release();

    // Upload pump (Blocker 4)
    static void armUploadPump();
    static void pumpUploadTick(int budgetMs);
    static void drainUploadPump();
    static float drainProgressPct();
    static void abortUploadPump();
    static void failUpload();
    static void finalizeUploadIfReady();
    static bool selectInitialMergedStreamingCap();
    static void sampleMergedStreamingCap();
    static void logMergedStreamingBudgetSummary();
    // End-of-frame residency tick. `stage0RanThisFrame` selects the eviction boundary:
    // renderStage0 already evicted, so this only admits; otherwise both run here.
    static void tickResidency(bool stage0RanThisFrame);
    // Eviction boundary at the entry of renderStage0, before this frame's cull queries.
    static void evictResidencyAtStage0();
    static bool beginStaticsPhase();
    static bool stepStaticsPhase(int budgetMs, bool& phaseDone);
    static bool finishStaticsPhase();
    static void abortStaticsPhase();

    static void editProjectionZ(D3DMATRIX* m, float zn, float zf);
    static bool selectDistantCell();
    static void invalidateWorldSpaceCache();
    static bool isDistantCell();
    static void onResolveDuringInit();
    static void resolveDynamicVisGroups();
    static void scanDynamicVisGroups();

    static void setView(const D3DMATRIX* m);
    static void setProjection(D3DMATRIX* proj);
    static void beginDepthFrame();
    static void depthBufferCleared();
    static void trackDepthProjection(const D3DMATRIX* proj);
    static void setHorizonColour(const RGBVECTOR& c);
    static void setAmbientColour(const RGBVECTOR& c);
    static void setSunLight(const D3DLIGHT8* s);
    static void setScattering(const RGBVECTOR& out, const RGBVECTOR& in);
    static void adjustFog();
    static void observePostLights(const RenderedState& rs, const LightState& lightrs);
    static void finalizePostLights();
    static bool inspectIndexedPrimitive(int sceneCount, const RenderedState* rs, const FragmentState* frs, LightState* lightrs);

    static void renderSky();
    static void renderStage0();
    static void renderStage1();
    static void renderStage2();
    static void renderStageBlend();
    static void renderStageWater();

    static void setupCommonEffect(const D3DXMATRIX* view,const  D3DXMATRIX* proj);

    static void renderDistantLand(ID3DXEffect* e, const D3DXMATRIX* view, const D3DXMATRIX* proj);
    static void renderDistantLandZ();
    static void cullDistantStatics(const D3DXMATRIX* view, const D3DXMATRIX* proj);
    static void renderDistantStatics();
    static void cullGrass(const D3DXMATRIX* view, const D3DXMATRIX* proj);
    static void buildGrassInstanceVB(VisibleSet& grassSet);
    static bool hasVisibleGrass();
    static void renderGrassInst();
    static void renderGrassInstZ();
    static void renderGrassCommon(ID3DXEffect* e);

    static void renderWaterReflection(const D3DXMATRIX* view, const D3DXMATRIX* proj);
    static void renderReflectedSky();
    static void renderReflectedStatics(const D3DXMATRIX* view, const D3DXMATRIX* proj);
    static void clearReflection();
    static void simulateDynamicWaves();
    static void renderWaterPlane();

    static void renderDepth();
    static void renderDepthAdditional();
    static void renderDepthRecorded();
    static HRESULT captureNativeDepthIntz(NativeDepthMode mode, const D3DXMATRIX& projection);
    static HRESULT resolveNativeDepthMsaa();
    static HRESULT captureNativeDepth(NativeDepthMode mode, const D3DXMATRIX& projection);
    static IDirect3DSurface9* nativeDepthSourceSurface();
    static bool projectionIsCanonical(const D3DXMATRIX& projection);

    static void renderShadowMap();
    static void renderShadowLayerGeneric(MWBridge* mwBridge, int layer, D3DXMATRIX* view, D3DXMATRIX* proj, VisibleSet& visible_set);
    static void renderShadowLayer(int layer, float radius);
    static void uploadShadowMatrices(const D3DXMATRIX* pre);
    static void renderShadow();
    static void renderShadowDebug();

    static void postProcess();
    static void updatePostShader(MGEShader* shader);

    static void requestCapture(std::function<void(IDirect3DSurface9*)> handler, bool captureWithUI);
    static void checkCaptureScreenshot(bool isUIDrawn);
    static IDirect3DSurface9* captureScreenshot();
};

static_assert(sizeof(DistantLand::TerrainRuntimeConstants) == 64, "Terrain runtime constants ABI drifted");

class RenderTargetSwitcher {
    IDirect3DSurface9* savedTarget, *savedDepthStencil;
    void init(IDirect3DSurface9* target, IDirect3DSurface9* targetDepthStencil);

public:
    RenderTargetSwitcher(IDirect3DSurface9* target, IDirect3DSurface9* targetDepthStencil);
    RenderTargetSwitcher(IDirect3DTexture9* targetTex, IDirect3DSurface9* targetDepthStencil);
    ~RenderTargetSwitcher();
};
