
#include "distantland.h"
#include "distantshader.h"
#include "configuration.h"
#include "dlformat.h"
#include "mwbridge.h"
#include "proxydx/d3d8header.h"

#include <algorithm>
#include <cmath>


namespace {
    void applyTerrainShaderState(ID3DXEffect* terrainEffect) {
        terrainEffect->SetTexture(DistantLand::ehTerrainAtlasTex, DistantLand::texTerrainAtlas);
        terrainEffect->SetTexture(DistantLand::ehTerrainMaterialTex, DistantLand::texTerrainMaterial);
        terrainEffect->SetTexture(DistantLand::ehTerrainMaterialFlagsTex, DistantLand::texTerrainMaterialFlags);
        terrainEffect->SetTexture(DistantLand::ehTerrainPatchAlbedoTex, DistantLand::texTerrainPatchAlbedo);
        terrainEffect->SetTexture(DistantLand::ehTerrainBlendPatternsTex, DistantLand::texTerrainBlendPatterns);
        terrainEffect->SetFloatArray(DistantLand::ehTerrainWorldOrigin, &DistantLand::terrainConstants.worldOrigin.x, 2);
        terrainEffect->SetFloatArray(DistantLand::ehTerrainInvAtlasSize, &DistantLand::terrainConstants.invAtlasSize.x, 2);
        terrainEffect->SetFloatArray(DistantLand::ehTerrainInvMaterialSize, &DistantLand::terrainConstants.invMaterialSize.x, 2);
        terrainEffect->SetFloat(DistantLand::ehTerrainLogicalTileSize, DistantLand::terrainConstants.logicalTileSize);
        terrainEffect->SetFloat(DistantLand::ehTerrainGutterSize, DistantLand::terrainConstants.gutterSize);
        terrainEffect->SetFloat(DistantLand::ehTerrainPhysicalTileSize, DistantLand::terrainConstants.physicalTileSize);
        terrainEffect->SetFloat(DistantLand::ehTerrainTilesPerRow, DistantLand::terrainConstants.tilesPerRow);
        terrainEffect->SetInt(DistantLand::ehTerrainAtlasMaxLod, static_cast<int>(DistantLand::terrainConstants.atlasMaxLod));
        terrainEffect->SetFloat(DistantLand::ehTerrainPatternCount, DistantLand::terrainConstants.patternCount);
        terrainEffect->SetFloat(DistantLand::ehTerrainPatternTileSize, DistantLand::terrainConstants.patternTileSize);
        terrainEffect->SetFloat(DistantLand::ehTerrainPatternGutterSize, DistantLand::terrainConstants.patternGutterSize);
        terrainEffect->SetFloat(DistantLand::ehTerrainPatternPhysicalSize, DistantLand::terrainConstants.patternPhysicalSize);
        terrainEffect->SetFloat(DistantLand::ehTerrainPatternsPerRow, DistantLand::terrainConstants.patternsPerRow);
    }

    void setSamplerStateForTexture(IDirect3DBaseTexture9* texture, D3DSAMPLERSTATETYPE state, DWORD value) {
        for (DWORD stage = 0; stage < 16; ++stage) {
            IDirect3DBaseTexture9* stageTexture = nullptr;
            if (DistantLand::device->GetTexture(stage, &stageTexture) != D3D_OK) {
                continue;
            }
            const bool matches = stageTexture == texture;
            if (stageTexture) {
                stageTexture->Release();
            }
            if (matches) {
                DistantLand::device->SetSamplerState(stage, state, value);
            }
        }
    }

