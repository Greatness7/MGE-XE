// Darker Nights
// Exterior night darkening and saturation grade.

// Compatibility: MGE XE 0
// Skip interiors, interior-like cells, and underwater scenes.
int mgeflags = 13;


float nightDarkness = 0.22;
float nightSaturation = 1.12;
float nightContrast = 1.0;


texture lastshader;
sampler sceneSampler = sampler_state {
    texture = <lastshader>;
    addressu = clamp;
    addressv = clamp;
    magfilter = linear;
    minfilter = linear;
};

float3 sunpos;


float4 mainPS(float2 uv : TEXCOORD0) : COLOR0
{
    float4 scene = tex2Dlod(sceneSampler, float4(uv, 0, 0));

    // Fade the grade out as the sun climbs above the horizon.
    float nightAmount = 1.0 - saturate(max(sunpos.z, 0.0) * 4.0);
    float saturation = lerp(1.0, nightSaturation, nightAmount);
    float luminance = dot(scene.rgb, float3(0.2126, 0.7152, 0.0722));
    float3 graded = lerp(luminance.xxx, scene.rgb, saturation);
    graded = pow(saturate(graded), lerp(1.0, nightContrast, nightAmount));
    graded *= 1.0 - saturate(nightDarkness) * nightAmount;

    scene.rgb = saturate(graded);
    return scene;
}

technique T0 < string MGEinterface = "MGE XE 0"; string category = "tone"; >
{
    pass { PixelShader = compile ps_3_0 mainPS(); }
}
