#pragma once

#include "proxydx/d3d8header.h"
#include "mgeindexedskinning.h"
#include "dxvk_morrowind_interop.h"

#include <unordered_map>
#include <vector>
#include <mutex>
#include <thread>
#include <string>

// The legacy Shader Model 3 effect cannot carry more lights than this; its
// varying layout is already near the interpolator limit. It does not follow
// the native packet maximum.
static constexpr uint32_t MGE_LEGACY_PPL_MAX_LIGHTS = 8;



struct RenderedState {
    IDirect3DTexture9* texture;
    IDirect3DVertexBuffer9* vb;
    UINT vbOffset, vbStride;
    IDirect3DIndexBuffer9* ib;
    DWORD ibBase;
    DWORD fvf;
    DWORD zWrite, cullMode;
    DWORD vertexBlendState;
    D3DXMATRIX worldTransforms[4];
    D3DXMATRIX viewTransform;
    D3DXMATRIX worldViewTransforms[4];
    D3DCOLORVALUE diffuseMaterial;
    BYTE blendEnable, srcBlend, destBlend;
    BYTE alphaTest, alphaFunc, alphaRef;
    BYTE useLighting, useFog, matSrcDiffuse, matSrcEmissive;

    D3DPRIMITIVETYPE primType;
    UINT baseIndex, minIndex, vertCount, startIndex, primCount;
};

struct FragmentState {
    struct Stage {
        BYTE colorOp, colorArg1, colorArg2;
        BYTE alphaOp, alphaArg1, alphaArg2;
        BYTE colorArg0, alphaArg0, resultArg;
        DWORD texcoordIndex;
        DWORD texTransformFlags;
        float bumpEnvMat[2][2];
        float bumpLumiScale, bumpLumiBias;
        // Shadowed pipeline state, avoiding per-draw readback from the device
        IDirect3DTexture9* texture;
        D3DXMATRIX textureTransform;
    } stage[8];

    struct Material {
        D3DCOLORVALUE diffuse, ambient, emissive;
    } material;
};

struct LightState {
    struct Light {
        D3DLIGHTTYPE type;
        D3DCOLORVALUE diffuse;
        D3DVECTOR position;     // position / normalized direction
        D3DVECTOR viewspacePos;
        union {
            D3DVECTOR falloff;  // constant, linear, quadratic
            D3DVECTOR ambient;  // for directional lights
        };
    };

    D3DCOLORVALUE globalAmbient;
    // Shadowed D3DRS_AMBIENT == 0xffffffff, avoiding per-draw readback from the device
    bool ambientWhite;
    std::unordered_map<DWORD, Light> lights;
    std::unordered_map<DWORD, bool> lightsTransformed;
    std::vector<DWORD> active;
};

struct DecodedPointLight {
    D3DCOLORVALUE diffuse;
    D3DVECTOR attenuation;  // constant, linear, quadratic
    float ambient;
    float viewspaceZBias;
};

DecodedPointLight decodeMorrowindPointLight(
    const D3DCOLORVALUE& diffuse,
    const D3DVECTOR& falloff,
    float& sharedFalloffConstant);

class FixedFunctionShader {
    struct ShaderKey {
        DWORD uvSets : 4;
        DWORD usesSkinning : 1;
        DWORD indexedSkinning : 1;
        DWORD vertexColour : 1;
        DWORD heavyLighting : 1;
        DWORD vertexMaterial : 2;
        DWORD fogMode : 2;
        DWORD activeStages : 3;
        DWORD usesBumpmap : 1;
        DWORD bumpmapStage : 3;
        DWORD usesTexgen : 1;
        DWORD projectiveTexgen : 1;
        DWORD texgenStage : 3;

        struct Stage {
            DWORD colorOp : 6;
            DWORD colorArg1 : 6;
            DWORD colorArg2 : 6;
            DWORD colorArg0 : 6;
            DWORD alphaOpMatched : 1;
            DWORD alphaOpSelect1 : 1;
            DWORD texcoordIndex : 2;
            DWORD texcoordGen : 4;
        } stage[8];

        ShaderKey();
        ShaderKey(const RenderedState* rs, const FragmentState* frs, const LightState* lightrs);
        bool operator<(const ShaderKey& other) const;
        bool operator==(const ShaderKey& other) const;
        void log() const;

        struct hasher {
            std::size_t operator()(const ShaderKey& k) const;
        };
    };

    struct PplDrawData {
        ShaderKey key;

        D3DXVECTOR4 materialDiffuse;
        D3DXVECTOR4 materialAmbient;
        D3DXVECTOR4 materialEmissive;

        RGBVECTOR sceneAmbient;
        RGBVECTOR sunDiffuse;
        D3DXVECTOR3 sunDirection;
        bool hasSunDirection;

        // Exact number of densely packed point-light slots written. The pack
        // width chosen by renderMorrowind is also the row stride of the linear
        // lightPosition array, which stays linear for the legacy upload.
        uint32_t pointLightCount;

