
#include "ffeshader.h"
#include "camerarelative.h"
#include "configuration.h"
#include "support/log.h"

#include <algorithm>
#include <sstream>
#include <thread>
#include <chrono>
#include <atomic>

using std::string;
using std::stringstream;
using std::unordered_map;

// D3DX hoists uniform-only expressions out of a shader into a "preshader", a bytecode
// program it re-evaluates on the CPU in an interpreter (D3DXShader::FXLExecutePreSortVM)
// every time a dependent effect parameter is dirty. Because the FFE path sets transforms,
// material and light parameters on every draw, that interpreter runs per draw; a VTune
// capture in a dense exterior showed it as the single hottest function in the process
// (5.5% of all CPU time, with the surrounding D3DX effect machinery at 11%).
// D3DXSHADER_NO_PRESHADER folds the math back into the shader, where it is a handful of
// extra instructions on constant registers.
static const DWORD MGE_FFE_COMPILE_FLAGS = D3DXSHADER_OPTIMIZATION_LEVEL3 | D3DXSHADER_NO_PRESHADER;

DecodedPointLight decodeMorrowindPointLight(
    const D3DCOLORVALUE& diffuse,
    const D3DVECTOR& falloff,
    float& sharedFalloffConstant) {
    DecodedPointLight decoded = {};
    decoded.diffuse = diffuse;

    if (falloff.x > 0) {
        sharedFalloffConstant = falloff.x;
        decoded.attenuation.y = falloff.y;
        decoded.attenuation.z = falloff.z;
    } else if (falloff.z > 0) {
        decoded.diffuse.r *= sharedFalloffConstant;
        decoded.diffuse.g *= sharedFalloffConstant;
        decoded.diffuse.b *= sharedFalloffConstant;
        decoded.ambient = 1.0f + 1e-4f / sqrt(falloff.z);
        decoded.attenuation.z = sharedFalloffConstant * falloff.z;
    } else if (falloff.y == 0.10000001f) {
        decoded.attenuation.z = 5e-5f;
    } else if (falloff.y > 0) {
        float brightness = 0.25f + 1e-4f / falloff.y;
        decoded.diffuse.r = brightness;
        decoded.diffuse.g = brightness;
        decoded.diffuse.b = brightness;
        decoded.ambient = 1.0f;
        decoded.attenuation.z = 0.5555f * falloff.y * falloff.y;
        decoded.viewspaceZBias = 25.0f;
    }

    decoded.attenuation.x = sharedFalloffConstant;
    return decoded;
}

IDirect3DDevice* FixedFunctionShader::device;
ID3DXEffectPool* FixedFunctionShader::constantPool;
unordered_map<FixedFunctionShader::ShaderKey, ID3DXEffect*, FixedFunctionShader::ShaderKey::hasher> FixedFunctionShader::cacheEffects;
FixedFunctionShader::ShaderLRU FixedFunctionShader::shaderLRU;
ID3DXEffect* FixedFunctionShader::effectDefaultPurple;
bool FixedFunctionShader::indexedSkinningShadersCompatible = false;
D3DXMATRIX FixedFunctionShader::skinWorldTransforms[MGE_INDEXED_SKINNING_PALETTE_SIZE];
D3DXMATRIX FixedFunctionShader::skinWorldViewTransforms[MGE_INDEXED_SKINNING_PALETTE_SIZE];

D3DXHANDLE FixedFunctionShader::ehWorld, FixedFunctionShader::ehWorldView;
D3DXHANDLE FixedFunctionShader::ehVertexBlendState, FixedFunctionShader::ehVertexBlendPalette;
D3DXHANDLE FixedFunctionShader::ehTex0, FixedFunctionShader::ehTex1, FixedFunctionShader::ehTex2, FixedFunctionShader::ehTex3, FixedFunctionShader::ehTex4, FixedFunctionShader::ehTex5;
D3DXHANDLE FixedFunctionShader::ehMaterialDiffuse, FixedFunctionShader::ehMaterialAmbient, FixedFunctionShader::ehMaterialEmissive;
D3DXHANDLE FixedFunctionShader::ehLightSceneAmbient, FixedFunctionShader::ehLightSunDiffuse, FixedFunctionShader::ehLightDiffuse;
D3DXHANDLE FixedFunctionShader::ehLightSunDirection, FixedFunctionShader::ehLightPosition, FixedFunctionShader::ehLightAmbient;
D3DXHANDLE FixedFunctionShader::ehLightFalloffQuadratic, FixedFunctionShader::ehLightFalloffLinear, FixedFunctionShader::ehLightFalloffConstant;
D3DXHANDLE FixedFunctionShader::ehTexgenTransform, FixedFunctionShader::ehBumpMatrix, FixedFunctionShader::ehBumpLumiScaleBias;

float FixedFunctionShader::sunMultiplier, FixedFunctionShader::ambMultiplier;

