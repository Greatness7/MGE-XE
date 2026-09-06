
// XE Mod Shadow Data.fx
// MGE XE 0.16.0
// Shared constants



//------------------------------------------------------------
// Engine parameters

// Number of cascades that are rendered. Do not change this in a core mod: it must match
// DistantLand::kShadowCascades, which sizes the atlas and the per-cascade matrices, and
// nothing checks that the two agree.
static const int shadowCascades = 4;

// Shadow atlas texture scaling factor
static const float shadowCascadeSize = 1. / shadowCascades;

//------------------------------------------------------------
// Shadow parameters

// Luminance floor for shadowed areas; higher is lighter
static const float shade = 0.6;

// Per-channel shadow strength, kept near neutral: a blue cast reads as fog
static const float3 shadecolor = float3(1.0, 0.985, 0.93);

// Constant depth bias, world units. Must exceed the offset between a caster and its near
// twin (LOD error); the slope-scaled bias is on the caster side (XE Shadowmap.fx)
static const float shadowBias = 24.0;

// Extra bias in texels of the sampled cascade
static const float shadowBiasTexels = 1.0;

// Receiver push along its normal, in texels of the sampled cascade, 0.5x facing the sun to
// 1.5x grazing
static const float shadowNormalOffset = 1.5;

// Cap on that push, world units
static const float shadowNormalOffsetMax = 16.0;

// 3x3 tap spacing in cascade 0 texels, kept in world units on the cascades that use the grid;
// penumbra about 3 * radius + 1 texels
static const float shadowFilterRadius = 2.0;

// First cascade sampled with one compare instead of the 3x3 grid: from here the grid falls
// inside a quarter texel
static const int shadowSingleTapCascade = 2;

// World units the terrain caster is lowered by, so its coarse mesh stays below roads built on it
static const float shadowTerrainSink = 24.0;

// Elevation band, degrees on sunVec, over which the shadow term fades out. The top must match
// shadowMinElevation in rendershadow.cpp. Dormant with the vanilla light (13.8 degree minimum)
static const float2 shadowElevationFade = float2(5.0, 10.0);
