#ifndef DXVK_MORROWIND_LIMITS_H
#define DXVK_MORROWIND_LIMITS_H

// Shared Morrowind/DXVK light limits.
//
// This file is included from C++ and from GLSL shader sources, and is kept
// byte-for-byte equivalent in the independent MGE-XE and DXVK trees, so it must
// contain nothing but include guards and integer macros.

// Ordinary D3D9 fixed-function lighting: packed light array and reported
// D3DCAPS9::MaxActiveLights.
#define DXVK_D3D9_MAX_ENABLED_LIGHTS 32

// Native Morrowind per-pixel lighting draw packet: point-light slot count.
#define DXVK_MORROWIND_PPL_MAX_LIGHTS 32

#endif
