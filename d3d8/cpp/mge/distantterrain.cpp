#include "proxydx/d3d8header.h"
#include "support/log.h"
#include "configuration.h"
#include "distantland.h"
#include "dlformat.h"
#include "dlmapping.h"
#include "mgeversion.h"
#include "ipc/dlshare.h"

#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <limits>
#include <vector>

using std::vector;

namespace {
    struct MeshResources {
        IDirect3DVertexBuffer9* vb;
        IDirect3DIndexBuffer9* ib;
        IDirect3DTexture9* tex;

        MeshResources(IDirect3DVertexBuffer9* _vb, IDirect3DIndexBuffer9* _ib, IDirect3DTexture9* _tex)
            : vb(_vb), ib(_ib), tex(_tex) {}
    };

    vector<MeshResources> meshCollectionLand;

    constexpr const char* TerrainInitLogPrefix = "!! Terrain init:";
    constexpr const char* TerrainShaderLogPrefix = "!! Terrain shader:";
    constexpr std::uint64_t TerrainFileWindowBytes = 64ull * 1024ull * 1024ull;
    constexpr const char* TerrainFilePath = "Data Files\\distantland\\terrain.bin";
    bool readDistantLandVersion(std::uint8_t& version) {
        HANDLE file = CreateFile("Data Files\\distantland\\version", GENERIC_READ, FILE_SHARE_READ, nullptr, OPEN_EXISTING, 0, nullptr);
        if (file == INVALID_HANDLE_VALUE) {
            LOG::logline("!! Distant-land version file is missing");
            return false;
        }
        LARGE_INTEGER size{};
        DWORD bytesRead = 0;
        const bool valid = GetFileSizeEx(file, &size) && size.QuadPart == 1
            && ReadFile(file, &version, sizeof(version), &bytesRead, nullptr) && bytesRead == sizeof(version);
        CloseHandle(file);
        if (!valid) {
            LOG::logline("!! Distant-land version file is malformed or unreadable");
        }
        return valid;
    }

    // Version-15 payloads use fixed paths. The host still validates state authority; the
    // client only checks the version byte before opening payloads.
    bool resolveRuntimeOutputPaths() {
        std::uint8_t version = 0;
        if (!readDistantLandVersion(version)) {
            return false;
        }
        if (version != MGE_DL_VERSION) {
            LOG::logline("!! Distant-land output version %u is unsupported", static_cast<unsigned>(version));
            return false;
        }
        return true;
    }

    struct TerrainFileView {
        ReadOnlyMappedFile mapping{ TerrainFileWindowBytes };
        std::uint64_t fileSize = 0;
        TerrainBin::TerrainFileHeader header = {};
        std::vector<TerrainBin::TerrainMeshLayout> layouts;
    };

    bool tryConvertUploadBytes(std::uint64_t bytes, UINT& result) {
        if (bytes > std::numeric_limits<UINT>::max()) {
            return false;
        }

        result = static_cast<UINT>(bytes);
        return true;
    }

    bool readTerrainFile(TerrainFileView& terrainFile) {
        HANDLE terrainHandle = INVALID_HANDLE_VALUE;
        {
            DistantLoadInstrumentation::ScopedLoadTimer timer("terrain.open_and_validate");
            terrainHandle = CreateFile(TerrainFilePath, GENERIC_READ, FILE_SHARE_READ, 0, OPEN_EXISTING, 0, 0);
            if (terrainHandle == INVALID_HANDLE_VALUE) {
                const DWORD openError = GetLastError();
                if (openError == ERROR_FILE_NOT_FOUND || openError == ERROR_PATH_NOT_FOUND) {
                    LOG::logline("%s missing terrain.bin - %s", TerrainInitLogPrefix, TerrainFilePath);
                } else {
                    SetLastError(openError);
                    LOG::winerror("Terrain init: failed to open terrain.bin");
                }
                LOG::flush();
                return false;
            }

            if (!MappedFileUtil::QueryFileSize(terrainHandle, terrainFile.fileSize, "Terrain init: failed to query terrain.bin size")) {
                CloseHandle(terrainHandle);
                LOG::flush();
                return false;
            }

            if (!terrainFile.mapping.initialize(terrainHandle, terrainFile.fileSize)) {
                CloseHandle(terrainHandle);
                MappedFileUtil::LogMappingFailure("Terrain init: failed to create terrain.bin mapping", terrainFile.mapping);
                LOG::flush();
                return false;
            }
        }
        CloseHandle(terrainHandle);

        if (terrainFile.fileSize < TerrainBin::SerializedHeaderSize) {
            LOG::logline(
                "%s terrain.bin is truncated before the %lu-byte header completes (file_size=%llu).",
                TerrainInitLogPrefix,
                static_cast<unsigned long>(TerrainBin::SerializedHeaderSize),
                static_cast<unsigned long long>(terrainFile.fileSize)
            );
            LOG::flush();
            return false;
        }

        if (!terrainFile.mapping.copyRange(0, TerrainBin::SerializedHeaderSize, &terrainFile.header)) {
            MappedFileUtil::LogMappingFailure("Terrain init: failed to map terrain.bin header", terrainFile.mapping);
            LOG::flush();
            return false;
        }

        TerrainBin::HeaderValidation validation = TerrainBin::ValidateHeader(terrainFile.header);
        if (validation != TerrainBin::HeaderValidation::Ok) {
            const auto detail = TerrainBin::DetailedValidationMessage(terrainFile.header, validation);
            LOG::logline("%s %s.", TerrainInitLogPrefix, detail.c_str());
            LOG::flush();
            return false;
        }

        std::uint64_t cursor = 0;
        if (!TerrainBin::ReadTerrainFileLayoutsMapped(
                terrainFile.header,
                terrainFile.fileSize,
                [&](std::uint64_t offset, void* destination, std::size_t bytes) {
                    return terrainFile.mapping.copyRange(offset, static_cast<std::uint64_t>(bytes), destination);
                },
                terrainFile.layouts,
                cursor
            )) {
            LOG::logline(
                "%s terrain.bin mesh payloads are truncated or overflow the declared layout (file_size=%llu).",
                TerrainInitLogPrefix,
                static_cast<unsigned long long>(terrainFile.fileSize)
            );
            LOG::flush();
            return false;
        }

        if (cursor != terrainFile.fileSize) {
            LOG::logline(
                "%s terrain.bin has %llu unexpected trailing bytes after the declared mesh payloads.",
                TerrainInitLogPrefix,
                static_cast<unsigned long long>(terrainFile.fileSize - cursor)
            );
            LOG::flush();
            return false;
        }

        return true;
    }