std::thread FixedFunctionShader::precacheThread;
std::mutex FixedFunctionShader::precacheMutex;
std::mutex FixedFunctionShader::compileMutex;
unordered_map<FixedFunctionShader::ShaderKey, ID3DXBuffer*, FixedFunctionShader::ShaderKey::hasher> FixedFunctionShader::precompiled;

IDxvkMorrowindPplInterop1* FixedFunctionShader::m_morrowindInterop = nullptr;
FixedFunctionShader::PplSceneState FixedFunctionShader::m_pplSceneState = {};
unsigned long long FixedFunctionShader::m_nativePplDraws = 0;
unsigned long long FixedFunctionShader::m_nativePplUnavailable = 0;
unsigned long long FixedFunctionShader::m_nativePplUnsupported = 0;
unsigned long long FixedFunctionShader::m_nativePplFailures = 0;
bool FixedFunctionShader::m_loggedNativePplUnsupported = false;
bool FixedFunctionShader::m_loggedNativePplFailure = false;

void FixedFunctionShader::waitForPrecacheThread() {
    if (precacheThread.joinable()) {
        precacheThread.join();
    }
}

void FixedFunctionShader::resetSkinningTransforms() {
    for (UINT i = 0; i < MGE_INDEXED_SKINNING_PALETTE_SIZE; ++i) {
        D3DXMatrixIdentity(&skinWorldTransforms[i]);
        D3DXMatrixIdentity(&skinWorldViewTransforms[i]);
    }
}

void FixedFunctionShader::setSkinningWorldTransform(UINT index, const D3DXMATRIX* world, const D3DXMATRIX* view) {
    if (index >= MGE_INDEXED_SKINNING_PALETTE_SIZE) {
        return;
    }

    skinWorldTransforms[index] = *world;
    D3DXMatrixMultiply(&skinWorldViewTransforms[index], world, view);
}

void FixedFunctionShader::setSkinningViewTransform(const D3DXMATRIX* view) {
    for (UINT i = 0; i < MGE_INDEXED_SKINNING_PALETTE_SIZE; ++i) {
        D3DXMatrixMultiply(&skinWorldViewTransforms[i], &skinWorldTransforms[i], view);
    }
}

const D3DXMATRIX* FixedFunctionShader::getSkinningWorldViewTransforms() {
    return skinWorldViewTransforms;
}


bool FixedFunctionShader::init(IDirect3DDevice* d, ID3DXEffectPool* pool) {
    release();

    device = d;
    constantPool = pool;
    indexedSkinningShadersCompatible = false;
    m_pplSceneState = PplSceneState();
    m_nativePplDraws = 0;
    m_nativePplUnavailable = 0;
    m_nativePplUnsupported = 0;
    m_nativePplFailures = 0;
    m_loggedNativePplUnsupported = false;
    m_loggedNativePplFailure = false;

    if (Configuration.EnableNativePplPackets) {
        HRESULT interopHr = device->QueryInterface(
            __uuidof(IDxvkMorrowindPplInterop1),
            reinterpret_cast<void**>(&m_morrowindInterop));

        if (FAILED(interopHr)) {
            LOG::logline("-- Native PPL packets unavailable (DXVK PPL interop query failed, hr 0x%08lx)", interopHr);
        } else {
            const uint64_t capabilities = m_morrowindInterop->GetCapabilities();

            if (!(capabilities & DXVK_MORROWIND_CAP_PPL_DRAW_V2)) {
                LOG::logline(
                    "-- Native PPL packets unavailable (caps 0x%08lx%08lx)",
                    static_cast<unsigned long>(capabilities >> 32),
                    static_cast<unsigned long>(capabilities));
                m_morrowindInterop->Release();
                m_morrowindInterop = nullptr;
            } else {
                LOG::logline(
                    "-- Native PPL packets armed (caps 0x%08lx%08lx)",
                    static_cast<unsigned long>(capabilities >> 32),
                    static_cast<unsigned long>(capabilities));
            }
        }
    }

    // Create last resort shader when a generated shader fails somehow
    const D3DXMACRO generateDefault[] = { "FFE_ERROR_MATERIAL", "", 0, 0 };
    ID3DXEffect* effect;
    ID3DXBuffer* errors;

    HRESULT hr = D3DXCreateEffectFromFile(device, "Data Files\\shaders\\core\\XE FixedFuncEmu.fx", generateDefault, 0, MGE_FFE_COMPILE_FLAGS|D3DXFX_LARGEADDRESSAWARE, constantPool, &effect, &errors);
    if (hr != D3D_OK) {
        if (errors) {
            LOG::write("!! Shader compile errors:\n");
            LOG::write(reinterpret_cast<const char*>(errors->GetBufferPointer()));
            LOG::write("\n");
            errors->Release();
        }
        return false;
    }

    // Use it to bind shared parameters too
    ehWorld = effect->GetParameterByName(0, "world");
    ehVertexBlendState = effect->GetParameterByName(0, "vertexBlendState");
    ehVertexBlendPalette = effect->GetParameterByName(0, "vertexBlendPalette");
    ehTex0 = effect->GetParameterByName(0, "tex0");
    ehTex1 = effect->GetParameterByName(0, "tex1");
    ehTex2 = effect->GetParameterByName(0, "tex2");
    ehTex3 = effect->GetParameterByName(0, "tex3");
    ehTex4 = effect->GetParameterByName(0, "tex4");
    ehTex5 = effect->GetParameterByName(0, "tex5");

    D3DXPARAMETER_DESC paletteDesc = {};
    indexedSkinningShadersCompatible = ehVertexBlendPalette
        && SUCCEEDED(effect->GetParameterDesc(ehVertexBlendPalette, &paletteDesc))
        && (paletteDesc.Class == D3DXPC_MATRIX_ROWS || paletteDesc.Class == D3DXPC_MATRIX_COLUMNS)
        && paletteDesc.Type == D3DXPT_FLOAT
        && paletteDesc.Rows == 4
        && paletteDesc.Columns == 4
        && paletteDesc.Elements >= MGE_INDEXED_SKINNING_PALETTE_SIZE;

    ehWorldView = effect->GetParameterByName(0, "worldview");
    ehMaterialDiffuse = effect->GetParameterByName(0, "materialDiffuse");
    ehMaterialAmbient = effect->GetParameterByName(0, "materialAmbient");
    ehMaterialEmissive = effect->GetParameterByName(0, "materialEmissive");
    ehLightSceneAmbient = effect->GetParameterByName(0, "lightSceneAmbient");
    ehLightSunDiffuse = effect->GetParameterByName(0, "lightSunDiffuse");
    ehLightSunDirection = effect->GetParameterByName(0, "lightSunDirection");
    ehLightDiffuse = effect->GetParameterByName(0, "lightDiffuse");
    ehLightAmbient = effect->GetParameterByName(0, "lightAmbient");
    ehLightPosition = effect->GetParameterByName(0, "lightPosition");
    ehLightFalloffQuadratic = effect->GetParameterByName(0, "lightFalloffQuadratic");
    ehLightFalloffLinear = effect->GetParameterByName(0, "lightFalloffLinear");
    ehLightFalloffConstant = effect->GetParameterByName(0, "lightFalloffConstant");
    ehTexgenTransform = effect->GetParameterByName(0, "texgenTransform");
    ehBumpMatrix = effect->GetParameterByName(0, "bumpMatrix");
    ehBumpLumiScaleBias = effect->GetParameterByName(0, "bumpLumiScaleBias");

    effectDefaultPurple = effect;
    sunMultiplier = ambMultiplier = 1.0;

    // Clear cache and LRU, important if the renderer resets
    shaderLRU.effect = nullptr;
    shaderLRU.last_sk = ShaderKey();
    cacheEffects.clear();

    // Pre-warm cache if any per-pixel mode is active
    if (Configuration.MGEFlags & USE_FFESHADER) {
        LOG::logline("-- Per-pixel shader precaching");
        precacheAsync();
    }

    return true;
}