        D3DXVECTOR4 lightDiffuse[DXVK_MORROWIND_PPL_MAX_LIGHTS];
        float lightAmbient[DXVK_MORROWIND_PPL_MAX_LIGHTS];
        float lightPosition[3 * DXVK_MORROWIND_PPL_MAX_LIGHTS];
        float lightFalloffQuadratic[DXVK_MORROWIND_PPL_MAX_LIGHTS];
        float lightFalloffLinear[DXVK_MORROWIND_PPL_MAX_LIGHTS];
        float lightFalloffConstant;

        float bumpMatrix[4];
        float bumpLumiScaleBias[2];
        D3DXMATRIX texgenTransform;
    };

    struct PplSceneState {
        D3DXMATRIX projection;
        float nearFogStart;
        float nearFogRange;
        RGBVECTOR fogColor;
        bool initialized;
    };

    struct ShaderLRU {
        ID3DXEffect* effect;
        FixedFunctionShader::ShaderKey last_sk;
    };

    static IDirect3DDevice* device;
    static ID3DXEffectPool* constantPool;
    static std::unordered_map<ShaderKey, ID3DXEffect*, ShaderKey::hasher> cacheEffects;
    static ShaderLRU shaderLRU;
    static ID3DXEffect* effectDefaultPurple;
    static bool indexedSkinningShadersCompatible;
    static D3DXMATRIX skinWorldTransforms[MGE_INDEXED_SKINNING_PALETTE_SIZE];
    static D3DXMATRIX skinWorldViewTransforms[MGE_INDEXED_SKINNING_PALETTE_SIZE];

    struct ShaderSource {
        std::string genVBCoupling, genPSCoupling, genTransform, genTexcoords,
                    genVertexColour, genLightCount, genMaterial, genTexturing, genFog;
        std::vector<D3DXMACRO> macros;
    };

    static std::thread precacheThread;
    static std::mutex precacheMutex;
    static std::mutex compileMutex;
    static std::unordered_map<ShaderKey, ID3DXBuffer*, ShaderKey::hasher> precompiled;

    static bool buildShaderSource(const ShaderKey& sk, ShaderSource& src);
    static ID3DXBuffer* compileMWShader(const ShaderKey& sk);
    static void waitForPrecacheThread();

    static D3DXHANDLE ehWorld, ehWorldView;
    static D3DXHANDLE ehVertexBlendState, ehVertexBlendPalette;
    static D3DXHANDLE ehTex0, ehTex1, ehTex2, ehTex3, ehTex4, ehTex5;
    static D3DXHANDLE ehMaterialDiffuse, ehMaterialAmbient, ehMaterialEmissive;
    static D3DXHANDLE ehLightSceneAmbient, ehLightSunDiffuse, ehLightSunDirection;
    static D3DXHANDLE ehLightDiffuse, ehLightAmbient, ehLightPosition;
    static D3DXHANDLE ehLightFalloffQuadratic, ehLightFalloffLinear, ehLightFalloffConstant;
    static D3DXHANDLE ehTexgenTransform, ehBumpMatrix, ehBumpLumiScaleBias;

    static float sunMultiplier, ambMultiplier;

    static IDxvkMorrowindPplInterop1* m_morrowindInterop;
    static PplSceneState m_pplSceneState;
    static unsigned long long m_nativePplDraws;
    static unsigned long long m_nativePplUnavailable;
    static unsigned long long m_nativePplUnsupported;
    static unsigned long long m_nativePplFailures;
    static bool m_loggedNativePplUnsupported;
    static bool m_loggedNativePplFailure;

    static void buildPplDrawData(
        const RenderedState* rs,
        const FragmentState* frs,
        LightState* lightrs,
        const ShaderKey& key,
        uint32_t packWidth,
        PplDrawData* out);
    static void renderMorrowindLegacy(
        const RenderedState* rs,
        const FragmentState* frs,
        const PplDrawData& data);
    static bool renderMorrowindNative(
        const RenderedState* rs,
        const PplDrawData& data,
        DxvkMorrowindPplDrawV1& packet);
    static bool encodeNativePplKey(
        const ShaderKey& source,
        DxvkMorrowindPplDrawV1* destination);

    static ID3DXEffect* generateMWShader(const ShaderKey& sk);

public:
    static bool supportsIndexedSkinningShaders() { return indexedSkinningShadersCompatible; }
    static void resetSkinningTransforms();
    static void setSkinningWorldTransform(UINT index, const D3DXMATRIX* world, const D3DXMATRIX* view);
    static void setSkinningViewTransform(const D3DXMATRIX* view);
    static const D3DXMATRIX* getSkinningWorldViewTransforms();

    static bool init(IDirect3DDevice* d, ID3DXEffectPool* pool);
    static void precacheAsync();
    static void updateLighting(float sunMult, float ambMult);
    static void updatePplSceneState(
        const D3DXMATRIX* projection,
        float nearFogStart,
        float nearFogRange,
        const RGBVECTOR& fogColor);

    static void renderMorrowind(const RenderedState* rs, const FragmentState* frs, LightState* lightrs);
    static void release();
};