    void applyTerrainSamplerStates() {
        setSamplerStateForTexture(DistantLand::texTerrainAtlas, D3DSAMP_MINFILTER, D3DTEXF_LINEAR);
        setSamplerStateForTexture(DistantLand::texTerrainAtlas, D3DSAMP_MAGFILTER, D3DTEXF_LINEAR);
        setSamplerStateForTexture(DistantLand::texTerrainAtlas, D3DSAMP_MIPFILTER, D3DTEXF_LINEAR);
        setSamplerStateForTexture(DistantLand::texTerrainAtlas, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP);
        setSamplerStateForTexture(DistantLand::texTerrainAtlas, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP);
        setSamplerStateForTexture(DistantLand::texTerrainAtlas, D3DSAMP_MAXMIPLEVEL, static_cast<DWORD>(DistantLand::terrainConstants.atlasMaxLod));

        setSamplerStateForTexture(DistantLand::texTerrainMaterial, D3DSAMP_MINFILTER, D3DTEXF_POINT);
        setSamplerStateForTexture(DistantLand::texTerrainMaterial, D3DSAMP_MAGFILTER, D3DTEXF_POINT);
        setSamplerStateForTexture(DistantLand::texTerrainMaterial, D3DSAMP_MIPFILTER, D3DTEXF_NONE);
        setSamplerStateForTexture(DistantLand::texTerrainMaterial, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP);
        setSamplerStateForTexture(DistantLand::texTerrainMaterial, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP);

        setSamplerStateForTexture(DistantLand::texTerrainMaterialFlags, D3DSAMP_MINFILTER, D3DTEXF_POINT);
        setSamplerStateForTexture(DistantLand::texTerrainMaterialFlags, D3DSAMP_MAGFILTER, D3DTEXF_POINT);
        setSamplerStateForTexture(DistantLand::texTerrainMaterialFlags, D3DSAMP_MIPFILTER, D3DTEXF_NONE);
        setSamplerStateForTexture(DistantLand::texTerrainMaterialFlags, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP);
        setSamplerStateForTexture(DistantLand::texTerrainMaterialFlags, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP);

        setSamplerStateForTexture(DistantLand::texTerrainBlendPatterns, D3DSAMP_MINFILTER, D3DTEXF_LINEAR);
        setSamplerStateForTexture(DistantLand::texTerrainBlendPatterns, D3DSAMP_MAGFILTER, D3DTEXF_LINEAR);
        setSamplerStateForTexture(DistantLand::texTerrainBlendPatterns, D3DSAMP_MIPFILTER, D3DTEXF_NONE);
        setSamplerStateForTexture(DistantLand::texTerrainBlendPatterns, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP);
        setSamplerStateForTexture(DistantLand::texTerrainBlendPatterns, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP);

        setSamplerStateForTexture(DistantLand::texTerrainPatchAlbedo, D3DSAMP_MINFILTER, D3DTEXF_LINEAR);
        setSamplerStateForTexture(DistantLand::texTerrainPatchAlbedo, D3DSAMP_MAGFILTER, D3DTEXF_LINEAR);
        setSamplerStateForTexture(DistantLand::texTerrainPatchAlbedo, D3DSAMP_MIPFILTER, D3DTEXF_LINEAR);
        setSamplerStateForTexture(DistantLand::texTerrainPatchAlbedo, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP);
        setSamplerStateForTexture(DistantLand::texTerrainPatchAlbedo, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP);
    }
}