void FixedFunctionShader::precacheAsync() {
    // Move precaching to a separate thread - essential variants to prevent stuttering
    waitForPrecacheThread();
    {
        std::scoped_lock lk(precacheMutex);
        for (auto& kv : precompiled) {
            if (kv.second) {
                kv.second->Release();
            }
        }
        precompiled.clear();
    }

    precacheThread = std::thread([]() {
        LOG::logline("-- Starting async per-pixel shader precaching (essential variants)");

        ShaderKey skCommon;
        memset(&skCommon, 0, sizeof skCommon);
        skCommon.uvSets = 1;

        int compiledVariants = 0;
        ID3DXBuffer* code = nullptr;

        for (int vertexCol = 0; vertexCol <= 1; ++vertexCol) {
            skCommon.vertexColour = vertexCol;
            skCommon.vertexMaterial = vertexCol + 1;

            for (int heavyLighting = 0; heavyLighting <= 1; ++heavyLighting) {
                skCommon.heavyLighting = heavyLighting;

                for (int skinning = 0; skinning <= 1; ++skinning) {
                    skCommon.usesSkinning = skinning;

                    // Standard diffuse texturing (most common)
                    skCommon.activeStages = 1;
                    skCommon.fogMode = 1;
                    skCommon.usesTexgen = 0;
                    skCommon.stage[0] = { D3DTOP_MODULATE, D3DTA_TEXTURE, D3DTA_DIFFUSE, D3DTA_CURRENT, 1, 0, 0, 0 };
                    memset(&skCommon.stage[1], 0, sizeof skCommon.stage[1]);
                    code = compileMWShader(skCommon);
                    if (code) {
                        std::scoped_lock lk(precacheMutex);
                        if (!precompiled.emplace(skCommon, code).second) {
                            code->Release();
                        }
                    }
                    compiledVariants++;

                    // Dual texture (common for details)
                    skCommon.activeStages = 2;
                    skCommon.fogMode = 1;
                    skCommon.usesTexgen = 0;
                    skCommon.stage[0] = { D3DTOP_MODULATE, D3DTA_TEXTURE, D3DTA_DIFFUSE, D3DTA_CURRENT, 1, 0, 0, 0 };
                    skCommon.stage[1] = { D3DTOP_ADD, D3DTA_TEXTURE, D3DTA_CURRENT, D3DTA_CURRENT, 0, 0, 0, 0 };
                    code = compileMWShader(skCommon);
                    if (code) {
                        std::scoped_lock lk(precacheMutex);
                        if (!precompiled.emplace(skCommon, code).second) {
                            code->Release();
                        }
                    }
                    compiledVariants++;

                    // Particle effects (additive blend)
                    skCommon.activeStages = 1;
                    skCommon.fogMode = 2;
                    skCommon.usesTexgen = 0;
                    skCommon.stage[0] = { D3DTOP_MODULATE, D3DTA_TEXTURE, D3DTA_DIFFUSE, D3DTA_CURRENT, 1, 0, 0, 0 };
                    memset(&skCommon.stage[1], 0, sizeof skCommon.stage[1]);
                    code = compileMWShader(skCommon);
                    if (code) {
                        std::scoped_lock lk(precacheMutex);
                        if (!precompiled.emplace(skCommon, code).second) {
                            code->Release();
                        }
                    }
                    compiledVariants++;

                    // Enchantment effects
                    skCommon.activeStages = 2;
                    skCommon.fogMode = 0;
                    skCommon.usesTexgen = 1;
                    skCommon.stage[0] = { D3DTOP_MODULATE, D3DTA_TEXTURE, D3DTA_DIFFUSE, D3DTA_CURRENT, 0, 1, 0, 3 };
                    skCommon.stage[1] = { D3DTOP_MODULATE, D3DTA_TEXTURE, D3DTA_CURRENT, D3DTA_CURRENT, 1, 0, 0, 0 };
                    code = compileMWShader(skCommon);
                    if (code) {
                        std::scoped_lock lk(precacheMutex);
                        if (!precompiled.emplace(skCommon, code).second) {
                            code->Release();
                        }
                    }
                    compiledVariants++;
                }

                // Untextured surfaces
                skCommon.usesSkinning = 0;
                skCommon.fogMode = 1;
                skCommon.usesTexgen = 0;
                skCommon.activeStages = 1;
                skCommon.stage[0] = { D3DTOP_SELECTARG2, D3DTA_TEXTURE, D3DTA_DIFFUSE, D3DTA_CURRENT, 1, 0, 0, 0 };
                memset(&skCommon.stage[1], 0, sizeof skCommon.stage[1]);
                code = compileMWShader(skCommon);
                if (code) {
                    std::scoped_lock lk(precacheMutex);
                    if (!precompiled.emplace(skCommon, code).second) {
                        code->Release();
                    }
                }
                compiledVariants++;

                // Progress logging
                if (compiledVariants % 4 == 0) {
                    LOG::logline("-- Precaching progress: %d shaders", compiledVariants);
                }
            }
        }

        LOG::logline("-- Async precaching completed: %d essential shaders compiled", compiledVariants);
    });
}

