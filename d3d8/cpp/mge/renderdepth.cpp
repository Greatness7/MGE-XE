
#include "configuration.h"
#include "distantland.h"
#include "distantshader.h"
#include "mwbridge.h"
#include "dxvk_morrowind_interop.h"
#include "proxydx/d3d8header.h"
#include "support/log.h"



HRESULT DistantLand::captureNativeDepthIntz(NativeDepthMode mode, const D3DXMATRIX& projection) {
    if (nativeDepthBackend == NativeDepthBackend::None || !texDepthStencil || !surfDepthStencil
        || !texDepthFrame || !surfDepthDepth || !effectDepth) {
        return E_FAIL;
    }

    IDirect3DSurface9* target = nullptr;
    IDirect3DSurface9* savedTarget = nullptr;
    IDirect3DSurface9* savedDepthStencil = nullptr;
    HRESULT hr = texDepthFrame->GetSurfaceLevel(0, &target);
    bool passBegun = false;
    bool drawSucceeded = false;

    if (FAILED(hr)) {
        goto cleanup;
    }
    hr = device->GetRenderTarget(0, &savedTarget);
    if (FAILED(hr)) {
        goto cleanup;
    }
    hr = device->GetDepthStencilSurface(&savedDepthStencil);
    if (FAILED(hr)) {
        goto cleanup;
    }
    hr = device->SetRenderTarget(0, target);
    if (FAILED(hr)) {
        goto cleanup;
    }
    hr = device->SetDepthStencilSurface(surfDepthDepth);
    if (FAILED(hr)) {
        goto cleanup;
    }

    if (mode == NativeDepthMode::Replace) {
        hr = device->Clear(0, nullptr, D3DCLEAR_ZBUFFER, 0, 1.0f, 0);
        if (FAILED(hr)) {
            goto cleanup;
        }
    }

    hr = effectDepth->SetTexture(ehDepthSrc, texDepthStencil);
    if (FAILED(hr)) {
        goto cleanup;
    }
    hr = effectDepth->SetFloat(ehSourceM33, projection._33);
    if (FAILED(hr)) {
        goto cleanup;
    }
    hr = effectDepth->SetFloat(ehSourceM43, projection._43);
    if (FAILED(hr)) {
        goto cleanup;
    }

    hr = effectDepth->BeginPass(
        mode == NativeDepthMode::Replace
            ? PASS_NATIVEDEPTH_REPLACE
            : PASS_NATIVEDEPTH_MERGE
    );
    if (FAILED(hr)) {
        goto cleanup;
    }
    passBegun = true;

    hr = device->SetVertexDeclaration(WaterDecl);
    if (FAILED(hr)) {
        goto cleanup;
    }
    hr = device->SetStreamSource(0, vbFullFrame, 0, 12);
    if (FAILED(hr)) {
        goto cleanup;
    }
    hr = device->DrawPrimitive(D3DPT_TRIANGLESTRIP, 0, 2);
    drawSucceeded = SUCCEEDED(hr);

cleanup:
    // The INTZ texture must no longer be sampled before it is restored as the DSV.
    effectDepth->SetTexture(ehDepthSrc, nullptr);
    if (passBegun) {
        effectDepth->CommitChanges();
        effectDepth->EndPass();
    }

    if (savedTarget) {
        device->SetRenderTarget(0, savedTarget);
    }
    if (savedDepthStencil) {
        device->SetDepthStencilSurface(savedDepthStencil);
    }

    if (savedDepthStencil) {
        savedDepthStencil->Release();
    }
    if (savedTarget) {
        savedTarget->Release();
    }
    if (target) {
        target->Release();
    }

    return drawSucceeded ? D3D_OK : (FAILED(hr) ? hr : E_FAIL);
}

HRESULT DistantLand::resolveNativeDepthMsaa() {
    if (nativeDepthBackend != NativeDepthBackend::DxvkMsaaResolve
        || !dxvkMorrowindInterop
        || !surfAutoDepthStencil
        || !surfDepthStencil) {
        return E_FAIL;
    }

    IDirect3DSurface9* activeDepthStencil = nullptr;
    HRESULT hr = device->GetDepthStencilSurface(&activeDepthStencil);
    if (SUCCEEDED(hr)) {
        hr = activeDepthStencil == surfAutoDepthStencil
            ? dxvkMorrowindInterop->ResolveDepthMinV1(activeDepthStencil, surfDepthStencil)
            : D3DERR_INVALIDCALL;
        activeDepthStencil->Release();
    }

    if (hr == D3DERR_NOTAVAILABLE) {
        LOG::logline("!! DXVK MSAA native depth resolve became unavailable; using geometry replay until renderer restart");
        nativeDepthBackend = NativeDepthBackend::None;
        dxvkMorrowindInterop->Release();
        dxvkMorrowindInterop = nullptr;
    }

    return hr;
}