    bool validateTerrainIndexData(
        ReadOnlyMappedFile& terrainMapping,
        const TerrainBin::TerrainMeshLayout& layout,
        size_t meshIndex,
        const char* logPrefix,
        bool& useIndex16
    ) {
        useIndex16 = TerrainBin::MeshUsesU16Indices(layout.header.vertexCount);
        const std::size_t indexWidth = useIndex16 ? sizeof(std::uint16_t) : sizeof(std::uint32_t);

        const bool success = terrainMapping.visitRange(layout.indexDataOffset, layout.indexDataBytes, [&](const std::uint8_t* chunk, std::size_t chunkBytes) {
            if ((chunkBytes % indexWidth) != 0) {
                return false;
            }

            const size_t indexCount = chunkBytes / indexWidth;
            for (size_t index = 0; index < indexCount; ++index) {
                const std::uint32_t fileIndex = useIndex16
                    ? static_cast<std::uint32_t>(reinterpret_cast<const std::uint16_t*>(chunk)[index])
                    : reinterpret_cast<const std::uint32_t*>(chunk)[index];
                if (fileIndex >= layout.header.vertexCount) {
                    LOG::logline("!! terrain.bin mesh %zu has index %lu outside vertex_count=%lu", meshIndex, fileIndex, layout.header.vertexCount);
                    LOG::flush();
                    return false;
                }
            }

            return true;
        });
        if (!success && terrainMapping.lastError() != ERROR_SUCCESS) {
            MappedFileUtil::LogMappingFailure(logPrefix, terrainMapping);
            LOG::flush();
            return false;
        }

        return success;
    }

    bool copyTerrainIndexData(
        ReadOnlyMappedFile& terrainMapping,
        const TerrainBin::TerrainMeshLayout& layout,
        void* destination,
        const char* logPrefix
    ) {
        // On-disk index width matches the GPU index-buffer width by construction, so
        // the payload copies verbatim with no narrowing pass.
        auto* outputBytes = static_cast<std::uint8_t*>(destination);
        const bool success = terrainMapping.visitRange(layout.indexDataOffset, layout.indexDataBytes, [&](const std::uint8_t* chunk, std::size_t chunkBytes) {
            std::memcpy(outputBytes, chunk, chunkBytes);
            outputBytes += chunkBytes;
            return true;
        });
        if (!success && terrainMapping.lastError() != ERROR_SUCCESS) {
            MappedFileUtil::LogMappingFailure(logPrefix, terrainMapping);
            LOG::flush();
            return false;
        }

        return success;
    }

    void releaseTerrainTextures(
        IDirect3DTexture9*& terrainAtlas,
        IDirect3DTexture9*& terrainMaterial,
        IDirect3DTexture9*& terrainMaterialFlags,
        IDirect3DTexture9*& terrainPatchAlbedo,
        IDirect3DTexture9*& terrainBlendPatterns
    ) {
        if (terrainAtlas) {
            terrainAtlas->Release();
            terrainAtlas = nullptr;
        }
        if (terrainMaterial) {
            terrainMaterial->Release();
            terrainMaterial = nullptr;
        }
        if (terrainMaterialFlags) {
            terrainMaterialFlags->Release();
            terrainMaterialFlags = nullptr;
        }
        if (terrainPatchAlbedo) {
            terrainPatchAlbedo->Release();
            terrainPatchAlbedo = nullptr;
        }
        if (terrainBlendPatterns) {
            terrainBlendPatterns->Release();
            terrainBlendPatterns = nullptr;
        }
    }

    const char* d3dFormatName(D3DFORMAT format) {
        switch (format) {
        case D3DFMT_A8B8G8R8:
            return "A8B8G8R8";
        case D3DFMT_A8R8G8B8:
            return "A8R8G8B8";
        case D3DFMT_X8R8G8B8:
            return "X8R8G8B8";
        case D3DFMT_DXT1:
            return "DXT1";
        case D3DFMT_DXT3:
            return "DXT3";
        case D3DFMT_DXT5:
            return "DXT5";
        default:
            return nullptr;
        }
    }

    bool textureLevelBytes(const D3DSURFACE_DESC& desc, std::uint64_t& bytes) {
        if (desc.Format == D3DFMT_DXT1) {
            bytes = static_cast<std::uint64_t>((desc.Width + 3u) / 4u)
                * static_cast<std::uint64_t>((desc.Height + 3u) / 4u) * 8u;
            return true;
        }
        if (desc.Format == D3DFMT_DXT3 || desc.Format == D3DFMT_DXT5) {
            bytes = static_cast<std::uint64_t>((desc.Width + 3u) / 4u)
                * static_cast<std::uint64_t>((desc.Height + 3u) / 4u) * 16u;
            return true;
        }

        switch (desc.Format) {
        case D3DFMT_A8B8G8R8:
        case D3DFMT_A8R8G8B8:
        case D3DFMT_X8R8G8B8:
            bytes = static_cast<std::uint64_t>(desc.Width) * desc.Height * 4u;
            return true;
        default:
            return false;
        }
    }

    bool inspectTextureFootprint(
        IDirect3DTexture9* texture,
        D3DSURFACE_DESC& topLevel,
        UINT& levelCount,
        std::uint64_t& bytes
    ) {
        bytes = 0;
        levelCount = texture->GetLevelCount();
        for (UINT level = 0; level < levelCount; ++level) {
            D3DSURFACE_DESC desc = {};
            if (texture->GetLevelDesc(level, &desc) != D3D_OK) {
                return false;
            }
            if (level == 0) {
                topLevel = desc;
            }

            std::uint64_t levelBytes = 0;
            if (!textureLevelBytes(desc, levelBytes)) {
                return false;
            }
            bytes += levelBytes;
        }
        return true;
    }

