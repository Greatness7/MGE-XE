
// XE Mod Shadow.fx
// MGE XE 0.16.0
// Shadow receiver functions. Can be used as a core mod.

#include "XE Mod Shadow Data.fx"



//------------------------------------------------------------
// Texture atlas

// Clip space margin of 4 texels, to prevent bleeding from the filter kernel + adjacent textures
// Clip space limits are -1 to +1, so texel dimensions are multiplied by two here
static float3 atlasMargin = float3(1-2*4*shadowRcpRes, 1-2*4*shadowRcpRes, 1);

// Shadow UV to shadow atlas UV
float4 mapShadowToAtlas(float2 t, int layer) {
    // Result is intended for use with tex2Dlod
    return float4(t.x * shadowCascadeSize + layer * shadowCascadeSize, t.y, 0, 0);
}

//------------------------------------------------------------
// Incoming vertex sunlight estimation

float shadowSunEstimate(float lambert) {
    float x = lambert * dot(sunCol, float3(0.36, 0.53, 0.11));
    x *= 0.25 + 0.75 * sunVis;
    // Fade out over the last degrees of elevation, where the fit clamps the light direction
    x *= smoothstep(sin(radians(shadowElevationFade.x)), sin(radians(shadowElevationFade.y)), -sunVec.z);
    return x / (shade + x);
}

TransformedVert transformShadowVert(MorrowindVertIn IN) {
    TransformedVert v;
    float4 normal = float4(IN.normal.xyz, 0);

    // Skin mesh if required
    if(hasBones) {
        v.viewpos = skin(IN.pos, IN.blendweights);
        v.normal = normalize(skin(normal, IN.blendweights));
    }
    else {
        v.viewpos = mul(IN.pos, vertexBlendPalette[0]);
        v.normal = mul(normal, vertexBlendPalette[0]);
    }

    v.pos = mul(v.viewpos, proj);
    return v;
}

//------------------------------------------------------------
// Cascaded ortho percentage-closer lookup
//
// Set 0 (shadowViewProj[0..3], shadowCascade[0..3]) is the current atlas, set 1 the next,
// cross-faded by shadowBlend. Receivers project per pixel in the space the matrices expect:
// view space with sunVecView in the near receiver pass, world space with sunVec otherwise.

// Receiver in a cascade's clip space, pushed along the normal against self-shadowing, more at
// grazing angles
float4 shadowReceiverPos(float4 pos, float3 normal, float3 sunDir, int set, int layer) {
    float ndotl = saturate(dot(normal, -sunDir));
    float slope = sqrt(1 - ndotl * ndotl);
    float offset = min(shadowNormalOffset * (0.5 + slope) * shadowCascade[set * shadowCascades + layer].x, shadowNormalOffsetMax);
    float4 sp = mul(pos + float4(normal * offset, 0), shadowViewProj[set * shadowCascades + layer]);
    sp.z /= sp.w;
    return sp;
}

// Lit fraction in [0, 1]: a 3x3 grid of bilinear compare taps, shadowFilterRadius cascade 0
// texels apart in world units, or one tap from shadowSingleTapCascade on
float shadowLayerLit(sampler atlas, float4 shadowpos, int set, int layer) {
    float4 cascade = shadowCascade[set * shadowCascades + layer];
    float2 shadowUV = (0.5 + 0.5*shadowRcpRes) + float2(0.5, -0.5) * shadowpos.xy;
    float4 t = mapShadowToAtlas(shadowUV, layer);
    t.z = shadowpos.z - (shadowBias + shadowBiasTexels * cascade.x) / cascade.y;

    // layer is a literal once the caller's loop is unrolled, so this resolves at compile time
    if(layer >= shadowSingleTapCascade) {
        return tex2Dlod(atlas, t).r;
    }

    float spacing = shadowFilterRadius * shadowCascade[set * shadowCascades].x / cascade.x;
    float2 d = spacing * shadowRcpRes * float2(shadowCascadeSize, 1);

    float lit = 0;
    [unroll] for(int y = -1; y <= 1; ++y) {
        [unroll] for(int x = -1; x <= 1; ++x) {
            lit += tex2Dlod(atlas, t + float4(x * d.x, y * d.y, 0, 0)).r;
        }
    }
    return lit / 9.0;
}

// Innermost containing cascade from minLayer up, faded at the outer edge. layerOut is the
// cascade used, shadowCascades if none
float shadowSetVisibility(sampler atlas, int set, float4 pos, float3 normal, float3 sunDir, int minLayer, out int layerOut) {
    layerOut = shadowCascades;
    [unroll] for(int i = 0; i < shadowCascades; ++i) {
        [branch] if(i >= minLayer) {
            float4 sp = shadowReceiverPos(pos, normal, sunDir, set, i);
            [branch] if(all(saturate(atlasMargin - abs(sp.xyz)))) {
                float v = 1 - shadowLayerLit(atlas, sp, set, i);
                if(i == shadowCascades - 1) {
                    float2 fade = saturate(25 * (1 - abs(sp.xy)));
                    v *= fade.x * fade.y;
                }
                layerOut = i;
                return v;
            }
        }
    }
    return 0;
}

