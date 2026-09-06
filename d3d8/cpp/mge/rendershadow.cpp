
#include "distantland.h"
#include "distantshader.h"
#include "configuration.h"
#include "mwbridge.h"
#include "proxydx/d3d8header.h"
#include "support/log.h"

#include <algorithm>
#include <cmath>



// Cascade half-widths in world units. The last covers the draw distance and must stay the
// outermost: only it carries the edge fade
static float shadowCascadeRadius(int layer) {
    static const float fixedRadius[] = { 1000.0f, 4000.0f, 16000.0f };
    if (layer < 3) {
        return fixedRadius[layer];
    }
    return std::max(Configuration.DL.DrawDist * DistantLand::kCellSize, 1.25f * fixedRadius[2]);
}

// Vertical half-extent of the cylinder a cascade covers around the eye
static float shadowCascadeHeight(float radius) {
    return radius;
}

// Minimum reach of the light frustum toward the sun, so hills a couple of cells out still
// cast into the near cascades
static const float shadowCasterReach = 2.0f * DistantLand::kCellSize;

// Lowest light elevation the fit uses, degrees, azimuth kept. Must match the top of
// shadowElevationFade in XE Mod Shadow Data.fx. Dormant with the vanilla light (13.8 minimum)
static const float shadowMinElevation = 10.0f;

// Half-extent of the eye cylinder (radius, half-height) along a unit axis of the light basis
static float shadowExtentAlong(const D3DXVECTOR3& axis, float radius, float height) {
    return radius * std::sqrt(axis.x * axis.x + axis.y * axis.y) + height * std::fabs(axis.z);
}

// Texel size and light depth range per cascade of each atlas, world units, indexed like smViewproj
struct ShadowFit {
    float texel;
    float depth;
};
static ShadowFit shadowFit[2][DistantLand::kShadowCascades];

// Eye at the start of the current build. A move past shadowRestartDistance is a jump
// (teleport, load): the build restarts and is shown without a fade
static D3DXVECTOR3 shadowBuildEye;
static bool shadowBuildEyeValid = false;
static bool shadowBuildRestarted = false;
static float shadowRestartDistance() {
    return 0.5f * shadowCascadeRadius(0);
}

// Real-time length of the crossfade from the current atlas to the next one
static const float shadowBlendSeconds = 0.25f;



// Both atlases' clip transforms, current in [0, N) and next in [N, 2N), premultiplied by pre
// when given, plus the blend factor and both textures
void DistantLand::uploadShadowMatrices(const D3DXMATRIX* pre) {
    D3DXMATRIX m[2 * kShadowCascades];
    for (int i = 0; i < kShadowCascades; ++i) {
        m[i] = pre ? (*pre) * smViewproj[shadowCurrent][i] : smViewproj[shadowCurrent][i];
        m[kShadowCascades + i] = pre ? (*pre) * smViewproj[shadowBuilding][i] : smViewproj[shadowBuilding][i];
    }
    effect->SetMatrixArray(ehShadowViewproj, m, 2 * kShadowCascades);
    effect->SetFloat(ehShadowBlend, shadowBuildComplete ? shadowBlend : 0.0f);
    effect->SetTexture(ehTex3, texShadow[shadowCurrent]);
    effect->SetTexture(ehTexShadowNext, texShadow[shadowBuilding]);
}