    const char* samplerStateName(D3DSAMPLERSTATETYPE state) {
        switch (state) {
        case D3DSAMP_ADDRESSU:
            return "addressu";
        case D3DSAMP_ADDRESSV:
            return "addressv";
        case D3DSAMP_MAGFILTER:
            return "magfilter";
        case D3DSAMP_MINFILTER:
            return "minfilter";
        case D3DSAMP_MIPFILTER:
            return "mipfilter";
        case D3DSAMP_MAXMIPLEVEL:
            return "maxmiplevel";
        default:
            return "sampler_state";
        }
    }

    const char* samplerStateValueName(D3DSAMPLERSTATETYPE state, DWORD value) {
        switch (state) {
        case D3DSAMP_ADDRESSU:
        case D3DSAMP_ADDRESSV:
            switch (value) {
            case D3DTADDRESS_WRAP:
                return "wrap";
            case D3DTADDRESS_CLAMP:
                return "clamp";
            default:
                return nullptr;
            }
        case D3DSAMP_MAGFILTER:
        case D3DSAMP_MINFILTER:
        case D3DSAMP_MIPFILTER:
            switch (value) {
            case D3DTEXF_NONE:
                return "none";
            case D3DTEXF_POINT:
                return "point";
            case D3DTEXF_LINEAR:
                return "linear";
            default:
                return nullptr;
            }
        default:
            return nullptr;
        }
    }

    bool validateTerrainTextureContract(
        IDirect3DTexture9* texture,
        const char* textureLabel,
        const char* path,
        D3DFORMAT expectedFormat,
        UINT expectedWidth,
        UINT expectedHeight,
        UINT exactLevels,
        UINT minimumLevels = 0
    ) {
        D3DSURFACE_DESC desc = {};
        HRESULT hr = texture->GetLevelDesc(0, &desc);
        if (hr != D3D_OK) {
            LOG::logline("%s could not inspect %s - %s (HRESULT=0x%08lx)", TerrainInitLogPrefix, textureLabel, path, hr);
            LOG::flush();
            return false;
        }

        if (desc.Format != expectedFormat) {
            bool formatMatch = (desc.Format == expectedFormat);
            if (expectedFormat == D3DFMT_A8B8G8R8 && desc.Format == D3DFMT_A8R8G8B8) {
                formatMatch = true;
            }
            if (!formatMatch) {
                const char* actualName = d3dFormatName(desc.Format);
                const char* expectedName = d3dFormatName(expectedFormat);
                LOG::logline(
                    "%s %s must use %s (got %s, raw format=0x%08lx) - %s",
                    TerrainInitLogPrefix,
                    textureLabel,
                    expectedName ? expectedName : "the expected format",
                    actualName ? actualName : "unknown",
                    static_cast<unsigned long>(desc.Format),
                    path
                );
                LOG::flush();
                return false;
            }
        }

        if (desc.Width != expectedWidth || desc.Height != expectedHeight) {
            LOG::logline(
                "%s %s size must be %lux%lu (got %lux%lu) - %s",
                TerrainInitLogPrefix,
                textureLabel,
                static_cast<unsigned long>(expectedWidth),
                static_cast<unsigned long>(expectedHeight),
                static_cast<unsigned long>(desc.Width),
                static_cast<unsigned long>(desc.Height),
                path
            );
            LOG::flush();
            return false;
        }

        const UINT levelCount = texture->GetLevelCount();
        if (exactLevels != 0 && levelCount != exactLevels) {
            LOG::logline(
                "%s %s must have %lu mip level(s) (got %lu) - %s",
                TerrainInitLogPrefix,
                textureLabel,
                static_cast<unsigned long>(exactLevels),
                static_cast<unsigned long>(levelCount),
                path
            );
            LOG::flush();
            return false;
        }
        if (minimumLevels != 0 && levelCount < minimumLevels) {
            LOG::logline(
                "%s %s must have at least %lu mip level(s) (got %lu) - %s",
                TerrainInitLogPrefix,
                textureLabel,
                static_cast<unsigned long>(minimumLevels),
                static_cast<unsigned long>(levelCount),
                path
            );
            LOG::flush();
            return false;
        }

        return true;
    }

