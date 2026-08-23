
// XE Mod Landscape.fx
// MGE XE 0.16.0
// Distant landscape rendering. Can be used as a core mod.


//------------------------------------------------------------
// Common functions

shared texture terrainAtlasTex;
shared texture terrainMaterialTex;
shared texture terrainMaterialFlagsTex;
shared texture terrainPatchAlbedoTex;
shared texture terrainBlendPatternsTex;
shared float2 terrainWorldOrigin;
shared float2 terrainInvAtlasSize;
shared float2 terrainInvMaterialSize;
shared float terrainLogicalTileSize;
shared float terrainGutterSize;
shared float terrainPhysicalTileSize;
shared float terrainTilesPerRow;
shared int terrainAtlasMaxLod;
shared float terrainPatternCount;
shared float terrainPatternTileSize;
shared float terrainPatternGutterSize;
shared float terrainPatternPhysicalSize;
shared float terrainPatternsPerRow;

sampler terrainAtlasSampler = sampler_state {
    texture = <terrainAtlasTex>;
    minfilter = linear;
    magfilter = linear;
    mipfilter = linear;
    maxmiplevel = terrainAtlasMaxLod;
    addressu = clamp;
    addressv = clamp;
};

sampler terrainMaterialSampler = sampler_state {
    texture = <terrainMaterialTex>;
    minfilter = point;
    magfilter = point;
    mipfilter = none;
    addressu = clamp;
    addressv = clamp;
};

sampler terrainMaterialFlagsSampler = sampler_state {
    texture = <terrainMaterialFlagsTex>;
    minfilter = point;
    magfilter = point;
    mipfilter = none;
    addressu = clamp;
    addressv = clamp;
};

sampler terrainBlendPatternSampler = sampler_state {
    texture = <terrainBlendPatternsTex>;
    minfilter = linear;
    magfilter = linear;
    mipfilter = none;
    addressu = clamp;
    addressv = clamp;
};

sampler terrainPatchAlbedoSampler = sampler_state {
    texture = <terrainPatchAlbedoTex>;
    minfilter = linear;
    magfilter = linear;
    mipfilter = linear;
    addressu = clamp;
    addressv = clamp;
};

TransformedVert transformLandVert(float4 pos) {
    TransformedVert v;

    v.viewpos = mul(mul(pos, world), view);
    v.pos = mul(v.viewpos, proj);
    return v;
}

//------------------------------------------------------------
// Distant land height bias to prevent low lod meshes from clipping

float landBias(float dist) {
    float maxDist = nearViewRange - 1152;
    return -1 + -2 * max(0, maxDist - dist);
}

//------------------------------------------------------------
// Distant landscape rendering

struct TerrainVertIn {
    float4 position : POSITION;
    float4 normal : NORMAL;
    float4 color : COLOR0;
};

struct TerrainVertOut {
    float4 pos: POSITION;
    float2 terrainLocalXY : TEXCOORD0;
    float3 normalWS : TEXCOORD1;
    float3 color : TEXCOORD2;
    centroid float4 fog : TEXCOORD3;
};

float decodeByte(float value) {
    return floor(value * 255.0 + 0.5);
}

float decodeU16(float2 encoded) {
    return decodeByte(encoded.x) + 256.0 * decodeByte(encoded.y);
}

float2 terrainAtlasUv(float texId, float2 patchUv) {
    float tileY = floor(texId / terrainTilesPerRow);
    float2 tileOrigin = float2(texId - tileY * terrainTilesPerRow, tileY) * terrainPhysicalTileSize;
    float2 interior = tileOrigin + terrainGutterSize;
    return (interior + patchUv * terrainLogicalTileSize) * terrainInvAtlasSize;
}

float2 terrainPatternInvSize() {
    float patternRows = floor((terrainPatternCount - 1.0) / terrainPatternsPerRow) + 1.0;
    return 1.0 / float2(
        terrainPatternsPerRow * terrainPatternPhysicalSize,
        patternRows * terrainPatternPhysicalSize
    );
}

float2 terrainPatternUv(float patternId, float2 patchUv) {
    float tileY = floor(patternId / terrainPatternsPerRow);
    float2 tileOrigin = float2(patternId - tileY * terrainPatternsPerRow, tileY) * terrainPatternPhysicalSize;
    float2 interior = tileOrigin + terrainPatternGutterSize;
    return (interior + patchUv * terrainPatternTileSize) * terrainPatternInvSize();
}

float samplePattern(float patternId, float2 patchUv) {
    float clampedPatternId = min(patternId, max(terrainPatternCount - 1.0, 0.0));
    return tex2D(terrainBlendPatternSampler, terrainPatternUv(clampedPatternId, patchUv)).r;
}