// Builds the next atlas one cascade per frame, cross-fades it in over shadowBlendSeconds,
// swaps. Restores render state on return
void DistantLand::renderShadowMap() {
    const DWORD now = GetTickCount();

    // Eye jump: end any fade now, restart the build here, show it without a fade
    const D3DXVECTOR3 eye(eyePos.x, eyePos.y, eyePos.z);
    const D3DXVECTOR3 moved = eye - shadowBuildEye;
    const bool eyeJumped = shadowBuildEyeValid && D3DXVec3Length(&moved) > shadowRestartDistance();

    if (shadowBuildComplete) {
        shadowBlend = std::min(1.0f, (now - shadowBlendStart) * (0.001f / shadowBlendSeconds));
        if (shadowBlend >= 1.0f || !shadowCurrentValid || shadowBuildRestarted || eyeJumped) {
            shadowCurrent = shadowBuilding;
            shadowBuilding ^= 1;
            shadowCurrentValid = true;
            shadowBuildComplete = false;
            shadowBuildRestarted = false;
            shadowBlend = 0;
        }
    }

    if (!shadowBuildComplete) {
        if (eyeJumped) {
            shadowBuildLayer = 0;
            shadowBuildRestarted = true;
        }

        // Depth-only render into the next atlas, colour writes go to the null target
        RenderTargetSwitcher rtsw(surfShadowColor, surfShadow[shadowBuilding]);
        D3DVIEWPORT9 vp;
        device->GetViewport(&vp);

        // Clear the atlas parameters left by the last receiver pass
        effect->SetTexture(ehTex0, 0);
        effect->SetTexture(ehTex2, 0);
        effect->SetTexture(ehTex3, 0);
        effect->SetTexture(ehTexShadowNext, 0);

        if (shadowBuildLayer == 0) {
            device->Clear(0, 0, D3DCLEAR_ZBUFFER, 0, 1.0, 0);
            shadowBuildEye = eye;
            shadowBuildEyeValid = true;
        }

        renderShadowLayer(shadowBuildLayer, shadowCascadeRadius(shadowBuildLayer));

        if (++shadowBuildLayer == kShadowCascades) {
            shadowBuildLayer = 0;
            shadowBuildComplete = true;
            shadowBlendStart = now;
        }

        // Reset viewport and the caster pass states that the distant land passes do not set
        device->SetViewport(&vp);
        device->SetRenderState(D3DRS_COLORWRITEENABLE, D3DCOLORWRITEENABLE_RED | D3DCOLORWRITEENABLE_GREEN | D3DCOLORWRITEENABLE_BLUE | D3DCOLORWRITEENABLE_ALPHA);
        device->SetRenderState(D3DRS_SLOPESCALEDEPTHBIAS, 0);
        device->SetRenderState(D3DRS_DEPTHBIAS, 0);
    }

    // Per-cascade texel size and depth range for both atlases, laid out like the matrices;
    // unfitted layers get their first fit's values
    D3DXVECTOR4 cascadeParams[2 * kShadowCascades];
    const int atlas[2] = { shadowCurrent, shadowBuilding };
    for (int set = 0; set < 2; ++set) {
        for (int layer = 0; layer < kShadowCascades; ++layer) {
            ShadowFit& fit = shadowFit[atlas[set]][layer];
            if (fit.depth <= 0) {
                const float radius = shadowCascadeRadius(layer);
                fit.texel = 2.0f * radius / Configuration.DL.ShadowResolution;
                fit.depth = std::max(shadowCasterReach, radius) + radius;
            }
            cascadeParams[set * kShadowCascades + layer] = D3DXVECTOR4(fit.texel, fit.depth, 0, 0);
        }
    }
    effect->SetVectorArray(ehShadowCascade, cascadeParams, 2 * kShadowCascades);

    // Distant land and statics receive next; they sample from world space
    uploadShadowMatrices(nullptr);
}