void FixedFunctionShader::updateLighting(float sunMult, float ambMult) {
    sunMultiplier = sunMult;
    ambMultiplier = ambMult;
}

// Packs light state into the width the selected renderer consumes. The width is
// chosen before traversal, as the reverse traversal order and the shared falloff
// constant make the inspected prefix semantically significant.
void FixedFunctionShader::buildPplDrawData(
    const RenderedState* rs,
    const FragmentState* frs,
    LightState* lightrs,
    const ShaderKey& key,
    uint32_t packWidth,
    PplDrawData* out) {
    *out = PplDrawData();
    out->key = key;

    memcpy(&out->materialDiffuse, &frs->material.diffuse, sizeof(out->materialDiffuse));
    memcpy(&out->materialAmbient, &frs->material.ambient, sizeof(out->materialAmbient));
    memcpy(&out->materialEmissive, &frs->material.emissive, sizeof(out->materialEmissive));

    out->lightFalloffConstant = 0.33f;

    RGBVECTOR sunDiffuse(0, 0, 0);
    RGBVECTOR ambient = lightrs->globalAmbient;
    size_t n = std::min<size_t>(lightrs->active.size(), packWidth);
    size_t pointLightCount = 0;

    // Reverse traversal is load-bearing: decoding can update the shared falloff
    // constant, which later lights in this order consume.
    for (; n --> 0; ) {
        DWORD i = lightrs->active[n];
        LightState::Light* light = &lightrs->lights.find(i)->second;

        if (lightrs->lightsTransformed.find(i) == lightrs->lightsTransformed.end()) {
            if (light->type == D3DLIGHT_DIRECTIONAL) {
                D3DXVec3TransformNormal(
                    (D3DXVECTOR3*)&light->viewspacePos,
                    (D3DXVECTOR3*)&light->position,
                    &rs->viewTransform);
            } else {
                // Light positions are recorded in absolute space; in camera-relative
                // space the view carries no translation, so subtract the camera here.
                D3DVECTOR position = light->position;
                if (CameraRelative::active()) {
                    CameraRelative::relativePosition(&light->position, &position);
                }
                D3DXVec3TransformCoord(
                    (D3DXVECTOR3*)&light->viewspacePos,
                    (D3DXVECTOR3*)&position,
                    &rs->viewTransform);
            }

            lightrs->lightsTransformed[i] = true;
        }

        if (light->type == D3DLIGHT_POINT) {
            DecodedPointLight decoded = decodeMorrowindPointLight(
                light->diffuse,
                light->falloff,
                out->lightFalloffConstant);

            memcpy(&out->lightDiffuse[pointLightCount], &decoded.diffuse, sizeof(decoded.diffuse));

            out->lightPosition[pointLightCount] = light->viewspacePos.x;
            out->lightPosition[pointLightCount + packWidth] = light->viewspacePos.y;
            out->lightPosition[pointLightCount + 2 * packWidth] = light->viewspacePos.z;
            if (decoded.viewspaceZBias != 0) {
                out->lightPosition[pointLightCount + 2 * packWidth] += decoded.viewspaceZBias;
            }
            out->lightAmbient[pointLightCount] = decoded.ambient;
            out->lightFalloffLinear[pointLightCount] = decoded.attenuation.y;
            out->lightFalloffQuadratic[pointLightCount] = decoded.attenuation.z;

            ++pointLightCount;
        } else if (light->type == D3DLIGHT_DIRECTIONAL) {
            memcpy(&out->sunDirection, &light->viewspacePos, sizeof(out->sunDirection));
            out->hasSunDirection = true;

            sunDiffuse = light->diffuse;
            ambient.r += light->ambient.x;
            ambient.g += light->ambient.y;
            ambient.b += light->ambient.z;
        }
    }

    // An unlit draw carries no point lights, even if lights were enabled.
    out->pointLightCount = out->key.vertexMaterial != 0
        ? static_cast<uint32_t>(pointLightCount)
        : 0u;

    sunDiffuse *= sunMultiplier;
    ambient *= ambMultiplier;

    if (lightrs->ambientWhite) {
        ambient.r = ambient.g = ambient.b = 1.25f;
        sunDiffuse.r = sunDiffuse.g = sunDiffuse.b = 0.0f;
    }

    out->sceneAmbient = ambient;
    out->sunDiffuse = sunDiffuse;

    if (out->key.usesBumpmap) {
        const FragmentState::Stage& bumpStage = frs->stage[out->key.bumpmapStage];
        memcpy(out->bumpMatrix, &bumpStage.bumpEnvMat[0][0], sizeof(out->bumpMatrix));
        out->bumpLumiScaleBias[0] = bumpStage.bumpLumiScale;
        out->bumpLumiScaleBias[1] = bumpStage.bumpLumiBias;
    }

    if (out->key.usesTexgen) {
        out->texgenTransform = frs->stage[out->key.texgenStage].textureTransform;
    }
}