HRESULT DistantLand::captureNativeDepth(NativeDepthMode mode, const D3DXMATRIX& projection) {
    switch (nativeDepthBackend) {
        case NativeDepthBackend::IntzMainDsv:
            return captureNativeDepthIntz(mode, projection);

        case NativeDepthBackend::DxvkMsaaResolve: {
            const HRESULT hr = resolveNativeDepthMsaa();
            if (FAILED(hr)) {
                return hr;
            }

            // Keep this call adjacent to the private resolve. The render-target
            // switch inside the Phase A conversion ends any inline resolve
            // before INTZ is sampled.
            return captureNativeDepthIntz(mode, projection);
        }

        default:
            return E_FAIL;
    }
}

void DistantLand::renderDepth() {
    auto mwBridge = MWBridge::get();

    // Switch to render target
    RenderTargetSwitcher rtsw(texDepthFrame, surfDepthDepth);
    device->Clear(0, 0, D3DCLEAR_ZBUFFER, 0, 1.0, 0);

    // Unbind depth sampler
    effect->SetTexture(ehTex3, NULL);

    // Projection should cover whole scene
    D3DXMATRIX distProj = mwProj;
    editProjectionZ(&distProj, 4.0f, Configuration.DL.DrawDist * kCellSize);
    effect->SetMatrix(ehProj, &distProj);

    // Clear floating point buffer to far depth
    effectDepth->BeginPass(PASS_CLEARDEPTH);
    device->SetVertexDeclaration(WaterDecl);
    device->SetStreamSource(0, vbFullFrame, 0, 12);
    device->DrawPrimitive(D3DPT_TRIANGLESTRIP, 0, 2);
    effectDepth->EndPass();

    // Recorded draw calls
    renderDepthRecorded();

    if (isDistantCell()) {
        if (!mwBridge->IsUnderwater(eyePos.z)) {
            // Distant land
            if (mwBridge->IsExterior()) {
                effectDepth->BeginPass(PASS_RENDERLANDDEPTH);
                depthReplayDips += visLandShared.Size();
                renderDistantLandZ();
                effectDepth->EndPass();
            }

            if (staticsUploaded) {
                // Distant statics
                effectDepth->BeginPass(PASS_RENDERSTATICSDEPTH);
                device->SetVertexDeclaration(StaticDecl);
                depthReplayDips += visDistantShared.Size();
                visDistantShared.Render(device, effectDepth, effect, &ehTex0, &ehHasAlpha, &ehHasVCol, &ehWorld, SIZEOFSTATICVERT);
                effectDepth->EndPass();
            }
        }

        if (staticsUploaded && (Configuration.MGEFlags & USE_GRASS)) {
            // Grass
            effectDepth->BeginPass(PASS_RENDERGRASSDEPTHINST);
            renderGrassInstZ();
            effectDepth->EndPass();
        }
    }

    // Reset projection matrix
    effect->SetMatrix(ehProj, &mwProj);
}

void DistantLand::renderDepthAdditional() {
    // Switch to render target
    RenderTargetSwitcher rtsw(texDepthFrame, surfDepthDepth);

    // Unbind depth sampler
    effect->SetTexture(ehTex3, NULL);

    // Projection should cover whole scene
    D3DXMATRIX distProj = mwProj;
    editProjectionZ(&distProj, 4.0f, Configuration.DL.DrawDist * kCellSize);
    effect->SetMatrix(ehProj, &distProj);

    // Recorded draw calls
    renderDepthRecorded();

    // Reset projection matrix
    effect->SetMatrix(ehProj, &mwProj);
}

void DistantLand::renderDepthRecorded() {
    // Use an alpha threshold for solidity that isn't precisely equal to a commonly used value (such as 0.5).
    // Vertex interpolators can be slightly inaccurate and cause a value that should be constant across a triangle
    // to have interpolated fragment values that vary either side of the threshold and cause noise.
    const float solidThreshold = 0.499f;

    // Recorded renders
    const auto& recordMW_const = recordMW;
    for (const auto& i : recordMW_const) {
        // Set variables in main effect; variables are shared via effect pool

        // Fragment colour routing
        bool alphaDependent = i.alphaTest || i.blendEnable;
        effect->SetBool(ehHasVCol, alphaDependent && (i.fvf & D3DFVF_DIFFUSE) != 0);
        effect->SetFloat(ehMaterialAlpha, alphaDependent ? i.diffuseMaterial.a : 1.0f);

        // Only bind texture for alphas
        if (alphaDependent && i.texture) {
            effect->SetTexture(ehTex0, i.texture);
            effect->SetBool(ehHasAlpha, true);
            effect->SetFloat(ehAlphaRef, i.alphaTest ? (i.alphaRef / 255.0f) : solidThreshold);
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

        effectDepth->BeginPass(indexedSkinning ? PASS_RENDERMWDEPTH_INDEXED : PASS_RENDERMWDEPTH);
        effectDepth->CommitChanges();

        device->SetRenderState(D3DRS_CULLMODE, i.cullMode);
        device->SetStreamSource(0, i.vb, i.vbOffset, i.vbStride);
        device->SetIndices(i.ib);
        device->SetFVF(i.fvf);
        device->DrawIndexedPrimitive(i.primType, i.baseIndex, i.minIndex, i.vertCount, i.startIndex, i.primCount);
        ++depthReplayDips;

        effectDepth->EndPass();
    }
}
