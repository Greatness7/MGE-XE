#include "ffeshader.h"
#include "support/log.h"

#include <cstring>
bool FixedFunctionShader::encodeNativePplKey(
    const ShaderKey& source,
    DxvkMorrowindPplDrawV1* destination) {
    if (!destination
     || source.indexedSkinning
     || source.activeStages > DXVK_MORROWIND_PPL_MAX_STAGES
     || source.uvSets > 4) {
        return false;
    }

    const uint32_t totalOutputCoords = source.uvSets
        + (source.usesTexgen ? (source.projectiveTexgen ? 2u : 1u) : 0u);
    if (totalOutputCoords > 4u) {
        return false;
    }

    auto isSupportedArg = [](DWORD arg) {
        return arg == D3DTA_DIFFUSE
            || arg == D3DTA_CURRENT
            || arg == D3DTA_TEXTURE;
    };

    auto isSupportedOp = [](DWORD op) {
        switch (op) {
            case D3DTOP_SELECTARG1:
            case D3DTOP_SELECTARG2:
            case D3DTOP_MODULATE:
            case D3DTOP_MODULATE2X:
            case D3DTOP_MODULATE4X:
            case D3DTOP_ADD:
            case D3DTOP_ADDSIGNED:
            case D3DTOP_ADDSIGNED2X:
            case D3DTOP_SUBTRACT:
            case D3DTOP_BLENDDIFFUSEALPHA:
            case D3DTOP_BLENDTEXTUREALPHA:
            case D3DTOP_BUMPENVMAP:
            case D3DTOP_BUMPENVMAPLUMINANCE:
            case D3DTOP_DOTPRODUCT3:
            case D3DTOP_MULTIPLYADD:
                return true;
            default:
                return false;
        }
    };

    memset(destination, 0, sizeof(*destination));
    destination->structSize = sizeof(*destination);
    destination->structVersion = DXVK_MORROWIND_PPL_STRUCT_VERSION;
    destination->uvSetCount = source.uvSets;
    destination->vertexMaterialMode = source.vertexMaterial;
    destination->fogMode = source.fogMode;
    destination->activeStageCount = source.activeStages;
    // lightSlotCount is the exact packed point count, known only after packing.

    if (source.usesSkinning)
        destination->flags |= DXVK_MW_PPL_USE_SKINNING;
    if (source.vertexColour)
        destination->flags |= DXVK_MW_PPL_VERTEX_COLOR;
    if (source.usesBumpmap) {
        destination->flags |= DXVK_MW_PPL_USE_BUMPMAP;
        destination->bumpmapStage = source.bumpmapStage;
    }
    if (source.usesTexgen) {
        destination->flags |= DXVK_MW_PPL_USE_TEXGEN;
        destination->texgenStage = source.texgenStage;
        if (source.projectiveTexgen)
            destination->flags |= DXVK_MW_PPL_PROJECTIVE_TEXGEN;
    }

    for (uint32_t i = 0; i < source.activeStages; ++i) {
        const ShaderKey::Stage& sourceStage = source.stage[i];
        if (!isSupportedOp(sourceStage.colorOp)
         || !isSupportedArg(sourceStage.colorArg1)
         || !isSupportedArg(sourceStage.colorArg2)
         || (sourceStage.colorOp == D3DTOP_MULTIPLYADD
            && !isSupportedArg(sourceStage.colorArg0))
         || sourceStage.texcoordIndex >= 4
         || sourceStage.texcoordGen > (D3DTSS_TCI_SPHEREMAP >> 16)) {
            return false;
        }

        DxvkMorrowindPplStageV1& stage = destination->stages[i];
        stage.colorOp = sourceStage.colorOp;
        stage.colorArg1 = sourceStage.colorArg1;
        stage.colorArg2 = sourceStage.colorArg2;
        stage.colorArg0 = sourceStage.colorOp == D3DTOP_MULTIPLYADD
            ? sourceStage.colorArg0
            : 0u;
        stage.texcoordIndex = sourceStage.texcoordIndex;
        stage.texcoordGen = sourceStage.texcoordGen;
        if (sourceStage.alphaOpMatched)
            stage.flags |= DXVK_MW_STAGE_ALPHA_MATCHES_COLOR;
        if (sourceStage.alphaOpSelect1)
            stage.flags |= DXVK_MW_STAGE_ALPHA_SELECT_ARG1;
    }

    return true;
}