void FixedFunctionShader::renderMorrowindLegacy(
    const RenderedState* rs,
    const FragmentState* frs,
    const PplDrawData& data) {
    ID3DXEffect* effectFFE;

    if (data.key == shaderLRU.last_sk) {
        effectFFE = shaderLRU.effect;
    } else {
        decltype(cacheEffects)::const_iterator iEffect = cacheEffects.find(data.key);
        effectFFE = iEffect != cacheEffects.end()
            ? iEffect->second
            : generateMWShader(data.key);

        shaderLRU.effect = effectFFE;
        shaderLRU.last_sk = data.key;
    }

    effectFFE->SetVector(ehMaterialDiffuse, &data.materialDiffuse);
    effectFFE->SetVector(ehMaterialAmbient, &data.materialAmbient);
    effectFFE->SetVector(ehMaterialEmissive, &data.materialEmissive);

    if (data.hasSunDirection) {
        effectFFE->SetFloatArray(ehLightSunDirection, (const float*)&data.sunDirection, 3);
    }

    effectFFE->SetFloatArray(ehLightSceneAmbient, data.sceneAmbient, 3);
    effectFFE->SetFloatArray(ehLightSunDiffuse, data.sunDiffuse, 3);
    effectFFE->SetVectorArray(ehLightDiffuse, data.lightDiffuse, MGE_LEGACY_PPL_MAX_LIGHTS);
    effectFFE->SetFloatArray(ehLightAmbient, data.lightAmbient, MGE_LEGACY_PPL_MAX_LIGHTS);
    effectFFE->SetFloatArray(ehLightPosition, data.lightPosition, 3 * MGE_LEGACY_PPL_MAX_LIGHTS);
    effectFFE->SetFloatArray(ehLightFalloffQuadratic, data.lightFalloffQuadratic, MGE_LEGACY_PPL_MAX_LIGHTS);
    effectFFE->SetFloatArray(ehLightFalloffLinear, data.lightFalloffLinear, MGE_LEGACY_PPL_MAX_LIGHTS);
    effectFFE->SetFloat(ehLightFalloffConstant, data.lightFalloffConstant);

    if (data.key.usesBumpmap) {
        effectFFE->SetFloatArray(ehBumpMatrix, data.bumpMatrix, 4);
        effectFFE->SetFloatArray(ehBumpLumiScaleBias, data.bumpLumiScaleBias, 2);
    }

    if (data.key.usesTexgen) {
        effectFFE->SetMatrix(ehTexgenTransform, &data.texgenTransform);
    }

    const D3DXHANDLE ehIndex[] = { ehTex0, ehTex1, ehTex2, ehTex3, ehTex4, ehTex5 };
    for (size_t n = 0; n != std::min<size_t>(data.key.activeStages, DXVK_MORROWIND_PPL_MAX_STAGES); ++n) {
        effectFFE->SetTexture(ehIndex[n], frs->stage[n].texture);
    }

    effectFFE->SetInt(ehVertexBlendState, rs->vertexBlendState);
    if (data.key.indexedSkinning) {
        effectFFE->SetMatrixArray(ehVertexBlendPalette, skinWorldViewTransforms, MGE_INDEXED_SKINNING_PALETTE_SIZE);
    } else if (rs->vertexBlendState) {
        effectFFE->SetMatrixArray(ehVertexBlendPalette, rs->worldViewTransforms, 4);
    } else {
        effectFFE->SetMatrix(ehWorld, &rs->worldTransforms[0]);
        effectFFE->SetMatrix(ehWorldView, &rs->worldViewTransforms[0]);
    }

    UINT passes;
    effectFFE->Begin(&passes, D3DXFX_DONOTSAVESTATE);
    effectFFE->BeginPass(0);
    device->DrawIndexedPrimitive(
        rs->primType,
        rs->baseIndex,
        rs->minIndex,
        rs->vertCount,
        rs->startIndex,
        rs->primCount);
    effectFFE->EndPass();
    effectFFE->End();

    device->SetVertexShader(NULL);
    device->SetPixelShader(NULL);
}

