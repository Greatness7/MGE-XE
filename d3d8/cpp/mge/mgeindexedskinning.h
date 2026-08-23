#pragma once

#include <cstdint>

#include "proxydx/d3d9header.h"

constexpr std::uint32_t MGE_INDEXED_SKINNING_CAPS_VERSION = 1;
constexpr std::uint32_t MGE_INDEXED_SKINNING_PALETTE_SIZE = 8;

struct MgeIndexedSkinningCaps {
    std::uint32_t structVersion;
    std::uint32_t maxPaletteBones;
};

static const GUID IID_IMgeIndexedSkinningCaps = {
    0xb1f69e22, 0x7ff5, 0x4e59, { 0x9d, 0x8c, 0x27, 0xc9, 0x65, 0x99, 0x3f, 0xae }
};

struct IMgeIndexedSkinningCaps : IUnknown {
    virtual HRESULT _stdcall GetIndexedSkinningCaps(MgeIndexedSkinningCaps* caps) = 0;
};
