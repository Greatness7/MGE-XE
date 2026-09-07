
#include "distantland.h"
#include "distantshader.h"
#include "configuration.h"
#include "mwbridge.h"
#include "proxydx/d3d8header.h"
#include "support/log.h"

#include <algorithm>
#include <cmath>



static const float shadowNearRadius = 1000.0;
static const float shadowFarRadius = 4000.0;

// Stencil mask dilation, must exceed the blur kernel reach of ~3 texels
static const int shadowStencilMarginTexels = 8;

namespace {

float cross2(const D3DXVECTOR2& a, const D3DXVECTOR2& b, const D3DXVECTOR2& c) {
    return (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
}

// Andrew's monotone chain. Sorts pts, writes the counter-clockwise hull (capacity 2 * n)
// and returns its vertex count.
size_t convexHull(D3DXVECTOR2* pts, size_t n, D3DXVECTOR2* hull) {
    std::sort(pts, pts + n, [](const D3DXVECTOR2& a, const D3DXVECTOR2& b) {
        return a.x < b.x || (a.x == b.x && a.y < b.y);
    });

    size_t k = 0;
    for (size_t i = 0; i < n; ++i) {
        while (k >= 2 && cross2(hull[k - 2], hull[k - 1], pts[i]) <= 0) {
            --k;
        }
        hull[k++] = pts[i];
    }
    for (size_t i = n - 1, lower = k + 1; i > 0; --i) {
        while (k >= lower && cross2(hull[k - 2], hull[k - 1], pts[i - 1]) <= 0) {
            --k;
        }
        hull[k++] = pts[i - 1];
    }
    return k - 1;   // Last point repeats the first
}

// The camera frustum's silhouette in light clip space, clipped to the light far plane and
// dilated by the margin square, so at least the margin outward in every direction. A
// Minkowski sum, as a union of translated copies leaves sharp silhouette vertices with no
// margin at all. Returns the fan vertex count, 0 if the transform is degenerate. Every
// value is checked finite before it can reach std::sort, whose comparator is undefined
// on NaN.
size_t buildStencilHull(const D3DXMATRIX& clipToLight, float margin, D3DXVECTOR3* fan) {
    // Frustum corners, camera clip space to light post-projective space
    D3DXVECTOR3 corner[8];
    for (int i = 0; i < 8; ++i) {
        D3DXVECTOR4 p((i & 1) ? 1.0f : -1.0f, (i & 2) ? 1.0f : -1.0f, (i & 4) ? 1.0f : 0.0f, 1.0f);
        D3DXVec4Transform(&p, &p, &clipToLight);
        if (!std::isfinite(p.w) || p.w <= 0.0f) {
            return 0;
        }
        corner[i] = D3DXVECTOR3(p.x / p.w, p.y / p.w, p.z / p.w);
        if (!std::isfinite(corner[i].x) || !std::isfinite(corner[i].y) || !std::isfinite(corner[i].z)) {
            return 0;
        }
    }

    // Clip to the light far plane. The vertex shader clamps the near side instead of
    // clipping it, so only z > 1 is cut. Corners in range plus crossing points of the
    // 12 edges, then four margin square offsets of each.
    D3DXVECTOR2 pts[80], hull[160];
    size_t n = 0;
    for (int i = 0; i < 8; ++i) {
        if (corner[i].z <= 1.0f) {
            pts[n++] = D3DXVECTOR2(corner[i].x, corner[i].y);
        }
    }
    for (int i = 0; i < 8; ++i) {
        for (int bit = 1; bit <= 4; bit <<= 1) {
            if (i & bit) {
                continue;
            }
            const D3DXVECTOR3& a = corner[i], &b = corner[i | bit];
            if ((a.z <= 1.0f) == (b.z <= 1.0f)) {
                continue;
            }
            const float t = (1.0f - a.z) / (b.z - a.z);
            pts[n++] = D3DXVECTOR2(a.x + t * (b.x - a.x), a.y + t * (b.y - a.y));
        }
    }
    for (size_t i = 0, base = n; i < base; ++i) {
        const D3DXVECTOR2 p = pts[i];
        pts[i] = D3DXVECTOR2(p.x - margin, p.y - margin);
        pts[n++] = D3DXVECTOR2(p.x - margin, p.y + margin);
        pts[n++] = D3DXVECTOR2(p.x + margin, p.y - margin);
        pts[n++] = D3DXVECTOR2(p.x + margin, p.y + margin);
    }
    if (n < 3) {
        return 0;
    }
    for (size_t i = 0; i < n; ++i) {
        if (!std::isfinite(pts[i].x) || !std::isfinite(pts[i].y)) {
            return 0;
        }
    }

    const size_t count = convexHull(pts, n, hull);
    for (size_t i = 0; i < count; ++i) {
        fan[i] = D3DXVECTOR3(hull[i].x, hull[i].y, 0.5f);
    }
    return count;
}

}



// Renders multiple shadow map layers to channels in one texture with soft edges.
// Restores render state on return.
void DistantLand::renderShadowMap() {
    IDirect3DSurface9* target, *targetSoft;
    texShadow->GetSurfaceLevel(0, &target);
    texSoftShadow->GetSurfaceLevel(0, &targetSoft);

    // Switch to render target
    RenderTargetSwitcher rtsw(targetSoft, surfShadowZ);
    D3DVIEWPORT9 vp;
    device->GetViewport(&vp);

    // Unbind shadow samplers
    effect->SetTexture(ehTex0, 0);
    effect->SetTexture(ehTex2, 0);

    // Clear floating point buffer to far depth
    device->Clear(0, 0, D3DCLEAR_ZBUFFER|D3DCLEAR_STENCIL, 0, 1.0, 0);
    effectShadow->BeginPass(PASS_CLEARSHADOWMAP);
    effectShadow->CommitChanges();
    device->SetVertexDeclaration(WaterDecl);
    device->SetStreamSource(0, vbFullFrame, 0, 12);
    device->DrawPrimitive(D3DPT_TRIANGLESTRIP, 0, 2);
    effectShadow->EndPass();

    // Calculate transform to map view frustum into world space
    // Null when the camera projection is singular, which masks the whole cascade instead
    D3DXMATRIX inverseCameraProj, cameraViewProj;
    D3DXMatrixMultiply(&cameraViewProj, &mwView, &mwProj);
    const D3DXMATRIX* frustumToWorld = D3DXMatrixInverse(&inverseCameraProj, NULL, &cameraViewProj) ? &inverseCameraProj : nullptr;

    // Render near layer (changes viewport)
    renderShadowLayer(0, shadowNearRadius, frustumToWorld);

    // Render far layer (changes viewport)
    renderShadowLayer(1, shadowFarRadius, frustumToWorld);

    // Reset viewport
    device->SetViewport(&vp);

    // Soften shadow map, separable blur through texShadow and back
    device->SetVertexDeclaration(WaterDecl);
    device->SetStreamSource(0, vbFullFrame, 0, 12);

    device->SetRenderTarget(0, target);
    effectShadow->BeginPass(PASS_SOFTENSHADOWMAP_H);
    effect->SetTexture(ehTex3, texSoftShadow);
    effectShadow->CommitChanges();
    device->DrawPrimitive(D3DPT_TRIANGLESTRIP, 0, 2);
    effectShadow->EndPass();

    device->SetRenderTarget(0, targetSoft);
    effectShadow->BeginPass(PASS_SOFTENSHADOWMAP_V);
    effect->SetTexture(ehTex3, texShadow);
    effectShadow->CommitChanges();
    device->DrawPrimitive(D3DPT_TRIANGLESTRIP, 0, 2);
    effectShadow->EndPass();

    // Clean up surface pointers
    target->Release();
    targetSoft->Release();
}

void DistantLand::renderShadowLayerGeneric(MWBridge* mwBridge, int layer, const D3DXMATRIX* inverseCameraProj, const D3DXMATRIX* viewproj, D3DXMATRIX* view, D3DXMATRIX* proj, VisibleSet& visible_set) {
    // Clip to atlas region with viewport
    const DWORD res = Configuration.DL.ShadowResolution;
    D3DVIEWPORT9 vp = { layer * res, 0, res, res, 0.0f, 1.0f };
    device->SetViewport(&vp);

    // Render view frustum to stencil, which limits rendering to visible texels
    // Dilated so receivers at the frustum edge do not blur into the cleared atlas.
    // The hull is already in light clip space, so both transforms are identity here.
    // A missing inverse or degenerate hull falls back to masking the whole cascade, which
    // is only slower.
    D3DXVECTOR3 fan[80];
    size_t fanCount = 0;
    if (inverseCameraProj) {
        const D3DXMATRIX clipToLight = (*inverseCameraProj) * (*viewproj);
        fanCount = buildStencilHull(clipToLight, shadowStencilMarginTexels * 2.0f / res, fan);
    }

    D3DXMATRIX identity;
    D3DXMatrixIdentity(&identity);
    effect->SetMatrix(ehWorld, &identity);
    effect->SetMatrixArray(ehShadowViewproj, &identity, 1);
    effectShadow->BeginPass(PASS_SHADOWSTENCIL);
    device->SetVertexDeclaration(WaterDecl);
    if (fanCount >= 3) {
        device->DrawPrimitiveUP(D3DPT_TRIANGLEFAN, fanCount - 2, fan, 12);
    } else {
        device->SetStreamSource(0, vbFullFrame, 0, 12);
        device->DrawPrimitive(D3DPT_TRIANGLESTRIP, 0, 2);
    }
    effectShadow->EndPass();

    // Restore cascade transform for casters
    effect->SetMatrixArray(ehShadowViewproj, viewproj, 1);

    // Render land
    effectShadow->BeginPass(PASS_RENDERSHADOWMAP);

    if (mwBridge->IsExterior()) {
        renderDistantLand(effectShadow, view, proj);
    }

    effectShadow->EndPass();

    if (staticsUploaded) {
        // Render statics with their texture-atlas UV bounds
        effectShadow->BeginPass(PASS_RENDERSTATICSHADOWMAP);
        device->SetVertexDeclaration(StaticDecl);
        visible_set.Render(device, effectShadow, effect, &ehTex0, &ehHasAlpha, &ehHasVCol, &ehWorld, &ehUvBoundPalette, SIZEOFSTATICVERT, true);
        effectShadow->EndPass();
    }
}

void DistantLand::renderShadowLayer(int layer, float radius, const D3DXMATRIX* inverseCameraProj) {
    auto mwBridge = MWBridge::get();
    D3DXVECTOR3 lookAt, lookAtEye, shadowCameraPos, up(0, 0, 1);
    D3DXMATRIX* view = &smView[layer], *proj = &smProj[layer], *viewproj = &smViewproj[layer];

    // Select light vector, sunPos during daytime, sunVec during night
    D3DXVECTOR4 lightVec = (sunPos.z > 0) ? -sunPos : sunVec;

    // Centre of projection is one radius ahead of the player
    // Not as far in z direction as player is likely looking at the ground plane rather than below
    // This will be split into a non-texel-quantized but temporally stable view position part,
    // and a texel-quantized view rotation part with small magnitude
    lookAt.x = eyePos.x + radius * eyeVec.x;
    lookAt.y = eyePos.y + radius * eyeVec.y;
    lookAt.z = eyePos.z + 0.5f * radius * eyeVec.z;

    // Quantize eye position to partially reduce texture swimming during camera movement
    lookAtEye.x = float(16.0 * std::floor(0.0625 * eyePos.x));
    lookAtEye.y = float(16.0 * std::floor(0.0625 * eyePos.y));
    lookAtEye.z = float(16.0 * std::floor(0.0625 * eyePos.z));

    // Create shadow frustum centred on lookAtEye, looking along lightVec
    const float zrange = kCellSize;
    shadowCameraPos.x = lookAtEye.x - zrange * lightVec.x;
    shadowCameraPos.y = lookAtEye.y - zrange * lightVec.y;
    shadowCameraPos.z = lookAtEye.z - zrange * lightVec.z;

    D3DXMatrixLookAtRH(view, &shadowCameraPos, &lookAtEye, &up);
    D3DXMatrixOrthoRH(proj, 2 * radius, (1 + std::fabs(lightVec.z)) * radius, 0, 2.0 * zrange);
    *viewproj = (*view) * (*proj);

    // Transform remainder into shadow clip space and quantize
    // Prevents all shimmer during camera rotation
    D3DXVECTOR3 dv, deltaLookAt = lookAtEye - lookAt;
    D3DXVec3TransformNormal(&dv, &deltaLookAt, viewproj);

    // Quantize clip space range [-1, +1] over ShadowResolution texels
    const float quantizer = 2.0f / Configuration.DL.ShadowResolution;
    viewproj->_41 += quantizer * floor(dv.x / quantizer);
    viewproj->_42 += quantizer * floor(dv.y / quantizer);
    viewproj->_43 += dv.z;

    // Cull
    ViewFrustum range_frustum(viewproj);

    visExtraShared.RemoveAll();
    if (staticsUploaded) {
        // because shadow meshes don't need to be sorted, we can read and write in parallel
        ipcClient.getVisibleMeshesCoarse(visExtraSharedId, range_frustum, VIS_STATIC);
    }

    renderShadowLayerGeneric(mwBridge, layer, inverseCameraProj, viewproj, view, proj, visExtraShared);
}

// renderShadow - Renders shadows (using blending) over Morrowind shadow receivers
void DistantLand::renderShadow() {
    // Supply view space -> shadow clip space matrix
    D3DXMATRIX inverseView, viewToShadow[2];
    D3DXMatrixInverse(&inverseView, NULL, &mwView);
    viewToShadow[0] = inverseView * smViewproj[0];
    viewToShadow[1] = inverseView * smViewproj[1];
    effect->SetMatrixArray(ehShadowViewproj, viewToShadow, 2);

    // Bind filtered ESM
    effect->SetTexture(ehTex3, texSoftShadow);

    // Use an alpha threshold for solidity that isn't precisely equal to a commonly used value (such as 0.5).
    // Vertex interpolators can be slightly inaccurate and cause a value that should be constant across a triangle
    // to have interpolated fragment values that vary either side of the threshold and cause noise.
    const float alphaThreshold = 0.0101f;

    // Draw shadows over recorded renders
    const auto& recordMW_const = recordMW;
    for (const auto& i : recordMW_const) {
        // Additive alphas do not receive shadows
        if (i.blendEnable && i.destBlend == D3DBLEND_ONE) {
            continue;
        }

        // Fragment colour routing
        bool alphaDependent = i.alphaTest || i.blendEnable;
        effect->SetBool(ehHasVCol, alphaDependent && (i.fvf & D3DFVF_DIFFUSE) != 0);
        effect->SetFloat(ehMaterialAlpha, alphaDependent ? i.diffuseMaterial.a : 1.0f);

        // Only bind texture for alphas
        if (alphaDependent && i.texture) {
            effect->SetTexture(ehTex0, i.texture);
            effect->SetBool(ehHasAlpha, true);
            effect->SetFloat(ehAlphaRef, i.alphaTest ? (i.alphaRef / 255.0f) : alphaThreshold);
        } else {
            effect->SetTexture(ehTex0, 0);
            effect->SetBool(ehHasAlpha, false);
            effect->SetFloat(ehAlphaRef, -1.0f);
        }

        // Skin using worldview matrices for numerical accuracy
        const bool indexedSkinning = i.skinPaletteCount != 0;
        effect->SetBool(ehHasBones, i.vertexBlendState != 0);
        effect->SetInt(ehVertexBlendState, i.vertexBlendState);
        if (indexedSkinning) {
            effect->SetMatrixArray(
                ehVertexBlendPalette,
                recordedSkinPalettes.data() + i.skinPaletteOffset,
                i.skinPaletteCount
            );
        } else {
            effect->SetMatrixArray(ehVertexBlendPalette, i.worldViewTransforms, 4);
        }

        UINT pass;
        if (indexedSkinning) {
            pass = isPPLActive ? PASS_RENDERSHADOWFFE_INDEXED : PASS_RENDERSHADOW_INDEXED;
        } else {
            pass = isPPLActive ? PASS_RENDERSHADOWFFE : PASS_RENDERSHADOW;
        }
        effect->BeginPass(pass);
        effect->CommitChanges();

        // Ignore two-sided poly (cull none) mode, shadow casters are drawn with CW culling only,
        // which causes false shadows when cast on the reverse side (wrt normals) of a two-sided poly
        DWORD cull = (i.cullMode != D3DCULL_NONE) ? i.cullMode : (DWORD)D3DCULL_CW;
        device->SetRenderState(D3DRS_CULLMODE, cull);
        device->SetStreamSource(0, i.vb, i.vbOffset, i.vbStride);
        device->SetIndices(i.ib);
        device->SetFVF(i.fvf);
        device->DrawIndexedPrimitive(i.primType, i.baseIndex, i.minIndex, i.vertCount, i.startIndex, i.primCount);

        effect->EndPass();
    }
}

// renderShadowDebug - display shadow layers
void DistantLand::renderShadowDebug() {
    UINT passes;

    // Create shadow clip space -> camera clip space matrices
    D3DXMATRIX inverseShadowViewProj, cameraViewProj, shadowToCameraProj[2];

    D3DXMatrixMultiply(&cameraViewProj, &mwView, &mwProj);
    D3DXMatrixInverse(&inverseShadowViewProj, NULL, &smViewproj[0]);
    D3DXMatrixMultiply(&shadowToCameraProj[0], &inverseShadowViewProj, &cameraViewProj);
    D3DXMatrixInverse(&inverseShadowViewProj, NULL, &smViewproj[1]);
    D3DXMatrixMultiply(&shadowToCameraProj[1], &inverseShadowViewProj, &cameraViewProj);

    // Display shadow layers in top right corner
    effect->Begin(&passes, D3DXFX_DONOTSAVESTATE);
    effect->BeginPass(PASS_DEBUGSHADOW);
    device->SetRenderState(D3DRS_CULLMODE, D3DCULL_CW);
    effect->SetTexture(ehTex3, texSoftShadow);
    effect->SetMatrixArray(ehVertexBlendPalette, shadowToCameraProj, 2);
    effect->CommitChanges();
    device->SetVertexDeclaration(WaterDecl);
    device->SetStreamSource(0, vbFullFrame, 0, 12);
    device->DrawPrimitive(D3DPT_TRIANGLESTRIP, 0, 2);
    effect->EndPass();
    effect->End();
}
