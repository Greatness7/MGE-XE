#pragma once

// Private Morrowind/DXVK COM ABI.
//
// Prerequisites are deliberately limited to platform headers so this file can
// be kept byte-for-byte equivalent in the independent MGE-XE and DXVK trees.
#include <stdint.h>
#include <type_traits>
#include <unknwn.h>
#include <d3d9.h>

#include "dxvk_morrowind_limits.h"

static constexpr uint32_t DXVK_MORROWIND_INTEROP_VERSION = 1;

static constexpr uint64_t DXVK_MORROWIND_CAP_MSAA_DEPTH_RESOLVE = 1ull << 0;
static constexpr uint64_t DXVK_MORROWIND_CAP_PPL_DRAW_V1 = 1ull << 1;
static constexpr uint64_t DXVK_MORROWIND_CAP_PPL_DRAW_V2 = 1ull << 2;
// Every path this renderer can reach consumes the expanded light limit, not
// just the native packet. The client's engine patch is irreversible while the
// lighting mode stays runtime-mutable, so authorization has to be stated
// outright rather than inferred from the packet version: a build could speak
// V2 packets while its ordinary fixed-function path still stopped at 8.
static constexpr uint64_t DXVK_MORROWIND_CAP_EXPANDED_LIGHT_LIMIT = 1ull << 3;

static constexpr uint32_t DXVK_MORROWIND_PPL_STRUCT_VERSION = 2;
static constexpr uint32_t DXVK_MORROWIND_PPL_MAX_STAGES = 6;

enum DxvkMorrowindPplFlags : uint32_t {
    DXVK_MW_PPL_USE_SKINNING       = 1u << 0,
    DXVK_MW_PPL_VERTEX_COLOR       = 1u << 1,
    DXVK_MW_PPL_USE_BUMPMAP        = 1u << 2,
    DXVK_MW_PPL_USE_TEXGEN         = 1u << 3,
    DXVK_MW_PPL_PROJECTIVE_TEXGEN  = 1u << 4,
};

enum DxvkMorrowindPplStageFlags : uint32_t {
    DXVK_MW_STAGE_ALPHA_MATCHES_COLOR = 1u << 0,
    DXVK_MW_STAGE_ALPHA_SELECT_ARG1   = 1u << 1,
};

struct DxvkMorrowindPplStageV1 {
    uint32_t colorOp;
    uint32_t colorArg1;
    uint32_t colorArg2;
    uint32_t colorArg0;
    uint32_t texcoordIndex;
    uint32_t texcoordGen;
    uint32_t flags;
    uint32_t reserved;
};

struct DxvkMorrowindPplDrawV1 {
    uint32_t structSize;
    uint32_t structVersion;
    uint32_t flags;
    uint32_t reserved0;

    uint32_t primitiveType;
    int32_t  baseVertexIndex;
    uint32_t minVertexIndex;
    uint32_t vertexCount;
    uint32_t startIndex;
    uint32_t primitiveCount;

    uint32_t uvSetCount;
    uint32_t vertexBlendState;
    uint32_t vertexMaterialMode;
    uint32_t fogMode;
    uint32_t activeStageCount;
    // Exact packed point-light count, 0 through DXVK_MORROWIND_PPL_MAX_LIGHTS.
    // Zero is valid for a lit sun/ambient-only draw.
    uint32_t lightSlotCount;
    uint32_t bumpmapStage;
    uint32_t texgenStage;

    DxvkMorrowindPplStageV1 stages[DXVK_MORROWIND_PPL_MAX_STAGES];

    float projection[16];
    float worldView[4][16];
    float texgenTransform[16];

    float materialDiffuse[4];
    float materialAmbient[4];
    float materialEmissive[4];

    float sceneAmbient[4];
    float sunDiffuse[4];
    float sunDirection[4];

    float lightDiffuse[DXVK_MORROWIND_PPL_MAX_LIGHTS][4];
    float lightAmbient[DXVK_MORROWIND_PPL_MAX_LIGHTS];
    float lightPosition[3][DXVK_MORROWIND_PPL_MAX_LIGHTS];
    float lightFalloffQuadratic[DXVK_MORROWIND_PPL_MAX_LIGHTS];
    float lightFalloffConstant;

    float fogColor[4];
    float nearFogStart;
    float nearFogRange;

    float bumpMatrix[4];
    float bumpLumiScaleBias[2];
    uint32_t reserved1[2];
};

static_assert(std::is_standard_layout<DxvkMorrowindPplStageV1>::value,
    "Morrowind PPL stage ABI must be standard layout");
static_assert(std::is_standard_layout<DxvkMorrowindPplDrawV1>::value,
    "Morrowind PPL draw ABI must be standard layout");
static_assert(sizeof(DxvkMorrowindPplStageV1) == 32,
    "Unexpected Morrowind PPL stage ABI size");
static_assert(sizeof(DxvkMorrowindPplDrawV1) == 1956,
    "Unexpected Morrowind PPL draw ABI size");

// The packet layout is a function of the light count, but the client
// negotiates on the capability bit and the struct version. A build that
// changed the light count without bumping both would advertise a layout it
// does not speak: the client authorizes the expanded engine patch from the
// capability bit, then every packet fails the size check and renders through
// the legacy path at 8 lights while the engine submits far more.
static_assert(DXVK_MORROWIND_PPL_MAX_LIGHTS == 32,
    "Struct version 2 / CAP_PPL_DRAW_V2 is defined as the 32-light packet. "
    "Bump DXVK_MORROWIND_PPL_STRUCT_VERSION and add a new capability bit "
    "together with any change to the light count");

// CAP_EXPANDED_LIGHT_LIMIT is advertised unconditionally, so this is what
// makes that claim true. The ordinary fixed-function path has to reach the
// same limit as the packet: the client can leave the native path at any time
// through a keybind or a Lua call, and the engine patch does not come back.
static_assert(DXVK_D3D9_MAX_ENABLED_LIGHTS == DXVK_MORROWIND_PPL_MAX_LIGHTS,
    "Ordinary fixed-function lighting must reach the same limit as the native "
    "packet before CAP_EXPANDED_LIGHT_LIMIT may be advertised");

MIDL_INTERFACE("2ff12bfc-4622-4d9d-bcbf-1501f37e8aa3")
IDxvkMorrowindInterop : public IUnknown {
    virtual uint32_t STDMETHODCALLTYPE GetInterfaceVersion() = 0;
    virtual uint64_t STDMETHODCALLTYPE GetCapabilities() = 0;

    virtual HRESULT STDMETHODCALLTYPE ResolveDepthMinV1(
        IDirect3DSurface9* sourceMsaaDepth,
        IDirect3DSurface9* destinationIntz) = 0;
};

MIDL_INTERFACE("275c3348-5724-4a7e-aac0-46ceda965739")
IDxvkMorrowindPplInterop1 : public IUnknown {
    virtual uint64_t STDMETHODCALLTYPE GetCapabilities() = 0;

    virtual HRESULT STDMETHODCALLTYPE DrawPplV1(
        const DxvkMorrowindPplDrawV1* draw) = 0;
};

#ifndef _MSC_VER
__CRT_UUID_DECL(IDxvkMorrowindInterop,
    0x2ff12bfc, 0x4622, 0x4d9d, 0xbc, 0xbf, 0x15, 0x01, 0xf3, 0x7e, 0x8a, 0xa3);
__CRT_UUID_DECL(IDxvkMorrowindPplInterop1,
    0x275c3348, 0x5724, 0x4a7e, 0xaa, 0xc0, 0x46, 0xce, 0xda, 0x96, 0x57, 0x39);
#endif
