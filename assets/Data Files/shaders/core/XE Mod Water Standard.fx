
// XE Mod Water Standard.fx
// The standard look of the surface of a water volume. Can be used as a core mod.
// It is written as the water shader of a mod is (docs/water-shaders.md), with nothing that
// such a shader cannot use: the standard look is the first user of that contract.

float4 WaterShaderPS(in WaterVolumeVertOut IN) : COLOR0
{
    WaterSurfaceLook look = surfaceLook(IN.color);
    WaterShade shade = shadeWaterVolume(IN.water, IN.facing, IN.tint, look);
    return finishWaterVolume(IN.water, shade, IN.tint, look);
}