void FixedFunctionShader::updatePplSceneState(
    const D3DXMATRIX* projection,
    float nearFogStart,
    float nearFogRange,
    const RGBVECTOR& fogColor) {
    if (!projection) {
        m_pplSceneState.initialized = false;
        return;
    }

    m_pplSceneState.projection = *projection;
    m_pplSceneState.nearFogStart = nearFogStart;
    m_pplSceneState.nearFogRange = nearFogRange;
    m_pplSceneState.fogColor = fogColor;
    m_pplSceneState.initialized = true;
}

void FixedFunctionShader::renderMorrowind(
    const RenderedState* rs,
    const FragmentState* frs,
    LightState* lightrs) {
    if (!rs || !frs || !lightrs) {
        return;
    }

    const ShaderKey key(rs, frs, lightrs);

    // Decide the renderer before packing: the native packet takes 32 point
    // lights, the legacy effect exactly 8. Every early-out of the native path
    // has to be settled here, so that an unsupported draw packs once.
    DxvkMorrowindPplDrawV1 packet;
    bool useNative = false;

    if (Configuration.EnableNativePplPackets) {
        if (!m_morrowindInterop || !m_pplSceneState.initialized) {
            ++m_nativePplUnavailable;
        } else if (!encodeNativePplKey(key, &packet)) {
            ++m_nativePplUnsupported;
            if (!m_loggedNativePplUnsupported) {
                m_loggedNativePplUnsupported = true;
                LOG::logline("-- Native PPL packet encountered an unsupported shader key; falling back per draw");
                key.log();
            }
        } else {
            useNative = true;
        }
    }

    PplDrawData data;
    buildPplDrawData(
        rs, frs, lightrs, key,
        useNative ? DXVK_MORROWIND_PPL_MAX_LIGHTS : MGE_LEGACY_PPL_MAX_LIGHTS,
        &data);

    if (useNative) {
        if (renderMorrowindNative(rs, data, packet)) {
            return;
        }

        // The attempted draw failed, so repack the same captured state at the
        // width the legacy effect consumes.
        buildPplDrawData(rs, frs, lightrs, key, MGE_LEGACY_PPL_MAX_LIGHTS, &data);
    }

    renderMorrowindLegacy(rs, frs, data);
}

ID3DXBuffer* FixedFunctionShader::compileMWShader(const ShaderKey& sk) {
    ShaderSource src;
    if (!buildShaderSource(sk, src)) {
        return nullptr;
    }

    ID3DXBuffer* bytecode = nullptr;
    HRESULT hr;
    {
        std::scoped_lock cl(compileMutex);
        ID3DXEffectCompiler* compiler = nullptr;
        ID3DXBuffer* errors = nullptr;
        hr = D3DXCreateEffectCompilerFromFileA("Data Files\\shaders\\core\\XE FixedFuncEmu.fx", src.macros.data(), nullptr, MGE_FFE_COMPILE_FLAGS, &compiler, &errors);
        if (errors) {
            errors->Release();
        }
        if (FAILED(hr)) {
            return nullptr;
        }

        ID3DXBuffer* cerr = nullptr;
        hr = compiler->CompileEffect(MGE_FFE_COMPILE_FLAGS, &bytecode, &cerr);
        compiler->Release();
        if (cerr) {
            cerr->Release();
        }
    }

    if (FAILED(hr)) {
        return nullptr;
    }

    return bytecode;
}