void DistantLand::renderSky() {
    // Recorded renders
    const auto& recordSky_const = recordSky;
    const int standardCloudVerts = 65, standardCloudTris = 112;
    const int standardMoonVerts = 4, standardMoonTris = 2;

    // Render sky without clouds first
    effect->BeginPass(PASS_RENDERSKY);
    for (const auto& i : recordSky_const) {
        // Skip clouds
        if (i.texture && i.vertCount == standardCloudVerts && i.primCount == standardCloudTris) {
            continue;
        }

        // Set variables in main effect; variables are shared via effect pool
        effect->SetTexture(ehTex0, i.texture);
        if (i.texture) {
            // Textured object; draw as normal in shader, with exceptions:
            // - Sun/moon billboards do not use mipmaps
            // - Moon shadow cutout (prevents stars shining through moons)
            //   which requires colour to be replaced with atmosphere scattering colour
            bool isBillboard = (i.vertCount == standardMoonVerts && i.primCount == standardMoonTris);
            bool isMoonShadow = i.destBlend == D3DBLEND_INVSRCALPHA && !i.useLighting;

            effect->SetBool(ehHasAlpha, true);
            effect->SetBool(ehHasBones, isBillboard);
            effect->SetBool(ehHasVCol, isMoonShadow);
            device->SetRenderState(D3DRS_ALPHABLENDENABLE, 1);
            device->SetRenderState(D3DRS_SRCBLEND, i.srcBlend);
            device->SetRenderState(D3DRS_DESTBLEND, i.destBlend);
            device->SetRenderState(D3DRS_ALPHATESTENABLE, 1);
        } else {
            // Sky; perform atmosphere scattering in shader
            effect->SetBool(ehHasAlpha, false);
            effect->SetBool(ehHasVCol, true);
            device->SetRenderState(D3DRS_ALPHABLENDENABLE, 0);
            device->SetRenderState(D3DRS_ALPHATESTENABLE, 0);
        }

        effect->SetMatrix(ehWorld, &i.worldTransforms[0]);
        effect->CommitChanges();

        device->SetStreamSource(0, i.vb, i.vbOffset, i.vbStride);
        device->SetIndices(i.ib);
        device->SetFVF(i.fvf);
        device->DrawIndexedPrimitive(i.primType, i.baseIndex, i.minIndex, i.vertCount, i.startIndex, i.primCount);
    }
    effect->EndPass();

    // Render clouds with a separate shader
    effect->BeginPass(PASS_RENDERCLOUDS);
    for (const auto& i : recordSky_const) {
        // Clouds only
        if (!(i.texture && i.vertCount == standardCloudVerts && i.primCount == standardCloudTris)) {
            continue;
        }

        effect->SetTexture(ehTex0, i.texture);
        effect->SetBool(ehHasAlpha, true);
        device->SetRenderState(D3DRS_ALPHABLENDENABLE, 1);
        device->SetRenderState(D3DRS_SRCBLEND, i.srcBlend);
        device->SetRenderState(D3DRS_DESTBLEND, i.destBlend);
        device->SetRenderState(D3DRS_ALPHATESTENABLE, 1);
        effect->SetMatrix(ehWorld, &i.worldTransforms[0]);
        effect->CommitChanges();

        device->SetStreamSource(0, i.vb, i.vbOffset, i.vbStride);
        device->SetIndices(i.ib);
        device->SetFVF(i.fvf);
        device->DrawIndexedPrimitive(i.primType, i.baseIndex, i.minIndex, i.vertCount, i.startIndex, i.primCount);
    }
    effect->EndPass();
}

void DistantLand::renderDistantLand(ID3DXEffect* e, const D3DXMATRIX* view, const D3DXMATRIX* proj) {
    D3DXMATRIX world, viewproj = (*view) * (*proj);

    // Cull and draw
    ViewFrustum frustum(&viewproj);

    D3DXMatrixIdentity(&world);
    effect->SetMatrix(ehWorld, &world);
    if (e == effect) {
        applyTerrainShaderState(e);
    }
    e->CommitChanges();
    if (e == effect) {
        applyTerrainSamplerStates();
    }

    device->SetVertexDeclaration(TerrainDecl);
    visLandShared.RemoveAll();
    ipcClient.getVisibleMeshesCoarse(visLandSharedId, frustum, VIS_LAND);
    visLandShared.Render(device, TerrainBin::VertexStride, true);
}

void DistantLand::renderDistantLandZ() {
    D3DXMATRIX world;

    D3DXMatrixIdentity(&world);
    effect->SetMatrix(DistantLand::ehWorld, &world);
    effectDepth->CommitChanges();

    // Draw with cached vis set
    device->SetVertexDeclaration(TerrainDecl);
    visLandShared.Render(device, TerrainBin::VertexStride);
}

// Set by the FFI horizon setters in apiffi.cpp when the MWSE menu tweaks a value.
extern bool g_horizonDirty;