// Shadow term in [0, 1], 1 fully shadowed, cross-faded between the atlases. The next atlas is
// walked from the current one's cascade: a finer one could contain the point only next to a
// boundary
float shadowVisibility(float4 pos, float3 normal, float3 sunDir) {
    int layer, layerNext;
    float v = shadowSetVisibility(sampShadow, 0, pos, normal, sunDir, 0, layer);
    [branch] if(shadowBlend > 0) {
        int minLayer = (layer < shadowCascades) ? layer : 0;
        v = lerp(v, shadowSetVisibility(sampShadowNext, 1, pos, normal, sunDir, minLayer, layerNext), shadowBlend);
    }
    return v;
}

//------------------------------------------------------------
// Shadow reciever rendering

struct RenderShadowVertOut {
    float4 pos: POSITION;
    half2 texcoords: TEXCOORD0;
    centroid float light: COLOR0;
    centroid float alpha: COLOR1;

    float4 viewpos: TEXCOORD1;
    float3 normal: TEXCOORD2;
};

RenderShadowVertOut RenderShadowsBaseVS(MorrowindVertIn IN) {
    RenderShadowVertOut OUT;
    TransformedVert v = transformShadowVert(IN);

    OUT.pos = v.pos;

    // Fragment colour routing
    OUT.alpha = vertexMaterial(IN.color).a;

    // Non-standard shadow luminance, to create sufficient contrast when ambient is high
    OUT.light = shadowSunEstimate(saturate(dot(v.normal.xyz, -sunVecView)));

    // Fog attenuation (shadow darkness and distance fade)
    float fogatt = pow(fogMWScalar(OUT.pos.w), 2);
    if(isAboveSeaLevel(eyePos))
        OUT.light *= fogatt;
    else
        OUT.light *= saturate(4 * fogatt);

    // Light space projection happens per pixel
    OUT.viewpos = v.viewpos;
    OUT.normal = v.normal.xyz;

    OUT.texcoords = IN.texcoords;
    return OUT;
}

RenderShadowVertOut RenderShadowsVS(MorrowindVertIn IN) {
    RenderShadowVertOut OUT = RenderShadowsBaseVS(IN);

    // Depth bias to mitigate difference between FFP and VS
    OUT.pos.z *= 1 - 2e-6;
    OUT.pos.z -= clamp(0.05 / OUT.pos.w, 0, 1e-3);
    return OUT;
}

RenderShadowVertOut RenderShadowsFFEVS(MorrowindVertIn IN) {
    RenderShadowVertOut OUT = RenderShadowsBaseVS(IN);

    // Depth bias to mitigate difference between the FFE pass and this one.
    // Native PPL draw packets transform vertices in DXVK's own shader, so FFE
    // depth is no longer bit-identical to the transform above.
    OUT.pos.z *= 1 - 2e-6;
    OUT.pos.z -= clamp(0.05 / OUT.pos.w, 0, 1e-3);
    return OUT;
}

TransformedVert transformShadowIndexedVert(MorrowindIndexedVertIn IN) {
    TransformedVert v;
    float4 normal = float4(IN.normal.xyz, 0);
    v.viewpos = indexedSkin(IN.pos, IN.blendweights, IN.blendindices);
    v.normal = normalize(indexedSkin(normal, IN.blendweights, IN.blendindices));
    v.pos = mul(v.viewpos, proj);
    return v;
}

RenderShadowVertOut RenderShadowsIndexedBaseVS(MorrowindIndexedVertIn IN) {
    RenderShadowVertOut OUT;
    TransformedVert v = transformShadowIndexedVert(IN);

    OUT.pos = v.pos;
    OUT.alpha = vertexMaterial(IN.color).a;
    OUT.light = shadowSunEstimate(saturate(dot(v.normal.xyz, -sunVecView)));

    float fogatt = pow(fogMWScalar(OUT.pos.w), 2);
    if(isAboveSeaLevel(eyePos))
        OUT.light *= fogatt;
    else
        OUT.light *= saturate(4 * fogatt);

    OUT.viewpos = v.viewpos;
    OUT.normal = v.normal.xyz;
    OUT.texcoords = IN.texcoords;
    return OUT;
}

RenderShadowVertOut RenderShadowsIndexedVS(MorrowindIndexedVertIn IN) {
    RenderShadowVertOut OUT = RenderShadowsIndexedBaseVS(IN);
    OUT.pos.z *= 1 - 2e-6;
    OUT.pos.z -= clamp(0.05 / OUT.pos.w, 0, 1e-3);
    return OUT;
}

RenderShadowVertOut RenderShadowsFFEIndexedVS(MorrowindIndexedVertIn IN) {
    RenderShadowVertOut OUT = RenderShadowsIndexedBaseVS(IN);
    OUT.pos.z *= 1 - 2e-6;
    OUT.pos.z -= clamp(0.05 / OUT.pos.w, 0, 1e-3);
    return OUT;
}

float4 RenderShadowsPS(RenderShadowVertOut IN): COLOR0 {
    // Early reject unlit areas
    clip(IN.light - 2.0/255.0);

    // Respect alpha test
    float alpha = IN.alpha;
    if(hasAlpha) {
        alpha *= tex2D(sampBaseTex, IN.texcoords).a;
        clip(alpha - alphaRef);
    }

    // Soft shadowing
    float v = shadowVisibility(IN.viewpos, normalize(IN.normal), sunVecView) * IN.light * alpha;

    // Darken shadow area according to existing lighting (slightly towards blue)
    clip(v - 2.0/255.0);
    return float4(v * shadecolor, 1);
}