ID3DXEffect* FixedFunctionShader::generateMWShader(const ShaderKey& sk) {
    ID3DXBuffer* code = nullptr;
    {
        std::scoped_lock lk(precacheMutex);
        auto it = precompiled.find(sk);
        if (it != precompiled.end()) {
            code = it->second;
            precompiled.erase(it);
        }
    }

    ID3DXEffect* effectFFE = nullptr;
    ID3DXBuffer* errors = nullptr;
    HRESULT hr;

    if (code) {
        hr = D3DXCreateEffect(device, code->GetBufferPointer(), code->GetBufferSize(), nullptr, nullptr, D3DXFX_LARGEADDRESSAWARE, constantPool, &effectFFE, &errors);
        code->Release();
    } else {
        ShaderSource src;
        if (!buildShaderSource(sk, src)) {
            effectDefaultPurple->AddRef();
            cacheEffects[sk] = effectDefaultPurple;
            return effectDefaultPurple;
        }
        {
            std::scoped_lock cl(compileMutex);
            hr = D3DXCreateEffectFromFileA(device, "Data Files\\shaders\\core\\XE FixedFuncEmu.fx", src.macros.data(), 0, MGE_FFE_COMPILE_FLAGS | D3DXFX_LARGEADDRESSAWARE, constantPool, &effectFFE, &errors);
        }
    }

    if (FAILED(hr)) {
        LOG::logline("!! Generating FFE shader: compile error %xh", hr);
        if (errors) {
            LOG::write("!! Shader compile errors:\n");
            LOG::write(reinterpret_cast<const char*>(errors->GetBufferPointer()));
            LOG::write("\n");
            errors->Release();
            errors = nullptr;
        }
        LOG::write("\n");
        effectDefaultPurple->AddRef();
        effectFFE = effectDefaultPurple;
    }

    if (errors) {
        errors->Release();
    }

    cacheEffects[sk] = effectFFE;
    return effectFFE;
}

void FixedFunctionShader::release() {
    waitForPrecacheThread();
    indexedSkinningShadersCompatible = false;

    if (m_nativePplDraws || m_nativePplUnavailable
     || m_nativePplUnsupported || m_nativePplFailures) {
        LOG::logline(
            "-- Native PPL packet totals: accepted %llu, unavailable %llu, unsupported %llu, failures %llu",
            m_nativePplDraws,
            m_nativePplUnavailable,
            m_nativePplUnsupported,
            m_nativePplFailures);
    }

    if (m_morrowindInterop) {
        m_morrowindInterop->Release();
        m_morrowindInterop = nullptr;
    }
    m_pplSceneState = PplSceneState();

    for (auto& kv : precompiled) {
        if (kv.second) {
            kv.second->Release();
        }
    }
    precompiled.clear();

    for (auto& i : cacheEffects) {
        if (i.second) {
            i.second->Release();
        }
    }

    shaderLRU.effect = nullptr;
    shaderLRU.last_sk = ShaderKey();
    cacheEffects.clear();
    if (effectDefaultPurple) {
        effectDefaultPurple->Release();
        effectDefaultPurple = nullptr;
    }
}



// ShaderKey - Captures a generatable shader configuration

FixedFunctionShader::ShaderKey::ShaderKey() {
    memset(this, 0, sizeof(ShaderKey));         // Clear padding bits for compares
}