    void bindTerrainShaderState(ID3DXEffect* terrainEffect) {
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

    bool validateTerrainSamplerStates(ID3DXEffect* terrainEffect) {
        const struct {
            const char* samplerName;
            IDirect3DBaseTexture9* texture;
            D3DSAMPLERSTATETYPE state;
            DWORD expectedValue;
        } expectations[] = {
            { "terrainAtlasSampler", DistantLand::texTerrainAtlas, D3DSAMP_MINFILTER, D3DTEXF_LINEAR },
            { "terrainAtlasSampler", DistantLand::texTerrainAtlas, D3DSAMP_MAGFILTER, D3DTEXF_LINEAR },
            { "terrainAtlasSampler", DistantLand::texTerrainAtlas, D3DSAMP_MIPFILTER, D3DTEXF_LINEAR },
            { "terrainAtlasSampler", DistantLand::texTerrainAtlas, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP },
            { "terrainAtlasSampler", DistantLand::texTerrainAtlas, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP },
            { "terrainAtlasSampler", DistantLand::texTerrainAtlas, D3DSAMP_MAXMIPLEVEL, static_cast<DWORD>(DistantLand::terrainConstants.atlasMaxLod) },
            { "terrainMaterialSampler", DistantLand::texTerrainMaterial, D3DSAMP_MINFILTER, D3DTEXF_POINT },
            { "terrainMaterialSampler", DistantLand::texTerrainMaterial, D3DSAMP_MAGFILTER, D3DTEXF_POINT },
            { "terrainMaterialSampler", DistantLand::texTerrainMaterial, D3DSAMP_MIPFILTER, D3DTEXF_NONE },
            { "terrainMaterialSampler", DistantLand::texTerrainMaterial, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP },
            { "terrainMaterialSampler", DistantLand::texTerrainMaterial, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP },
            { "terrainMaterialFlagsSampler", DistantLand::texTerrainMaterialFlags, D3DSAMP_MINFILTER, D3DTEXF_POINT },
            { "terrainMaterialFlagsSampler", DistantLand::texTerrainMaterialFlags, D3DSAMP_MAGFILTER, D3DTEXF_POINT },
            { "terrainMaterialFlagsSampler", DistantLand::texTerrainMaterialFlags, D3DSAMP_MIPFILTER, D3DTEXF_NONE },
            { "terrainMaterialFlagsSampler", DistantLand::texTerrainMaterialFlags, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP },
            { "terrainMaterialFlagsSampler", DistantLand::texTerrainMaterialFlags, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP },
            { "terrainBlendPatternSampler", DistantLand::texTerrainBlendPatterns, D3DSAMP_MINFILTER, D3DTEXF_LINEAR },
            { "terrainBlendPatternSampler", DistantLand::texTerrainBlendPatterns, D3DSAMP_MAGFILTER, D3DTEXF_LINEAR },
            { "terrainBlendPatternSampler", DistantLand::texTerrainBlendPatterns, D3DSAMP_MIPFILTER, D3DTEXF_NONE },
            { "terrainBlendPatternSampler", DistantLand::texTerrainBlendPatterns, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP },
            { "terrainBlendPatternSampler", DistantLand::texTerrainBlendPatterns, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP },
            { "terrainPatchAlbedoSampler", DistantLand::texTerrainPatchAlbedo, D3DSAMP_MINFILTER, D3DTEXF_LINEAR },
            { "terrainPatchAlbedoSampler", DistantLand::texTerrainPatchAlbedo, D3DSAMP_MAGFILTER, D3DTEXF_LINEAR },
            { "terrainPatchAlbedoSampler", DistantLand::texTerrainPatchAlbedo, D3DSAMP_MIPFILTER, D3DTEXF_LINEAR },
            { "terrainPatchAlbedoSampler", DistantLand::texTerrainPatchAlbedo, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP },
            { "terrainPatchAlbedoSampler", DistantLand::texTerrainPatchAlbedo, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP }
        };

        D3DXHANDLE technique = terrainEffect->GetTechniqueByName("T0");
        if (!technique || terrainEffect->SetTechnique(technique) != D3D_OK) {
            LOG::logline("%s could not bind terrain technique T0 for sampler validation.", TerrainShaderLogPrefix);
            LOG::flush();
            return false;
        }

        IDirect3DStateBlock9* stateSaved = nullptr;
        if (DistantLand::device->CreateStateBlock(D3DSBT_ALL, &stateSaved) != D3D_OK) {
            LOG::logline("%s could not save device state for sampler validation.", TerrainShaderLogPrefix);
            LOG::flush();
            return false;
        }

        bindTerrainShaderState(terrainEffect);
        UINT passCount = 0;
        HRESULT beginHr = terrainEffect->Begin(&passCount, D3DXFX_DONOTSAVESTATE);
        if (beginHr != D3D_OK) {
            LOG::logline("%s could not begin XE Main.fx for sampler validation (HRESULT=0x%08lx).", TerrainShaderLogPrefix, beginHr);
            stateSaved->Apply();
            stateSaved->Release();
            LOG::flush();
            return false;
        }

        bool ok = true;
        if (passCount <= PASS_RENDERLAND) {
            LOG::logline("%s XE Main.fx does not expose pass index %d for terrain validation.", TerrainShaderLogPrefix, PASS_RENDERLAND);
            ok = false;
        } else {
            HRESULT passHr = terrainEffect->BeginPass(PASS_RENDERLAND);
            if (passHr != D3D_OK) {
                LOG::logline("%s could not begin the terrain render pass for sampler validation (HRESULT=0x%08lx).", TerrainShaderLogPrefix, passHr);
                ok = false;
            } else {
                HRESULT commitHr = terrainEffect->CommitChanges();
                if (commitHr != D3D_OK) {
                    LOG::logline("%s could not commit terrain parameters before sampler validation (HRESULT=0x%08lx).", TerrainShaderLogPrefix, commitHr);
                    ok = false;
                }
                for (const auto& expectation : expectations) {
                    DWORD samplerStage = static_cast<DWORD>(-1);
                    for (DWORD stage = 0; stage < 16; ++stage) {
                        IDirect3DBaseTexture9* stageTexture = nullptr;
                        if (DistantLand::device->GetTexture(stage, &stageTexture) != D3D_OK) {
                            continue;
                        }
                        const bool matches = stageTexture == expectation.texture;
                        if (stageTexture) {
                            stageTexture->Release();
                        }
                        if (matches) {
                            DistantLand::device->SetSamplerState(stage, expectation.state, expectation.expectedValue);
                            if (samplerStage == static_cast<DWORD>(-1)) {
                                samplerStage = stage;
                            }
                        }
                    }

                    if (samplerStage == static_cast<DWORD>(-1)) {
                        LOG::logline("%s could not find the bound stage for %s during sampler validation.", TerrainShaderLogPrefix, expectation.samplerName);
                        ok = false;
                        break;
                    }

                    DWORD actualValue = 0;
                    HRESULT stateHr = DistantLand::device->GetSamplerState(samplerStage, expectation.state, &actualValue);
                    if (stateHr != D3D_OK) {
                        LOG::logline("%s could not query %s %s (sampler register %lu, HRESULT=0x%08lx).",
                            TerrainShaderLogPrefix,
                            expectation.samplerName,
                            samplerStateName(expectation.state),
                            static_cast<unsigned long>(samplerStage),
                            stateHr);
                        ok = false;
                        break;
                    }

                    if (actualValue != expectation.expectedValue) {
                        const char* actualName = samplerStateValueName(expectation.state, actualValue);
                        const char* expectedName = samplerStateValueName(expectation.state, expectation.expectedValue);
                        LOG::logline(
                            "%s %s %s must be %s (got %s / %lu).",
                            TerrainShaderLogPrefix,
                            expectation.samplerName,
                            samplerStateName(expectation.state),
                            expectedName ? expectedName : "the expected value",
                            actualName ? actualName : "an unexpected value",
                            static_cast<unsigned long>(actualValue)
                        );
                        ok = false;
                        break;
                    }
                }
                terrainEffect->EndPass();
            }
        }

        terrainEffect->End();
        stateSaved->Apply();
        stateSaved->Release();
        if (!ok) {
            LOG::flush();
        }
        return ok;
    }

    bool loadTerrainTexture(
        const char* timerName,
        const char* textureName,
        const char* path,
        const char* failureLabel,
        IDirect3DTexture9** texture,
        std::uint64_t& totalTextureBytes
    ) {
        if (GetFileAttributes(path) == INVALID_FILE_ATTRIBUTES) {
            LOG::logline("%s missing %s - %s", TerrainInitLogPrefix, failureLabel, path);
            LOG::flush();
            return false;
        }

        HRESULT hr;
        {
            DistantLoadInstrumentation::ScopedLoadTimer timer(timerName);
            hr = D3DXCreateTextureFromFileEx(
                DistantLand::device,
                path,
                D3DX_FROM_FILE,
                D3DX_FROM_FILE,
                D3DX_FROM_FILE,
                0,
                D3DFMT_UNKNOWN,
                D3DPOOL_DEFAULT,
                D3DX_DEFAULT,
                D3DX_DEFAULT,
                0,
                nullptr,
                nullptr,
                texture
            );
        }
        if (hr != D3D_OK) {
            LOG::logline("%s failed to load %s - %s (HRESULT=0x%08lx)", TerrainInitLogPrefix, failureLabel, path, hr);
            LOG::flush();
            return false;
        }

        D3DSURFACE_DESC desc = {};
        UINT levelCount = 0;
        std::uint64_t bytes = 0;
        if (inspectTextureFootprint(*texture, desc, levelCount, bytes)) {
            const char* formatName = d3dFormatName(desc.Format);
            LOG::logline(
                "-- Terrain texture: name=%s width=%lu height=%lu format=%s format_raw=0x%08lx mip_levels=%lu bytes=%llu",
                textureName,
                desc.Width,
                desc.Height,
                formatName ? formatName : "unknown",
                static_cast<unsigned long>(desc.Format),
                levelCount,
                static_cast<unsigned long long>(bytes)
            );
            totalTextureBytes += bytes;
        } else {
            LOG::logline("%s could not measure %s footprint - %s", TerrainInitLogPrefix, failureLabel, path);
            LOG::flush();
        }

        return true;
    }

    void applyTerrainConstants(const TerrainBin::TerrainFileHeader& terrainHeader) {
        DistantLand::terrainConstants.worldOrigin.x = terrainHeader.worldOrigin[0];
        DistantLand::terrainConstants.worldOrigin.y = terrainHeader.worldOrigin[1];
        DistantLand::terrainConstants.invAtlasSize.x = 1.0f / static_cast<float>(terrainHeader.atlasSize);
        DistantLand::terrainConstants.invAtlasSize.y = 1.0f / static_cast<float>(terrainHeader.atlasSize);
        DistantLand::terrainConstants.invMaterialSize.x = 1.0f / static_cast<float>(terrainHeader.materialSizeXY[0]);
        DistantLand::terrainConstants.invMaterialSize.y = 1.0f / static_cast<float>(terrainHeader.materialSizeXY[1]);
        DistantLand::terrainConstants.logicalTileSize = static_cast<float>(terrainHeader.logicalTileSize);
        DistantLand::terrainConstants.gutterSize = static_cast<float>(terrainHeader.gutterSize);
        DistantLand::terrainConstants.physicalTileSize = static_cast<float>(terrainHeader.physicalTileSize);
        DistantLand::terrainConstants.tilesPerRow = static_cast<float>(terrainHeader.tilesPerRow);
        DistantLand::terrainConstants.atlasMaxLod = static_cast<float>(terrainHeader.atlasMaxLod);
        DistantLand::terrainConstants.patternCount = static_cast<float>(terrainHeader.patternCount);
        DistantLand::terrainConstants.patternTileSize = static_cast<float>(terrainHeader.patternTileSize);
        DistantLand::terrainConstants.patternGutterSize = static_cast<float>(terrainHeader.patternGutterSize);
        DistantLand::terrainConstants.patternPhysicalSize = static_cast<float>(terrainHeader.patternPhysicalSize);
        DistantLand::terrainConstants.patternsPerRow = static_cast<float>(terrainHeader.patternsPerRow);
    }

    bool initLandscapeClientFromTerrainFile(
        TerrainFileView& terrainFile,
        double parseTerrainMs,
        std::uint64_t terrainTextureBytes
    ) {
        double createVertexBuffersMs = 0.0;
        double createIndexBuffersMs = 0.0;
        size_t totalVertices = 0;
        size_t totalTriangles = 0;
        std::uint64_t totalBytesUploaded = 0;
        std::vector<MeshResources> uploadedMeshes;
        uploadedMeshes.reserve(terrainFile.layouts.size());

        if (terrainFile.header.meshCount == 0) {
            DistantLoadInstrumentation::log_timing("terrain.parse_client", parseTerrainMs);
            DistantLoadInstrumentation::log_timing("terrain.create_vertex_buffers_client", createVertexBuffersMs);
            DistantLoadInstrumentation::log_timing("terrain.create_index_buffers_client", createIndexBuffersMs);
            LOG::logline(
                "-- Terrain load summary: mesh_count=%lu total_vertices=%zu total_triangles=%zu total_bytes_uploaded=%llu",
                terrainFile.header.meshCount,
                totalVertices,
                totalTriangles,
                static_cast<unsigned long long>(totalBytesUploaded)
            );
            LOG::logline(
                "-- Terrain memory use: file_bytes=%llu mapping_window_bytes=%llu upload_bytes=%llu texture_bytes=%llu approx_total_mb=%.2f",
                static_cast<unsigned long long>(terrainFile.fileSize),
                static_cast<unsigned long long>(terrainFile.mapping.windowBytes()),
                static_cast<unsigned long long>(totalBytesUploaded),
                static_cast<unsigned long long>(terrainTextureBytes),
                static_cast<double>(terrainFile.mapping.windowBytes() + totalBytesUploaded + terrainTextureBytes) / static_cast<double>(1 << 20)
            );
            meshCollectionLand.clear();
            return true;
        }

        auto id = IPC::InvalidVector;
        {
            auto& maybeBuffers = DistantLand::ipcClient.allocVecBlocking<IPC::LandscapeBuffers>(1, 200000, terrainFile.header.meshCount);
            if (!maybeBuffers.has_value()) {
                return false;
            }

            auto& buffers = maybeBuffers.value();
            id = buffers.id();

            if (!DistantLand::ipcClient.initLandscape(id)) {
                DistantLand::ipcClient.freeVecBlocking(id);
                return false;
            }

            buffers.start_write();
            for (size_t meshIndex = 0; meshIndex < terrainFile.layouts.size(); ++meshIndex) {
                const auto& layout = terrainFile.layouts[meshIndex];
                IDirect3DVertexBuffer9* vb = nullptr;
                IDirect3DIndexBuffer9* ib = nullptr;
                void* lockdata = nullptr;

                totalVertices += layout.header.vertexCount;
                totalTriangles += layout.header.triangleCount;

                UINT vertexUploadBytes = 0;
                if (!tryConvertUploadBytes(layout.vertexDataBytes, vertexUploadBytes)) {
                    buffers.end_write();
                    for (auto& mesh : uploadedMeshes) {
                        mesh.vb->Release();
                        mesh.ib->Release();
                    }
                    DistantLand::ipcClient.freeVecBlocking(id);
                    LOG::logline("!! terrain.bin shared mesh %zu vertex payload exceeds the Direct3D upload limit", meshIndex);
                    LOG::flush();
                    return false;
                }

                auto vbStart = DistantLoadInstrumentation::counter_now();
                HRESULT hr = DistantLand::device->CreateVertexBuffer(
                    vertexUploadBytes,
                    D3DUSAGE_WRITEONLY,
                    0,
                    D3DPOOL_DEFAULT,
                    &vb,
                    0
                );
                if (hr != D3D_OK) {
                    buffers.end_write();
                    for (auto& mesh : uploadedMeshes) {
                        mesh.vb->Release();
                        mesh.ib->Release();
                    }
                    DistantLand::ipcClient.freeVecBlocking(id);
                    LOG::logline("!! Failed to create terrain vertex buffer for shared mesh %zu", meshIndex);
                    LOG::flush();
                    return false;
                }
                hr = vb->Lock(0, 0, &lockdata, 0);
                if (hr != D3D_OK) {
                    vb->Release();
                    buffers.end_write();
                    for (auto& mesh : uploadedMeshes) {
                        mesh.vb->Release();
                        mesh.ib->Release();
                    }
                    DistantLand::ipcClient.freeVecBlocking(id);
                    LOG::logline("!! Failed to lock terrain vertex buffer for shared mesh %zu", meshIndex);
                    LOG::flush();
                    return false;
                }
                if (!terrainFile.mapping.copyRange(layout.vertexDataOffset, layout.vertexDataBytes, lockdata)) {
                    vb->Unlock();
                    vb->Release();
                    buffers.end_write();
                    for (auto& mesh : uploadedMeshes) {
                        mesh.vb->Release();
                        mesh.ib->Release();
                    }
                    DistantLand::ipcClient.freeVecBlocking(id);
                    MappedFileUtil::LogMappingFailure("Terrain init: failed to map shared terrain vertex data", terrainFile.mapping);
                    LOG::flush();
                    return false;
                }
                vb->Unlock();
                createVertexBuffersMs += DistantLoadInstrumentation::elapsed_ms(vbStart);
                totalBytesUploaded += layout.vertexDataBytes;

                bool useIndex16 = false;
                if (!validateTerrainIndexData(terrainFile.mapping, layout, meshIndex, "Terrain init: failed to map shared terrain index data", useIndex16)) {
                    vb->Release();
                    buffers.end_write();
                    for (auto& mesh : uploadedMeshes) {
                        mesh.vb->Release();
                        mesh.ib->Release();
                    }
                    DistantLand::ipcClient.freeVecBlocking(id);
                    return false;
                }

                const std::uint64_t uploadedIndexBytes = static_cast<std::uint64_t>(layout.header.triangleCount) * 3u * (useIndex16 ? sizeof(std::uint16_t) : sizeof(std::uint32_t));
                UINT indexUploadBytes = 0;
                if (!tryConvertUploadBytes(uploadedIndexBytes, indexUploadBytes)) {
                    vb->Release();
                    buffers.end_write();
                    for (auto& mesh : uploadedMeshes) {
                        mesh.vb->Release();
                        mesh.ib->Release();
                    }
                    DistantLand::ipcClient.freeVecBlocking(id);
                    LOG::logline("!! terrain.bin shared mesh %zu index payload exceeds the Direct3D upload limit", meshIndex);
                    LOG::flush();
                    return false;
                }
                auto ibStart = DistantLoadInstrumentation::counter_now();
                hr = DistantLand::device->CreateIndexBuffer(
                    indexUploadBytes,
                    D3DUSAGE_WRITEONLY,
                    useIndex16 ? D3DFMT_INDEX16 : D3DFMT_INDEX32,
                    D3DPOOL_DEFAULT,
                    &ib,
                    0
                );
                if (hr != D3D_OK) {
                    vb->Release();
                    buffers.end_write();
                    for (auto& mesh : uploadedMeshes) {
                        mesh.vb->Release();
                        mesh.ib->Release();
                    }
                    DistantLand::ipcClient.freeVecBlocking(id);
                    LOG::logline("!! Failed to create terrain index buffer for shared mesh %zu", meshIndex);
                    LOG::flush();
                    return false;
                }
                hr = ib->Lock(0, 0, &lockdata, 0);
                if (hr != D3D_OK) {
                    ib->Release();
                    vb->Release();
                    buffers.end_write();
                    for (auto& mesh : uploadedMeshes) {
                        mesh.vb->Release();
                        mesh.ib->Release();
                    }
                    DistantLand::ipcClient.freeVecBlocking(id);
                    LOG::logline("!! Failed to lock terrain index buffer for shared mesh %zu", meshIndex);
                    LOG::flush();
                    return false;
                }
                if (!copyTerrainIndexData(terrainFile.mapping, layout, lockdata, "Terrain init: failed to map shared terrain index data")) {
                    ib->Unlock();
                    ib->Release();
                    vb->Release();
                    buffers.end_write();
                    for (auto& mesh : uploadedMeshes) {
                        mesh.vb->Release();
                        mesh.ib->Release();
                    }
                    DistantLand::ipcClient.freeVecBlocking(id);
                    return false;
                }
                ib->Unlock();
                createIndexBuffersMs += DistantLoadInstrumentation::elapsed_ms(ibStart);
                totalBytesUploaded += uploadedIndexBytes;

                buffers.push_back({ vb, ib });
                uploadedMeshes.push_back(MeshResources(vb, ib, nullptr));
            }
            buffers.end_write();
        }

        // Keep the shared vector alive until the host finishes InitLandscape.
        // finishLandscapeUpload() consumes the RPC result before issuing FreeVec.
        DistantLand::landscapeHostVecId = id;
        meshCollectionLand.clear();
        meshCollectionLand.swap(uploadedMeshes);
        DistantLoadInstrumentation::log_timing("terrain.parse_client", parseTerrainMs);
        DistantLoadInstrumentation::log_timing("terrain.create_vertex_buffers_client", createVertexBuffersMs);
        DistantLoadInstrumentation::log_timing("terrain.create_index_buffers_client", createIndexBuffersMs);
        LOG::logline(
            "-- Terrain load summary: mesh_count=%lu total_vertices=%zu total_triangles=%zu total_bytes_uploaded=%llu",
            terrainFile.header.meshCount,
            totalVertices,
            totalTriangles,
            static_cast<unsigned long long>(totalBytesUploaded)
        );
        LOG::logline(
            "-- Terrain memory use: file_bytes=%llu mapping_window_bytes=%llu upload_bytes=%llu texture_bytes=%llu approx_total_mb=%.2f",
            static_cast<unsigned long long>(terrainFile.fileSize),
            static_cast<unsigned long long>(terrainFile.mapping.windowBytes()),
            static_cast<unsigned long long>(totalBytesUploaded),
            static_cast<unsigned long long>(terrainTextureBytes),
            static_cast<double>(terrainFile.mapping.windowBytes() + totalBytesUploaded + terrainTextureBytes) / static_cast<double>(1 << 20)
        );
        return true;
    }

// Terrain vertex declaration
const D3DVERTEXELEMENT9 TerrainElem[] = {
    {0, 0,  D3DDECLTYPE_FLOAT3,  D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_POSITION, 0},
    {0, 12, D3DDECLTYPE_UBYTE4N, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_NORMAL,   0},
    {0, 16, D3DDECLTYPE_D3DCOLOR, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_COLOR,   0},
    D3DDECL_END()
};


}

bool DistantLand::initLandscapeClient() {
    TerrainFileView terrainFile;
    const auto parseStart = DistantLoadInstrumentation::counter_now();
    if (!readTerrainFile(terrainFile)) {
        return false;
    }

    return initLandscapeClientFromTerrainFile(terrainFile, DistantLoadInstrumentation::elapsed_ms(parseStart), 0);
}

bool DistantLand::initLandscape() {
    if (!resolveRuntimeOutputPaths()) {
        return false;
    }
    DistantLoadInstrumentation::ScopedLoadTimer totalTimer("terrain.total");
    if (GetFileAttributes(TerrainFilePath) == INVALID_FILE_ATTRIBUTES) {
        LOG::logline("%s missing terrain.bin - %s", TerrainInitLogPrefix, TerrainFilePath);
        LOG::flush();
        return !(Configuration.MGEFlags & USE_DISTANT_LAND);
    }

    IDirect3DVertexDeclaration9* terrainDecl = nullptr;
    {
        DistantLoadInstrumentation::ScopedLoadTimer timer("terrain.create_vertex_declaration");
        HRESULT hr = device->CreateVertexDeclaration(TerrainElem, &terrainDecl);
        if (hr != D3D_OK) {
            LOG::logline("!! Failed to create terrain vertex declaration");
            return false;
        }
    }

    TerrainFileView terrainFile;
    const auto parseStart = DistantLoadInstrumentation::counter_now();
    if (!readTerrainFile(terrainFile)) {
        terrainDecl->Release();
        return false;
    }
    const double parseTerrainMs = DistantLoadInstrumentation::elapsed_ms(parseStart);
    const auto& terrainHeader = terrainFile.header;

    IDirect3DTexture9* terrainAtlas = nullptr;
    IDirect3DTexture9* terrainMaterial = nullptr;
    IDirect3DTexture9* terrainMaterialFlags = nullptr;
    IDirect3DTexture9* terrainPatchAlbedo = nullptr;
    IDirect3DTexture9* terrainBlendPatterns = nullptr;
    std::uint64_t terrainTextureBytes = 0;

    auto releaseLocalTerrainResources = [&]() {
        if (TerrainDecl == terrainDecl) { TerrainDecl = nullptr; }
        if (texTerrainAtlas == terrainAtlas) { texTerrainAtlas = nullptr; }
        if (texTerrainMaterial == terrainMaterial) { texTerrainMaterial = nullptr; }
        if (texTerrainMaterialFlags == terrainMaterialFlags) { texTerrainMaterialFlags = nullptr; }
        if (texTerrainPatchAlbedo == terrainPatchAlbedo) { texTerrainPatchAlbedo = nullptr; }
        if (texTerrainBlendPatterns == terrainBlendPatterns) { texTerrainBlendPatterns = nullptr; }

        if (terrainDecl) {
            terrainDecl->Release();
            terrainDecl = nullptr;
        }
        releaseTerrainTextures(terrainAtlas, terrainMaterial, terrainMaterialFlags, terrainPatchAlbedo, terrainBlendPatterns);
    };

    if (!loadTerrainTexture("terrain.load_atlas_texture", "atlas", TerrainBin::TerrainAtlasFilePath, "terrain atlas texture", &terrainAtlas, terrainTextureBytes)
        || !loadTerrainTexture("terrain.load_material_texture", "material", TerrainBin::TerrainMaterialFilePath, "terrain material texture", &terrainMaterial, terrainTextureBytes)
        || !loadTerrainTexture("terrain.load_material_flags_texture", "material_flags", TerrainBin::TerrainMaterialFlagsFilePath, "terrain material flags texture", &terrainMaterialFlags, terrainTextureBytes)
        || !loadTerrainTexture("terrain.load_patch_albedo_texture", "patch_albedo", TerrainBin::TerrainPatchAlbedoFilePath, "terrain patch albedo texture", &terrainPatchAlbedo, terrainTextureBytes)
        || !loadTerrainTexture("terrain.load_blend_patterns_texture", "blend_patterns", TerrainBin::TerrainBlendPatternsFilePath, "terrain blend pattern texture", &terrainBlendPatterns, terrainTextureBytes)) {
        releaseLocalTerrainResources();
        return false;
    }
    LOG::logline(
        "-- Terrain texture memory use: textures=5 bytes=%llu total_mb=%.2f",
        static_cast<unsigned long long>(terrainTextureBytes),
        static_cast<double>(terrainTextureBytes) / static_cast<double>(1 << 20)
    );

    const UINT patternRows = (terrainHeader.patternCount + terrainHeader.patternsPerRow - 1u) / terrainHeader.patternsPerRow;
    if (!validateTerrainTextureContract(
            terrainAtlas,
            "terrain atlas texture",
            TerrainBin::TerrainAtlasFilePath,
            D3DFMT_DXT1,
            terrainHeader.atlasSize,
            terrainHeader.atlasSize,
            terrainHeader.atlasMaxLod + 1u
        )
        || !validateTerrainTextureContract(
            terrainMaterial,
            "terrain material texture",
            TerrainBin::TerrainMaterialFilePath,
            D3DFMT_A8B8G8R8,
            terrainHeader.materialSizeXY[0],
            terrainHeader.materialSizeXY[1],
            1u
        )
        || !validateTerrainTextureContract(
            terrainMaterialFlags,
            "terrain material flags texture",
            TerrainBin::TerrainMaterialFlagsFilePath,
            D3DFMT_A8B8G8R8,
            terrainHeader.materialSizeXY[0],
            terrainHeader.materialSizeXY[1],
            1u
        )
        || !validateTerrainTextureContract(
            terrainPatchAlbedo,
            "terrain patch albedo texture",
            TerrainBin::TerrainPatchAlbedoFilePath,
            D3DFMT_DXT1,
            terrainHeader.materialSizeXY[0],
            terrainHeader.materialSizeXY[1],
            0u,
            2u
        )
        || !validateTerrainTextureContract(
            terrainBlendPatterns,
            "terrain blend pattern texture",
            TerrainBin::TerrainBlendPatternsFilePath,
            D3DFMT_A8B8G8R8,
            terrainHeader.patternsPerRow * terrainHeader.patternPhysicalSize,
            patternRows * terrainHeader.patternPhysicalSize,
            1u
        )) {
        releaseLocalTerrainResources();
        return false;
    }

    applyTerrainConstants(terrainHeader);
    TerrainDecl = terrainDecl;
    texTerrainAtlas = terrainAtlas;
    texTerrainMaterial = terrainMaterial;
    texTerrainMaterialFlags = terrainMaterialFlags;
    texTerrainPatchAlbedo = terrainPatchAlbedo;
    texTerrainBlendPatterns = terrainBlendPatterns;
    if (!validateTerrainSamplerStates(effect)) {
        releaseLocalTerrainResources();
        return false;
    }

    if (!initLandscapeClientFromTerrainFile(terrainFile, parseTerrainMs, terrainTextureBytes)) {
        releaseLocalTerrainResources();
        return false;
    }

    TerrainDecl = terrainDecl;
    texTerrainAtlas = terrainAtlas;
    texTerrainMaterial = terrainMaterial;
    texTerrainMaterialFlags = terrainMaterialFlags;
    texTerrainPatchAlbedo = terrainPatchAlbedo;
    texTerrainBlendPatterns = terrainBlendPatterns;
    LOG::logline(
        "-- Terrain format: magic=%.8s version=%lu vertex_stride=%lu file_index_format=auto atlas_size=%lu material_size=%lux%lu",
        reinterpret_cast<const char*>(terrainHeader.magic),
        terrainHeader.version,
        terrainHeader.vertexStride,
        terrainHeader.atlasSize,
        terrainHeader.materialSizeXY[0],
        terrainHeader.materialSizeXY[1]
    );
    return true;
}

namespace DistantLoaders {

void releaseTerrainResources() {
    for (auto& mesh : meshCollectionLand) {
        if (mesh.vb) { mesh.vb->Release(); }
        if (mesh.ib) { mesh.ib->Release(); }
    }
    meshCollectionLand.clear();

    releaseTerrainTextures(
        DistantLand::texTerrainAtlas,
        DistantLand::texTerrainMaterial,
        DistantLand::texTerrainMaterialFlags,
        DistantLand::texTerrainPatchAlbedo,
        DistantLand::texTerrainBlendPatterns
    );
}

}