void DistantLand::cullDistantStatics(const D3DXMATRIX* view, const D3DXMATRIX* proj) {
    // Push any live horizon-culling tweaks to the host before this frame's visibility queries.
    if (g_horizonDirty) {
        const auto& h = Configuration.Horizon;
        IPC::SetHorizonConfigParameters horizon;
        horizon.enabled = h.Culling ? 1u : 0u;
        horizon.biasZ = h.BiasZ;
        horizon.objectBiasZ = h.ObjectBiasZ;
        horizon.nearUnits = h.NearUnits;
        horizon.ringStep = h.RingStep;
        horizon.maxRange = h.MaxRange;
        horizon.bins = h.Bins;
        horizon.sampleSpacing = h.SampleSpacing;
        horizon.adaptiveGate = h.AdaptiveGate ? 1u : 0u;
        ipcClient.setHorizonConfig(horizon);
        g_horizonDirty = false;
    }

    D3DXMATRIX ds_proj = *proj, ds_viewproj;
    D3DXVECTOR4 viewsphere(eyePos.x, eyePos.y, eyePos.z, 0);
    const float nearStaticHandoff = nearViewRange - 768.0f;
    // Length of the frustum corner ray per unit of view-space depth. The near and far
    // planes clip view-space z, but both the handoff distance and the host's view-sphere
    // test are radial, so converting between them needs this factor in both directions.
    const float tanX = 1.0f / proj->_11;
    const float tanY = 1.0f / proj->_22;
    const float cornerRay = std::sqrt(1.0f + tanX * tanX + tanY * tanY);
    // Divided, so high-FOV edge statics at the handoff are still considered.
    float zn = nearStaticHandoff / cornerRay, zf = zn;
    float cullDist = fogEnd;
    const float nearStaticEnd = Configuration.DL.NearStaticEnd * kCellSize;
    const float farStaticEnd = Configuration.DL.FarStaticEnd * kCellSize;
    const float veryFarStaticEnd = Configuration.DL.VeryFarStaticEnd * kCellSize;

    visDistantShared.RemoveAll();

    zf = std::min(nearStaticEnd, cullDist);
    if (zn < zf) {
        editProjectionZ(&ds_proj, zn, zf);
        ds_viewproj = (*view) * ds_proj;
        ViewFrustum range_frustum(&ds_viewproj);
        // Multiplied, so the host's radial view-sphere never clips inside the frustum.
        viewsphere.w = zf * cornerRay;
        ipcClient.getVisibleMeshes(visDistantSharedId, range_frustum, viewsphere, VIS_NEAR, nearStaticEnd, farStaticEnd);
    }

    zf = std::min(farStaticEnd, cullDist);
    if (zn < zf) {
        editProjectionZ(&ds_proj, zn, zf);
        ds_viewproj = (*view) * ds_proj;
        ViewFrustum range_frustum(&ds_viewproj);
        viewsphere.w = zf * cornerRay;
        ipcClient.getVisibleMeshes(visDistantSharedId, range_frustum, viewsphere, VIS_FAR, nearStaticEnd, farStaticEnd);
    }

    zf = std::min(veryFarStaticEnd, cullDist);
    if (zn < zf) {
        editProjectionZ(&ds_proj, zn, zf);
        ds_viewproj = (*view) * ds_proj;
        ViewFrustum range_frustum(&ds_viewproj);
        viewsphere.w = zf * cornerRay;
        ipcClient.getVisibleMeshes(visDistantSharedId, range_frustum, viewsphere, VIS_VERY_FAR, nearStaticEnd, farStaticEnd);
    }

    ipcClient.sortVisibleSet(visDistantSharedId, VisibleSetSort::ByState);
    ipcClient.waitForCompletion();
}

void DistantLand::renderDistantStatics() {
    if (!MWBridge::get()->IsExterior()) {
        // Set clipping to stop large architectural meshes (that don't match exactly)
        // from visible overdrawing and causing z-buffer occlusion
        float clipAt = nearViewRange - 768.0f;
        D3DXPLANE clipPlane(0, 0, clipAt, -(mwProj._33 * clipAt + mwProj._43));
        device->SetClipPlane(0, clipPlane);
        device->SetRenderState(D3DRS_CLIPPLANEENABLE, 1);
    }

    device->SetVertexDeclaration(StaticDecl);

    visDistantShared.Render(device, effect, effect, &ehTex0, nullptr, &ehHasVCol, &ehWorld, &ehUvBoundPalette, SIZEOFSTATICVERT);

    device->SetRenderState(D3DRS_CLIPPLANEENABLE, 0);
}