FixedFunctionShader::ShaderKey::ShaderKey(const RenderedState* rs, const FragmentState* frs, const LightState* lightrs)
    : ShaderKey() {

    uvSets = (rs->fvf & D3DFVF_TEXCOUNT_MASK) >> D3DFVF_TEXCOUNT_SHIFT;
    usesSkinning = rs->vertexBlendState ? 1 : 0;
    indexedSkinning = usesSkinning && (rs->fvf & D3DFVF_LASTBETA_UBYTE4) ? 1 : 0;
    vertexColour = (rs->fvf & D3DFVF_DIFFUSE) ? 1 : 0;

    // Match constant material, diffuse+ambient vcol, or emissive vcol
    if (rs->useLighting) {
        heavyLighting = (lightrs->active.size() > 4) ? 1 : 0;
        vertexMaterial = 1;

        if (vertexColour) {
            if (rs->matSrcDiffuse == D3DMCS_COLOR1) {
                vertexMaterial = 2;
            } else if (rs->matSrcEmissive == D3DMCS_COLOR1) {
                vertexMaterial = 3;
            }
        }
    }

    if (rs->useFog) {
        // Match premultipled alpha or additive blending
        if (rs->blendEnable && (rs->srcBlend == D3DBLEND_ONE || rs->destBlend == D3DBLEND_ONE)) {
            fogMode = 2;
        } else {
            fogMode = 1;
        }
    }

    DWORD maxTexcoordIndex = 0;
    bool bumpStageFixup = false;

    for (int i = 0; i != 8; ++i) {
        const FragmentState::Stage& s = frs->stage[i];

        if (s.colorOp == D3DTOP_DISABLE) {
            activeStages = i;
            break;
        }

        stage[i].colorOp = s.colorOp;
        stage[i].colorArg1 = s.colorArg1;
        stage[i].colorArg2 = s.colorArg2;
        stage[i].colorArg0 = s.colorArg0;
        stage[i].alphaOpMatched = (s.alphaOp == s.colorOp);
        stage[i].alphaOpSelect1 = (s.alphaOp == D3DTOP_SELECTARG1 && s.alphaArg1 == s.colorArg1);
        stage[i].texcoordIndex = s.texcoordIndex & 3;
        stage[i].texcoordGen = s.texcoordIndex >> 16;
        maxTexcoordIndex = std::max(maxTexcoordIndex, (DWORD)stage[i].texcoordIndex);

        if (s.colorOp == D3DTOP_BUMPENVMAP || s.colorOp == D3DTOP_BUMPENVMAPLUMINANCE) {
            usesBumpmap = 1;
            bumpmapStage = i;
            stage[i].alphaOpMatched = false;
            stage[i].alphaOpSelect1 = false;
            bumpStageFixup = true;
        } else if (bumpStageFixup) {
            stage[i].alphaOpMatched = false;
            stage[i].alphaOpSelect1 = false;
            bumpStageFixup = false;
        }

        if (stage[i].texcoordGen) {
            usesTexgen = 1;
            projectiveTexgen = (s.texTransformFlags == (D3DTTFF_COUNT3 | D3DTTFF_PROJECTED)) ? 1 : 0;
            texgenStage = i;
        }
    }

    // Generate based on actual UV sets available and used
    DWORD usedUVSets = maxTexcoordIndex + 1;
    uvSets = std::min((DWORD)uvSets, usedUVSets);
}

bool FixedFunctionShader::ShaderKey::operator<(const ShaderKey& other) const {
    return memcmp(this, &other, sizeof(ShaderKey)) < 0;
}

bool FixedFunctionShader::ShaderKey::operator==(const ShaderKey& other) const {
    return memcmp(this, &other, sizeof(ShaderKey)) == 0;
}

std::size_t FixedFunctionShader::ShaderKey::hasher::operator()(const ShaderKey& k) const {
    DWORD z[9];
    memcpy(&z, &k, sizeof(z));
    return (z[0] << 16) ^ z[1] ^ z[2] ^ z[3] ^ z[4] ^ z[5] ^ z[6] ^ z[7] ^ z[8];
}

void FixedFunctionShader::ShaderKey::log() const {
    const char* opSymbols[] = { "?", "disable", "select1", "select2", "mul", "mul2x", "mul4x", "add", "addsigned", "addsigned2x", "sub", "?", "blend.diffuse", "blend.texture", "?", "?", "?", "?", "?", "?", "?", "?", "bump", "bump.l", "dp3", "mad", "?" };
    const char* argSymbols[] = { "diffuse", "current", "texture", "tfactor", "specular", "temp", "constant" };
    const char* texgenSymbols[] = { "none", "normal", "position", "reflection", "sphere" };

    const unsigned char *dump = (const unsigned char*)this;
    stringstream stream;
    stream << "   Hex: ";
    for(int i = 0; i < sizeof *this; ++i) {
        char hex[4];
        snprintf(hex, sizeof hex, "%02x ", dump[i]);
        stream << hex;
    }
    LOG::logline("%s", stream.str().c_str());

    LOG::logline("   Input state: UVs:%d skin:%d vcol:%d lights:%d vmat:%d fogm:%d", uvSets, usesSkinning, vertexColour, vertexMaterial ? (heavyLighting ? 8 : 4) : 0, vertexMaterial, fogMode);
    LOG::logline("   Texture stages:");
    for (int i = 0; i != activeStages; ++i) {
        const auto& s = stage[i];
        if (s.colorOp != D3DTOP_MULTIPLYADD) { // or D3DTOP_LERP (unused)
            LOG::logline("    [%d] %s % 12s    %s, %s            uv %d texgen %s", i,
                         s.alphaOpMatched ? "RGBA" : "RGB ",
                         opSymbols[s.colorOp], argSymbols[s.colorArg1], argSymbols[s.colorArg2],
                         s.texcoordIndex, texgenSymbols[s.texcoordGen]);
        } else {
            LOG::logline("    [%d] %s % 12s    %s, %s, %s   uv %d texgen %s", i,
                         s.alphaOpMatched ? "RGBA" : "RGB ",
                         opSymbols[s.colorOp], argSymbols[s.colorArg1], argSymbols[s.colorArg2], argSymbols[s.colorArg0],
                         s.texcoordIndex, texgenSymbols[s.texcoordGen]);
        }
        if (s.alphaOpSelect1) {
            LOG::logline("           A % 12s    %s", opSymbols[D3DTOP_SELECTARG1], argSymbols[s.colorArg1]);
        }
    }
    LOG::logline("");
}
