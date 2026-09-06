
// XE Mod Statics.fx
// MGE XE 0.16.0
// Distant statics rendering. Can be used as a core mod.


//------------------------------------------------------------
// Common functions

// Distant-static near handoff.
// Distant statics dissolve in across a radial band that ends staticHandoff units
// inside Morrowind's view range. The cut is finished per-pixel in StaticPS so that
// batched whole-cell meshes are sliced cleanly through, instead of relying on a
// vertex-granular cutoff that frays large triangles straddling the boundary.
static const float staticHandoff = 1800;     // fully opaque at nearViewRange - staticHandoff
static const float staticHandoffBand = 512;  // dissolve width, toward the camera
static const float staticHandoffCull = 1024; // extra margin for the cheap per-vertex early-out

TransformedVert transformStaticVert(StatVertIn IN) {
    // Transforms with implicit depth bias
    TransformedVert v;

    // pos.w carries the palette ordinal, so it must not scale the world translation.
    v.worldpos = mul(float4(IN.pos.xyz, 1), world);
    v.viewpos = mul(v.worldpos, view);
    v.pos = mul(v.viewpos, proj);

    // Cheap early-out: collapse geometry well inside the handoff so the player's own
    // cell isn't rasterised only to be discarded. This sits deeper (nearer) than the
    // per-pixel cut in StaticPS, so its vertex-granular edge is always masked by it.
    if(length(v.viewpos) < nearViewRange - staticHandoff - staticHandoffBand - staticHandoffCull) {
        v.pos = float4(0, 0, -10000, 0);
    }

    return v;
}

float3 staticNormalWorld(StatVertIn IN) {
    // Decompress normal
    float4 normal = float4(normalize(2 * IN.normal.xyz - 1), 0);
    return mul(normal, world).xyz;
}

float4 lightStaticVert(StatVertIn IN) {
    float3 normal = staticNormalWorld(IN);

    // Lighting (worldspace)
    // Emissive is stored in the 4th value of the normal vector
    float emissive = IN.normal.w;
    float3 light = sunCol * saturate(dot(normal, -sunVec)) + sunAmb + emissive;

    return float4(IN.color.rgb * light, IN.color.a);
}

// Shadow receiver terms, using the near receiver pass's sun estimate so the handoff matches
void shadowStaticVert(StatVertIn IN, float4 worldpos, inout StatVertOut OUT) {
    OUT.normalWS = staticNormalWorld(IN);
    OUT.shadowLight = shadowSunEstimate(saturate(dot(OUT.normalWS, -sunVec)));
    OUT.worldpos = worldpos;
}

float2 texcoordsModifier(StatVertIn IN) {
    float2 tc = IN.texcoords;

    if (hasVCol) {
        // Linked to animateUV static flag
        // Render with fixed scrolling that approximates ghostfence
        tc.y += fmod(0.08 * time, 1);
    }
    return tc;
}

//------------------------------------------------------------
// Statics rendering

StatVertOut StaticExteriorVS(StatVertIn IN) {
    StatVertOut OUT;
    TransformedVert v = transformStaticVert(IN);
    OUT.pos = v.pos;
    OUT.color = lightStaticVert(IN);
    shadowStaticVert(IN, v.worldpos, OUT);

    // Fogging (exterior)
    float3 eyevec = v.worldpos.xyz - eyePos.xyz;
    float dist = length(eyevec);
    OUT.fog = fogColour(eyevec / dist, dist);

    OUT.texcoords_range = float3(texcoordsModifier(IN), dist);
    OUT.uvBounds = uvBoundPalette[(int)IN.pos.w];
    return OUT;
}

StatVertOut StaticInteriorVS (StatVertIn IN) {
    StatVertOut OUT;
    TransformedVert v = transformStaticVert(IN);
    OUT.pos = v.pos;
    OUT.color = lightStaticVert(IN);

    // No sun shadows without weather
    OUT.shadowLight = 0;
    OUT.worldpos = v.worldpos;
    OUT.normalWS = float3(0, 0, 1);

    // Fogging (interior)
    float dist = length(v.viewpos.xyz);
    OUT.fog = fogMWColour(dist);

    OUT.texcoords_range = float3(texcoordsModifier(IN), dist);
    OUT.uvBounds = uvBoundPalette[(int)IN.pos.w];
    return OUT;
}

float4 StaticPS (StatVertOut IN): COLOR0 {
    float2 texcoords = IN.texcoords_range.xy;
    float range = IN.texcoords_range.z;

    float2 scale = IN.uvBounds.yw - IN.uvBounds.zx;
    float2 dx = ddx(texcoords) * scale;
    float2 dy = ddy(texcoords) * scale;
    float2 atlasUV = IN.uvBounds.zx + frac(texcoords) * scale;
    float4 result = tex2Dgrad(sampBaseTex, atlasUV, dx, dy);

    result.rgb *= IN.color.rgb;

    // Sun shadow, darkened the same way as the near receiver pass
    [branch] if(shadowDistant > 0) {
        float v = IN.shadowLight * shadowVisibility(IN.worldpos, normalize(IN.normalWS), sunVec);
        result.rgb *= 1 - v * shadecolor;
    }

    result.rgb = fogApply(result.rgb, IN.fog);

    // Near handoff: dissolve distant statics in across the radial band so a batched
    // cell mesh is cut per-pixel (cleanly through the batch) at the boundary. The
    // clip is the load-bearing cut; the coverage scale just softens the edge.
    float dissolve = saturate((range - (nearViewRange - staticHandoff - staticHandoffBand)) / staticHandoffBand);
    clip(dissolve - 1.0/255.0);

    // Alpha to coverage conversion
    result.a = calc_coverage(result.a, 133.0/255.0, 2.0) * dissolve;

    return result;
}

//------------------------------------------------------------
// Depth buffer output

DepthVertOut DepthStaticVS (StatVertIn IN) {
    DepthVertOut OUT;

    TransformedVert v = transformStaticVert(IN);
    OUT.pos = v.pos;

    OUT.depth = OUT.pos.w;
    OUT.alpha = 1;
    OUT.texcoords = texcoordsModifier(IN);
    OUT.uvBounds = uvBoundPalette[(int)IN.pos.w];

    return OUT;
}

float4 DepthStaticPS (DepthVertOut IN) : COLOR0 {
    clip(IN.depth - nearViewRange);

    if(hasAlpha) {
        float2 scale = IN.uvBounds.yw - IN.uvBounds.zx;
        float2 dx = ddx(IN.texcoords) * scale;
        float2 dy = ddy(IN.texcoords) * scale;
        float2 atlasUV = IN.uvBounds.zx + frac(IN.texcoords) * scale;
        float alpha = tex2Dgrad(sampBaseTex, atlasUV, dx, dy).a;
        clip(alpha - 133.0/255.0);
    }

    return IN.depth;
}