// Draws one cascade's casters into its atlas strip. The whole box, not the camera frustum:
// the atlas is reused while the camera turns, so nothing here may depend on the view direction
void DistantLand::renderShadowLayerGeneric(MWBridge* mwBridge, int layer, D3DXMATRIX* view, D3DXMATRIX* proj, VisibleSet& visible_set) {
    // Clip to atlas region with viewport
    const DWORD res = Configuration.DL.ShadowResolution;
    D3DVIEWPORT9 vp = { layer * res, 0, res, res, 0.0f, 1.0f };
    device->SetViewport(&vp);

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

// Statics cast only this far from the eye, world units. 0 in the config means the whole
// draw distance, which the last cascade already covers
static float shadowStaticRadius() {
    const float cells = Configuration.ShadowStaticRange;
    return cells > 0 ? cells * DistantLand::kCellSize : shadowCascadeRadius(DistantLand::kShadowCascades - 1);
}

// Ortho light box for the given basis: the eye cylinder of this radius fits at any sun
// angle, and the camera sits at least shadowCasterReach toward the sun
static void fitLightBox(const D3DXVECTOR3& lookAt, const D3DXVECTOR3& lightDir, const D3DXVECTOR3& up, float radius,
                        D3DXMATRIX* view, D3DXMATRIX* proj, D3DXMATRIX* viewproj, ShadowFit* fit) {
    // Orientation first, then the box from its axes
    const D3DXVECTOR3 towardSun = lookAt - lightDir;
    D3DXMatrixLookAtRH(view, &towardSun, &lookAt, &up);
    const D3DXVECTOR3 axisX(view->_11, view->_21, view->_31);
    const D3DXVECTOR3 axisY(view->_12, view->_22, view->_32);
    const D3DXVECTOR3 axisZ(view->_13, view->_23, view->_33);
    const float height = shadowCascadeHeight(radius);
    const float halfX = shadowExtentAlong(axisX, radius, height);
    const float halfY = shadowExtentAlong(axisY, radius, height);
    const float halfZ = shadowExtentAlong(axisZ, radius, height);

    const float zSun = std::max(shadowCasterReach, halfZ);
    const D3DXVECTOR3 cameraPos = lookAt - lightDir * zSun;
    D3DXMatrixLookAtRH(view, &cameraPos, &lookAt, &up);
    D3DXMatrixOrthoRH(proj, 2 * halfX, 2 * halfY, 0, zSun + halfZ);
    *viewproj = (*view) * (*proj);

    fit->texel = 2 * halfX / Configuration.DL.ShadowResolution;
    fit->depth = zSun + halfZ;
}

void DistantLand::renderShadowLayer(int layer, float radius) {
    auto mwBridge = MWBridge::get();
    D3DXMATRIX* view = &smView[layer], *proj = &smProj[layer], *viewproj = &smViewproj[shadowBuilding][layer];

    // The sky light, not the sun disc: what the lambert terms and the engine's stencil
    // shadows use (docs/architecture/shadows.md)
    const D3DXVECTOR4& lightVec = sunVec;

    // Eye-centred, so the atlas stays valid while the camera turns
    const D3DXVECTOR3 lookAt(eyePos.x, eyePos.y, eyePos.z);

    // Up is world Z, or world Y within 26 degrees of the zenith, where Z would let the basis
    // spin with the sun
    D3DXVECTOR3 lightDir(lightVec.x, lightVec.y, lightVec.z);
    const float minSin = std::sin(shadowMinElevation * D3DX_PI / 180.0f);
    if (-lightDir.z < minSin) {
        const float horizontal = std::sqrt(lightDir.x * lightDir.x + lightDir.y * lightDir.y);
        if (horizontal > 1.0e-4f) {
            const float scale = std::cos(shadowMinElevation * D3DX_PI / 180.0f) / horizontal;
            lightDir.x *= scale;
            lightDir.y *= scale;
            lightDir.z = -minSin;
        }
    }
    const D3DXVECTOR3 up = (std::fabs(lightDir.z) < 0.9f) ? D3DXVECTOR3(0, 0, 1) : D3DXVECTOR3(0, 1, 0);

    fitLightBox(lookAt, lightDir, up, radius, view, proj, viewproj, &shadowFit[shadowBuilding][layer]);

    // Snap the translation row (the world origin in clip space) to whole texels, locking the
    // grid to the world
    const double quantizer = 2.0 / Configuration.DL.ShadowResolution;
    viewproj->_41 = float(quantizer * std::floor(viewproj->_41 / quantizer));
    viewproj->_42 = float(quantizer * std::floor(viewproj->_42 / quantizer));

    // Caster passes read shadowViewProj[0]; upload this cascade before it draws
    effect->SetMatrixArray(ehShadowViewproj, viewproj, 1);
    effectShadow->CommitChanges();

    visExtraShared.RemoveAll();
    if (staticsUploaded) {
        // Statics are culled with a light box of the static range when that is smaller than
        // the cascade; terrain always casts over the whole cascade
        D3DXMATRIX cullViewproj = *viewproj;
        const float staticRadius = shadowStaticRadius();
        if (staticRadius < radius) {
            D3DXMATRIX cullView, cullProj;
            ShadowFit cullFit;
            fitLightBox(lookAt, lightDir, up, staticRadius, &cullView, &cullProj, &cullViewproj, &cullFit);
        }
        ViewFrustum range_frustum(&cullViewproj);

        // because shadow meshes don't need to be sorted, we can read and write in parallel
        ipcClient.getVisibleMeshesCoarse(visExtraSharedId, range_frustum, VIS_STATIC);
    }

    renderShadowLayerGeneric(mwBridge, layer, view, proj, visExtraShared);
}

// renderShadow - Renders shadows (using blending) over Morrowind shadow receivers
void DistantLand::renderShadow() {
    if (!shadowCurrentValid) {
        return;
    }

    // Supply view space -> shadow clip space matrices and both atlases
    D3DXMATRIX inverseView;
    D3DXMatrixInverse(&inverseView, NULL, &mwView);
    uploadShadowMatrices(&inverseView);

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
    D3DXMATRIX inverseShadowViewProj, cameraViewProj, shadowToCameraProj[kShadowCascades];

    D3DXMatrixMultiply(&cameraViewProj, &mwView, &mwProj);
    for (int i = 0; i < kShadowCascades; ++i) {
        D3DXMatrixInverse(&inverseShadowViewProj, NULL, &smViewproj[shadowCurrent][i]);
        D3DXMatrixMultiply(&shadowToCameraProj[i], &inverseShadowViewProj, &cameraViewProj);
    }

    // Display shadow layers in top right corner
    effect->Begin(&passes, D3DXFX_DONOTSAVESTATE);
    effect->BeginPass(PASS_DEBUGSHADOW);
    device->SetRenderState(D3DRS_CULLMODE, D3DCULL_CW);
    effect->SetTexture(ehTex3, texShadow[shadowCurrent]);
    effect->SetMatrixArray(ehVertexBlendPalette, shadowToCameraProj, kShadowCascades);
    effect->CommitChanges();
    device->SetVertexDeclaration(WaterDecl);
    device->SetStreamSource(0, vbFullFrame, 0, 12);
    device->DrawPrimitive(D3DPT_TRIANGLESTRIP, 0, 2);
    effect->EndPass();
    effect->End();
}