// Completes a packet whose shader-key-derived fields were already accepted by
// encodeNativePplKey, then attempts the native draw.
bool FixedFunctionShader::renderMorrowindNative(
    const RenderedState* rs,
    const PplDrawData& data,
    DxvkMorrowindPplDrawV1& packet) {
    packet.lightSlotCount = data.pointLightCount;

    packet.primitiveType = rs->primType;
    packet.baseVertexIndex = static_cast<int32_t>(rs->baseIndex);
    packet.minVertexIndex = rs->minIndex;
    packet.vertexCount = rs->vertCount;
    packet.startIndex = rs->startIndex;
    packet.primitiveCount = rs->primCount;
    packet.vertexBlendState = rs->vertexBlendState;

    memcpy(packet.projection, &m_pplSceneState.projection, sizeof(packet.projection));
    memcpy(packet.worldView, rs->worldViewTransforms, sizeof(packet.worldView));
    memcpy(packet.texgenTransform, &data.texgenTransform, sizeof(packet.texgenTransform));

    memcpy(packet.materialDiffuse, &data.materialDiffuse, sizeof(packet.materialDiffuse));
    memcpy(packet.materialAmbient, &data.materialAmbient, sizeof(packet.materialAmbient));
    memcpy(packet.materialEmissive, &data.materialEmissive, sizeof(packet.materialEmissive));

    memcpy(packet.sceneAmbient, &data.sceneAmbient, 3 * sizeof(float));
    memcpy(packet.sunDiffuse, &data.sunDiffuse, 3 * sizeof(float));
    memcpy(packet.sunDirection, &data.sunDirection, 3 * sizeof(float));

    memcpy(packet.lightDiffuse, data.lightDiffuse, sizeof(packet.lightDiffuse));
    memcpy(packet.lightAmbient, data.lightAmbient, sizeof(packet.lightAmbient));
    memcpy(packet.lightPosition, data.lightPosition, sizeof(packet.lightPosition));
    memcpy(packet.lightFalloffQuadratic, data.lightFalloffQuadratic, sizeof(packet.lightFalloffQuadratic));
    packet.lightFalloffConstant = data.lightFalloffConstant;

    memcpy(packet.fogColor, &m_pplSceneState.fogColor, 3 * sizeof(float));
    packet.nearFogStart = m_pplSceneState.nearFogStart;
    packet.nearFogRange = m_pplSceneState.nearFogRange;

    memcpy(packet.bumpMatrix, data.bumpMatrix, sizeof(packet.bumpMatrix));
    memcpy(packet.bumpLumiScaleBias, data.bumpLumiScaleBias, sizeof(packet.bumpLumiScaleBias));

    HRESULT hr = m_morrowindInterop->DrawPplV1(&packet);
    if (hr == S_OK) {
        ++m_nativePplDraws;
        return true;
    }

    if (hr == D3DERR_NOTAVAILABLE) {
        ++m_nativePplUnsupported;
        if (!m_loggedNativePplUnsupported) {
            m_loggedNativePplUnsupported = true;
            LOG::logline("-- Native PPL packet rejected unsupported current state; falling back per draw");
        }
    } else {
        ++m_nativePplFailures;
        if (!m_loggedNativePplFailure) {
            m_loggedNativePplFailure = true;
            LOG::logline("!! Native PPL packet failed with hr 0x%08lx; falling back per draw", hr);
        }
    }

    return false;
}