TerrainVertOut LandscapeVS(TerrainVertIn IN) {
    TerrainVertOut OUT;
    float4 pos = IN.position;

    float3 eyevec = mul(pos, world).xyz - eyePos.xyz;
    float dist = length(eyevec);
    OUT.fog = fogColour(eyevec / dist, dist);

    // Move land down to avoid it appearing where reduced mesh doesn't match
    pos.z += landBias(dist);

    TransformedVert v = transformLandVert(pos);
    OUT.pos = v.pos;
    OUT.terrainLocalXY = mul(pos, world).xy - terrainWorldOrigin;
    OUT.normalWS = 2.0 * IN.normal.xyz - 1.0;
    OUT.color = IN.color.rgb;
    return OUT;
}

TerrainVertOut LandscapeReflVS(TerrainVertIn IN) {
    TerrainVertOut OUT;
    float4 pos = IN.position;

    float3 eyevec = mul(pos, world).xyz - eyePos.xyz;
    float dist = length(eyevec);
    if(isAboveSeaLevel(eyePos))
        OUT.fog = fogColour(eyevec / dist, dist);
    else
        OUT.fog = fogMWColour(dist);

    // Sink land near waterline a small amount
    pos.z += -16 * saturate(1 - pos.z/16);

    TransformedVert v = transformLandVert(pos);
    OUT.pos = v.pos;
    OUT.terrainLocalXY = mul(pos, world).xy - terrainWorldOrigin;
    OUT.normalWS = 2.0 * IN.normal.xyz - 1.0;
    OUT.color = IN.color.rgb;
    return OUT;
}

float4 LandscapePS(TerrainVertOut IN) : COLOR0 {
    float2 patchCoord = IN.terrainLocalXY / 512.0;
    float2 ddxPatch = ddx(patchCoord);
    float2 ddyPatch = ddy(patchCoord);
    float2 patchUv = frac(patchCoord);
    float2 ddxAtlas = ddxPatch * terrainLogicalTileSize * terrainInvAtlasSize;
    float2 ddyAtlas = ddyPatch * terrainLogicalTileSize * terrainInvAtlasSize;
    float2 materialUv = (floor(patchCoord) + 0.5) * terrainInvMaterialSize;

    float4 materialSample = tex2D(terrainMaterialSampler, materialUv);
    float4 materialFlagsSample = tex2D(terrainMaterialFlagsSampler, materialUv);
    float baseId = decodeU16(materialSample.rg);
    float decalId = decodeU16(materialSample.ba);
    float patternId = decodeByte(materialFlagsSample.r);
    float hasDecal = fmod(decodeByte(materialFlagsSample.g), 2.0);
    float3 baseAlbedo = tex2Dgrad(terrainAtlasSampler, terrainAtlasUv(baseId, patchUv), ddxAtlas, ddyAtlas).rgb;
    float3 decalAlbedo = tex2Dgrad(terrainAtlasSampler, terrainAtlasUv(decalId, patchUv), ddxAtlas, ddyAtlas).rgb;
    float alpha = samplePattern(patternId, patchUv) * hasDecal;
    float3 nearAlbedo = lerp(baseAlbedo, decalAlbedo, alpha);
    float3 farAlbedo = tex2D(terrainPatchAlbedoSampler, patchCoord * terrainInvMaterialSize).rgb;
    float footprint = max(length(ddxPatch), length(ddyPatch));
    float coarse = smoothstep(0.5, 1.5, footprint);
    float3 albedo = lerp(nearAlbedo, farAlbedo, coarse);
    float3 lighting = sunAmb + sunCol * saturate(dot(normalize(IN.normalWS), -sunVec));
    float3 result = albedo * IN.color * lighting;
    return float4(fogApply(result, IN.fog), 1);
}

DepthVertOut DepthLandVS(TerrainVertIn IN) {
    DepthVertOut OUT;
    float4 pos = IN.position;

    // Move land down to avoid it appearing where reduced mesh doesn't match
    pos.z += landBias(length(pos.xyz - eyePos.xyz));

    TransformedVert v = transformLandVert(pos);
    OUT.pos = v.pos;
    OUT.depth = v.pos.w;
    OUT.alpha = 1;
    OUT.texcoords = 0;
    OUT.uvBounds = float4(0, 0, 1, 1);

    return OUT;
}

float4 DepthLandPS(DepthVertOut IN) : COLOR0 {
    clip(IN.depth - nearViewRange);
    return IN.depth;
}