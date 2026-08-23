#include "proxydx/d3d8header.h"
#include "support/log.h"
#include "configuration.h"
#include "distantland.h"
#include "dlformat.h"
#include "dlmapping.h"
#include "morrowindbsa.h"
#include "mgeversion.h"
#include "ipc/dlshare.h"

#include <algorithm>
#include <array>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <limits>
#include <memory>
#include <string>
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

    vector<MeshResources> meshCollectionStatics;

    constexpr std::uint64_t StaticMeshesGeometryWindowBytes = 64ull * 1024ull * 1024ull;
    constexpr std::uint32_t StaticMeshShardCount = MGE_STATIC_MESH_SHARD_COUNT;

    std::string staticMeshShardPath(std::uint32_t shardId) {
        char path[64] = {};
        std::snprintf(
            path,
            sizeof(path),
            "Data Files\\distantland\\statics\\static_meshes_%0*u",
            MGE_STATIC_MESH_SHARD_ID_WIDTH,
            shardId
        );
        return path;
    }

    bool tryConvertUploadBytes(std::uint64_t bytes, UINT& result) {
        if (bytes > std::numeric_limits<UINT>::max()) {
            return false;
        }

        result = static_cast<UINT>(bytes);
        return true;
    }
    bool tryConvertCount(std::uint32_t value, int& result) {
        if (value > static_cast<std::uint32_t>(std::numeric_limits<int>::max())) {
            return false;
        }

        result = static_cast<int>(value);
        return true;
    }

    int classifyStaticComponent(const StaticMeshesBin::ComponentRecord& component, float farStaticMinSize, float veryFarStaticMinSize) {
        switch (component.classification) {
        case STATIC_NEAR:
            return 0;
        case STATIC_FAR:
            return 1;
        case STATIC_VERY_FAR:
            return 2;
        default: {
            const float radius = component.classification == STATIC_BUILDING ? component.radius * 2.0f : component.radius;
            if (radius <= farStaticMinSize) {
                return 0;
            }
            if (radius <= veryFarStaticMinSize) {
                return 1;
            }
            return 2;
        }
        }
    }

    bool isFiniteVec3(const D3DXVECTOR3& value) {
        return std::isfinite(value.x) && std::isfinite(value.y) && std::isfinite(value.z);
    }

    bool isFiniteVec3(const StaticMeshesBin::Vec3& value) {
        return std::isfinite(value.x) && std::isfinite(value.y) && std::isfinite(value.z);
    }

    bool isValidBoundingSphere(const StaticMeshesBin::BoundingSphere& sphere) {
        return std::isfinite(sphere.radius) && sphere.radius >= 0.0f && isFiniteVec3(sphere.center);
    }

    bool isValidAabb(const StaticMeshesBin::Aabb& aabb) {
        return isFiniteVec3(aabb.min)
            && isFiniteVec3(aabb.max)
            && aabb.min.x <= aabb.max.x
            && aabb.min.y <= aabb.max.y
            && aabb.min.z <= aabb.max.z;
    }

// Distant static vertex declaration
const D3DVERTEXELEMENT9 StaticElem[] = {
    {0, 0,  D3DDECLTYPE_FLOAT16_4, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_POSITION, 0},
    {0, 8,  D3DDECLTYPE_UBYTE4N,   D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_NORMAL,   0},
    {0, 12, D3DDECLTYPE_D3DCOLOR,  D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_COLOR,    0},
    {0, 16, D3DDECLTYPE_FLOAT16_2, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_TEXCOORD, 0},
    {0, 20, D3DDECLTYPE_FLOAT16_4, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_TEXCOORD, 1},
    D3DDECL_END()
};


}

struct StaticShardView {
    ReadOnlyMappedFile mapping{ StaticMeshesGeometryWindowBytes };
    StaticMeshesBin::StaticMeshesFileHeader header = {};
    const StaticMeshesBin::StaticRecord* staticRecords = nullptr;
    const StaticMeshesBin::SubsetRecord* subsetRecords = nullptr;
    const StaticMeshesBin::ComponentRecord* componentRecords = nullptr;
    std::string path;
};

struct DistantLand::StaticsLoader {
    HANDLE h = INVALID_HANDLE_VALUE;   // usage.data stream (static count, vis groups, usage)
    std::array<std::unique_ptr<StaticShardView>, StaticMeshShardCount> shards;
    StaticShardView* activeShard = nullptr;
    StaticMeshesBin::StaticMeshesFileHeader header = {};
    const StaticMeshesBin::StaticRecord* staticRecords = nullptr;
    const StaticMeshesBin::SubsetRecord* subsetRecords = nullptr;
    const StaticMeshesBin::ComponentRecord* componentRecords = nullptr;
    IDirect3DTexture9* errorTexture = nullptr;

    std::vector<DistantStatic> distantStatics;
    std::vector<DistantSubset> distantSubsets;
    std::vector<MeshResources> loadedMeshResources;
    std::vector<std::uint8_t> indexScratch;

    // Resumable cursors.
    std::uint32_t shardIndex = 0;
    std::uint32_t staticIndex = 0;
    std::uint32_t globalSubsetBase = 0;
    std::uint32_t subsetOffset = 0;          // subset within the current static
    DistantStatic runtimeStatic = {};        // in-progress static (valid while subsetOffset > 0)
    std::uint32_t expectedFirstSubsetIndex = 0;
    std::uint32_t expectedFirstComponentIndex = 0;
    std::uint64_t expectedGeometryOffset = 0;
    std::uint64_t textureBlobEnd = 0;
    std::uint64_t geometryBlobEnd = 0;
    std::uint32_t totalStaticCount = 0;
    std::uint32_t totalSubsetCount = 0;
    std::uint64_t totalMetadataPrefixBytes = 0;

    void activateShard(std::uint32_t index) {
        shardIndex = index;
        activeShard = shards[index].get();
        header = activeShard->header;
        staticRecords = activeShard->staticRecords;
        subsetRecords = activeShard->subsetRecords;
        componentRecords = activeShard->componentRecords;
        staticIndex = 0;
        subsetOffset = 0;
        expectedFirstSubsetIndex = 0;
        expectedFirstComponentIndex = 0;
        expectedGeometryOffset = header.geometry_blob_offset;
        StaticMeshesBin::TryAdd(header.texture_blob_offset, header.texture_blob_size, textureBlobEnd);
        StaticMeshesBin::TryAdd(header.geometry_blob_offset, header.geometry_blob_size, geometryBlobEnd);
    }

    // Instrumentation accumulators / totals.
    double createVertexBuffersMs = 0.0;
    double createIndexBuffersMs = 0.0;
    double loadTexturesMs = 0.0;
    double parseTotalMs = 0.0;
    size_t totalSubsets = 0;
    size_t totalVertices = 0;
    size_t totalFaces = 0;
    size_t totalFarFaces = 0;
    size_t totalVeryFarFaces = 0;
    std::uint64_t totalVertexBytes = 0;
    std::uint64_t totalIndexBytes = 0;

    // Statics not generated: the phase is a no-op whose success depends on whether
    // distant land is required.
    bool skipPhase = false;
    bool skipResult = false;
};

std::unique_ptr<DistantLand::StaticsLoader> DistantLand::staticsLoader;

// Free any statics-loader resources not yet committed to meshCollectionStatics,
// then drop the loader. Safe to call after a successful finish (loader emptied) or
// mid-phase on failure/teardown.
void DistantLand::abortStaticsPhase() {
    if (!staticsLoader) {
        return;
    }
    StaticsLoader& L = *staticsLoader;
    for (auto it = L.loadedMeshResources.rbegin(); it != L.loadedMeshResources.rend(); ++it) {
        if (it->tex) { it->tex->Release(); }
        if (it->ib) { it->ib->Release(); }
        if (it->vb) { it->vb->Release(); }
    }
    L.loadedMeshResources.clear();
    if (L.errorTexture) {
        L.errorTexture->Release();
        L.errorTexture = nullptr;
    }
    if (L.h != INVALID_HANDLE_VALUE) {
        CloseHandle(L.h);
        L.h = INVALID_HANDLE_VALUE;
    }
    staticsLoader.reset();   // unmaps every shard view via its destructor
}

// Phase init: create the static vertex declaration, open the usage stream and the
// fixed static-mesh shards, preflight every header, map their metadata prefixes,
// and create the fallback texture. Leaves cursors ready for stepStaticsPhase().
bool DistantLand::beginStaticsPhase() {
    staticsLoader = std::make_unique<StaticsLoader>();
    StaticsLoader& L = *staticsLoader;

    if (!(Configuration.MGEFlags & USE_DISTANT_STATICS)) {
        LOG::logline("-- Distant statics disabled; skipping static geometry and texture upload");
        L.skipPhase = true;
        L.skipResult = true;
        return true;
    }

    {
        DistantLoadInstrumentation::ScopedLoadTimer timer("statics.create_vertex_declaration");
        if (FAILED(device->CreateVertexDeclaration(StaticElem, &StaticDecl))) {
            LOG::logline("!! Failed to to create static vertex declaration");
            return false;
        }
    }

    if (GetFileAttributes("Data Files\\distantland\\statics") == INVALID_FILE_ATTRIBUTES) {
        LOG::logline("!! Distant statics have not been generated");
        LOG::flush();
        L.skipPhase = true;
        L.skipResult = !(Configuration.MGEFlags & USE_DISTANT_LAND);
        return true;
    }

    {
        DistantLoadInstrumentation::ScopedLoadTimer timer("statics.begin_read_statics");
        L.h = DistantLandShare::beginReadStatics();
    }
    if (L.h == INVALID_HANDLE_VALUE) {
        return false;
    }

    std::uint32_t usageStaticCount = 0;
    if (!DistantLoadInstrumentation::ReadExact(L.h, &usageStaticCount, sizeof(usageStaticCount), "statics.distant_static_count")) {
        return false;
    }

    std::uint64_t totalStaticCount = 0;
    std::uint64_t totalSubsetCount = 0;
    for (std::uint32_t shardId = 0; shardId < StaticMeshShardCount; ++shardId) {
        auto shard = std::make_unique<StaticShardView>();
        shard->path = staticMeshShardPath(shardId);
        HANDLE file = INVALID_HANDLE_VALUE;
        std::uint64_t fileSize = 0;
        {
            DistantLoadInstrumentation::ScopedLoadTimer timer("static_meshes.open_shard");
            file = CreateFile(shard->path.c_str(), GENERIC_READ, FILE_SHARE_READ, 0, OPEN_EXISTING, 0, 0);
        }
        if (file == INVALID_HANDLE_VALUE) {
            LOG::logline("!! Required distant statics shard is missing, regeneration required - %s", shard->path.c_str());
            LOG::flush();
            return false;
        }
        const std::string sizeError = "Distant statics: failed to query size for " + shard->path;
        if (!MappedFileUtil::QueryFileSize(file, fileSize, sizeError.c_str())) {
            CloseHandle(file);
            LOG::flush();
            return false;
        }
        if (!shard->mapping.initialize(file, fileSize)) {
            CloseHandle(file);
            const std::string mappingError = "Distant statics: failed to create mapping for " + shard->path;
            MappedFileUtil::LogMappingFailure(mappingError.c_str(), shard->mapping);
            LOG::flush();
            return false;
        }
        CloseHandle(file);
        if (fileSize < sizeof(StaticMeshesBin::StaticMeshesFileHeader)) {
            LOG::logline(
                "!! %s is truncated before the %u-byte header completes (file_size=%llu).",
                shard->path.c_str(),
                StaticMeshesBin::SerializedHeaderSize,
                static_cast<unsigned long long>(fileSize)
            );
            LOG::flush();
            return false;
        }
        if (!shard->mapping.copyRange(0, sizeof(shard->header), &shard->header)) {
            const std::string headerError = "Distant statics: failed to map header for " + shard->path;
            MappedFileUtil::LogMappingFailure(headerError.c_str(), shard->mapping);
            LOG::flush();
            return false;
        }
        // Header reads use the sliding window; do not retain one window per shard while all
        // persistent metadata prefixes are mapped for the resumable upload.
        shard->mapping.releaseSlidingView();
        const auto headerValidation = StaticMeshesBin::ValidateHeader(shard->header, fileSize);
        if (headerValidation != StaticMeshesBin::HeaderValidation::Ok) {
            const auto detail = StaticMeshesBin::DetailedValidationMessage(shard->header, headerValidation);
            LOG::logline("!! %s: %s.", shard->path.c_str(), detail.c_str());
            LOG::flush();
            return false;
        }
        if (!shard->mapping.mapPersistentPrefix(shard->header.geometry_blob_offset)) {
            const std::string prefixError = "Distant statics: failed to map metadata prefix for " + shard->path;
            MappedFileUtil::LogMappingFailure(prefixError.c_str(), shard->mapping);
            LOG::flush();
            return false;
        }
        shard->staticRecords = reinterpret_cast<const StaticMeshesBin::StaticRecord*>(
            shard->mapping.getPersistentRange(shard->header.static_table_offset, shard->header.static_table_size)
        );
        shard->subsetRecords = reinterpret_cast<const StaticMeshesBin::SubsetRecord*>(
            shard->mapping.getPersistentRange(shard->header.subset_table_offset, shard->header.subset_table_size)
        );
        shard->componentRecords = reinterpret_cast<const StaticMeshesBin::ComponentRecord*>(
            shard->mapping.getPersistentRange(shard->header.component_table_offset, shard->header.component_table_size)
        );
        if (!shard->staticRecords || !shard->subsetRecords || !shard->componentRecords) {
            LOG::logline("!! %s metadata tables are not fully covered by the mapped prefix.", shard->path.c_str());
            LOG::flush();
            return false;
        }
        if (totalStaticCount > std::numeric_limits<std::uint32_t>::max() - shard->header.static_count
            || totalSubsetCount > std::numeric_limits<std::uint32_t>::max() - shard->header.subset_count) {
            LOG::logline("!! Static shard header count sum overflows the global u32 runtime limits at %s.", shard->path.c_str());
            LOG::flush();
            return false;
        }
        totalStaticCount += shard->header.static_count;
        totalSubsetCount += shard->header.subset_count;
        L.totalMetadataPrefixBytes += shard->header.geometry_blob_offset;
        L.shards[shardId] = std::move(shard);
    }

    if (usageStaticCount != totalStaticCount) {
        LOG::logline(
            "!! usage.data distant static count (%lu) does not match the fixed shard header sum (%llu).",
            usageStaticCount,
            static_cast<unsigned long long>(totalStaticCount)
        );
        LOG::flush();
        return false;
    }
    L.totalStaticCount = static_cast<std::uint32_t>(totalStaticCount);
    L.totalSubsetCount = static_cast<std::uint32_t>(totalSubsetCount);
    L.distantStatics.reserve(L.totalStaticCount);
    L.distantSubsets.reserve(L.totalSubsetCount);
    L.loadedMeshResources.reserve(L.totalSubsetCount);

    // Bright yellow error texture
    if (FAILED(device->CreateTexture(1, 1, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &L.errorTexture, NULL))) {
        LOG::logline("!! Failed to create distant static fallback texture");
        return false;
    }

    D3DLOCKED_RECT yellow;
    if (FAILED(L.errorTexture->LockRect(0, &yellow, NULL, 0))) {
        LOG::logline("!! Failed to lock distant static fallback texture");
        return false;
    }
    *(DWORD*)yellow.pBits = 0xffffff00;
    L.errorTexture->UnlockRect(0);

    L.activateShard(0);
    return true;
}

// Resumable per-subset upload loop. Processes subsets until the frame budget is
// spent (phaseDone stays false; resume next frame) or the whole static set is
// loaded (phaseDone true). Returns false on a fatal error; the caller aborts the
// pump. Per-subset locals stay local; only cursors/accumulators live in the loader.
bool DistantLand::stepStaticsPhase(int budgetMs, bool& phaseDone) {
    StaticsLoader& L = *staticsLoader;
    phaseDone = false;
    if (L.skipPhase) {
        phaseDone = true;
        return true;
    }

    const auto stepStart = DistantLoadInstrumentation::counter_now();

    while (L.shardIndex < StaticMeshShardCount) {
        while (L.staticIndex < L.header.static_count) {
            const auto& staticRecord = L.staticRecords[L.staticIndex];

        if (L.subsetOffset == 0) {
            // Validate and start a new static.
            if (!StaticMeshesBin::IsKnownStaticType(staticRecord.static_type)) {
                LOG::logline("!! static_meshes static %lu has unknown static_type=%lu.", L.staticIndex, staticRecord.static_type);
                LOG::flush();
                return false;
            }
            if (!isValidBoundingSphere(staticRecord.sphere) || !isValidAabb(staticRecord.aabb)) {
                LOG::logline("!! static_meshes static %lu has invalid bounds.", L.staticIndex);
                LOG::flush();
                return false;
            }
            if (staticRecord.first_subset_index != L.expectedFirstSubsetIndex) {
                LOG::logline(
                    "!! static_meshes static %lu starts at subset %lu, expected %lu for contiguous subset ownership.",
                    L.staticIndex,
                    staticRecord.first_subset_index,
                    L.expectedFirstSubsetIndex
                );
                LOG::flush();
                return false;
            }
            if (staticRecord.subset_count > L.header.subset_count - L.expectedFirstSubsetIndex) {
                LOG::logline("!! static_meshes static %lu subset range overflows subset_count=%lu.", L.staticIndex, L.header.subset_count);
                LOG::flush();
                return false;
            }

            L.runtimeStatic = {};
            L.runtimeStatic.type = StaticMeshesBin::ToRuntimeStaticType(staticRecord.static_type);
            L.runtimeStatic.sphere = staticRecord.sphere.toRuntime();
            L.runtimeStatic.aabbMin = staticRecord.aabb.minRuntime();
            L.runtimeStatic.aabbMax = staticRecord.aabb.maxRuntime();
            L.runtimeStatic.firstSubsetIndex = L.globalSubsetBase + staticRecord.first_subset_index;
            L.runtimeStatic.numSubsets = staticRecord.subset_count;
        }

        while (L.subsetOffset < staticRecord.subset_count) {
            const std::uint32_t subsetTableIndex = staticRecord.first_subset_index + L.subsetOffset;
            const auto& subsetRecord = L.subsetRecords[subsetTableIndex];
            if (!isValidBoundingSphere(subsetRecord.sphere) || !isValidAabb(subsetRecord.aabb)) {
                LOG::logline("!! static_meshes subset %lu has invalid bounds.", subsetTableIndex);
                LOG::flush();
                return false;
            }
            if (!StaticMeshesBin::HasOnlyKnownFlags(subsetRecord.flags)) {
                LOG::logline("!! static_meshes subset %lu has unknown flags 0x%08lx.", subsetTableIndex, subsetRecord.flags);
                LOG::flush();
                return false;
            }
            if (subsetRecord.texture_path_length == 0) {
                LOG::logline("!! static_meshes subset %lu has an empty texture path.", subsetTableIndex);
                LOG::flush();
                return false;
            }
            if (subsetRecord.vertex_count == 0 || subsetRecord.triangle_count == 0) {
                LOG::logline("!! static_meshes subset %lu has an empty geometry payload.", subsetTableIndex);
                LOG::flush();
                return false;
            }

            std::uint64_t texturePathBytes = 0;
            std::uint64_t texturePathEnd = 0;
            if (!StaticMeshesBin::TryAdd(static_cast<std::uint64_t>(subsetRecord.texture_path_length), 1u, texturePathBytes)
                || subsetRecord.texture_path_offset < L.header.texture_blob_offset
                || !StaticMeshesBin::TryAdd(subsetRecord.texture_path_offset, texturePathBytes, texturePathEnd)
                || texturePathEnd > L.textureBlobEnd) {
                LOG::logline("!! static_meshes subset %lu texture path overflows the texture blob.", subsetTableIndex);
                LOG::flush();
                return false;
            }

            const std::uint32_t vertexStride = StaticMeshesBin::VertexStrideForStaticType(L.header, staticRecord.static_type);
            std::uint64_t vertexBytes = 0;
            std::uint64_t indexBytes = 0;
            std::uint64_t expectedIndexOffset = 0;
            std::uint64_t nextGeometryOffset = 0;
            if (!StaticMeshesBin::TryGetVertexDataBytes(subsetRecord.vertex_count, vertexStride, vertexBytes)
                || !StaticMeshesBin::TryGetIndexDataBytes(subsetRecord.triangle_count, indexBytes)
                || !StaticMeshesBin::TryAdd(L.expectedGeometryOffset, vertexBytes, expectedIndexOffset)
                || !StaticMeshesBin::TryAdd(expectedIndexOffset, indexBytes, nextGeometryOffset)) {
                LOG::logline("!! static_meshes subset %lu geometry sizes overflow.", subsetTableIndex);
                LOG::flush();
                return false;
            }
            if (subsetRecord.vertex_offset != L.expectedGeometryOffset
                || subsetRecord.index_offset != expectedIndexOffset
                || nextGeometryOffset > L.geometryBlobEnd) {
                LOG::logline("!! static_meshes subset %lu geometry offsets are not contiguous inside the geometry blob.", subsetTableIndex);
                LOG::flush();
                return false;
            }

            if (subsetRecord.first_component_index != L.expectedFirstComponentIndex) {
                LOG::logline(
                    "!! static_meshes subset %lu starts at component %lu, expected %lu for contiguous component ownership.",
                    subsetTableIndex,
                    subsetRecord.first_component_index,
                    L.expectedFirstComponentIndex
                );
                LOG::flush();
                return false;
            }
            if (subsetRecord.component_count > L.header.component_count - L.expectedFirstComponentIndex) {
                LOG::logline(
                    "!! static_meshes subset %lu component range overflows component_count=%lu.",
                    subsetTableIndex,
                    L.header.component_count
                );
                LOG::flush();
                return false;
            }

            const auto* components = L.componentRecords + subsetRecord.first_component_index;
            std::uint32_t expectedFirstTriangle = 0;
            std::uint32_t farFaceCount = subsetRecord.triangle_count;
            std::uint32_t veryFarFaceCount = subsetRecord.triangle_count;
            if (subsetRecord.component_count != 0) {
                farFaceCount = 0;
                veryFarFaceCount = 0;
                for (std::uint32_t componentIndex = 0; componentIndex < subsetRecord.component_count; ++componentIndex) {
                    const auto& component = components[componentIndex];
                    if (component.triangle_count == 0
                        || component.first_triangle != expectedFirstTriangle
                        || !StaticMeshesBin::IsKnownComponentStaticType(component.classification)
                        || !std::isfinite(component.radius)
                        || component.radius < 0.0f
                        || component.reserved[0] != 0
                        || component.reserved[1] != 0
                        || component.reserved[2] != 0) {
                        LOG::logline("!! static_meshes subset %lu has an invalid component record at local index %lu.", subsetTableIndex, componentIndex);
                        LOG::flush();
                        return false;
                    }
                    expectedFirstTriangle += component.triangle_count;
                    if (expectedFirstTriangle > subsetRecord.triangle_count) {
                        LOG::logline("!! static_meshes subset %lu component ranges exceed triangle_count=%lu.", subsetTableIndex, subsetRecord.triangle_count);
                        LOG::flush();
                        return false;
                    }

                    const int tier = classifyStaticComponent(
                        component,
                        Configuration.DL.FarStaticMinSize,
                        Configuration.DL.VeryFarStaticMinSize
                    );
                    if (tier >= 1) {
                        farFaceCount += component.triangle_count;
                    }
                    if (tier >= 2) {
                        veryFarFaceCount += component.triangle_count;
                    }
                }
                if (expectedFirstTriangle != subsetRecord.triangle_count) {
                    LOG::logline("!! static_meshes subset %lu component ranges do not cover all triangles.", subsetTableIndex);
                    LOG::flush();
                    return false;
                }
            }

            UINT vertexUploadBytes = 0;
            UINT indexUploadBytes = 0;
            int runtimeVertexCount = 0;
            int runtimeTriangleCount = 0;
            int runtimeFarFaceCount = 0;
            int runtimeVeryFarFaceCount = 0;
            if (!tryConvertUploadBytes(vertexBytes, vertexUploadBytes)
                || !tryConvertUploadBytes(indexBytes, indexUploadBytes)
                || !tryConvertCount(subsetRecord.vertex_count, runtimeVertexCount)
                || !tryConvertCount(subsetRecord.triangle_count, runtimeTriangleCount)
                || !tryConvertCount(farFaceCount, runtimeFarFaceCount)
                || !tryConvertCount(veryFarFaceCount, runtimeVeryFarFaceCount)) {
                LOG::logline("!! static_meshes subset %lu upload sizes exceed the Direct3D/runtime limits.", subsetTableIndex);
                LOG::flush();
                return false;
            }

            const auto* texturePathBytesView = L.activeShard->mapping.getPersistentRange(subsetRecord.texture_path_offset, texturePathBytes);
            if (!texturePathBytesView) {
                LOG::logline("!! static_meshes subset %lu texture path is outside the mapped metadata prefix.", subsetTableIndex);
                LOG::flush();
                return false;
            }
            if (texturePathBytesView[subsetRecord.texture_path_length] != 0) {
                LOG::logline("!! static_meshes subset %lu texture path is not NUL-terminated.", subsetTableIndex);
                LOG::flush();
                return false;
            }

            DistantSubset subset = {};
            subset.sphere = subsetRecord.sphere.toRuntime();
            subset.aabbMin = subsetRecord.aabb.minRuntime();
            subset.aabbMax = subsetRecord.aabb.maxRuntime();
            subset.hasAlpha = (subsetRecord.flags & 0x1u) != 0;
            subset.hasUVController = (subsetRecord.flags & 0x2u) != 0;
            subset.verts = runtimeVertexCount;
            subset.faces = runtimeTriangleCount;
            subset.farFaces = runtimeFarFaceCount;
            subset.veryFarFaces = runtimeVeryFarFaceCount;
            subset.horizonFootprint = subsetRecord.horizonFootprint;
            ++L.totalSubsets;
            L.totalVertices += subsetRecord.vertex_count;
            L.totalFaces += subsetRecord.triangle_count;
            L.totalFarFaces += farFaceCount;
            L.totalVeryFarFaces += veryFarFaceCount;
            L.totalVertexBytes += vertexBytes;
            L.totalIndexBytes += indexBytes;

            IDirect3DVertexBuffer9* vb = nullptr;
            IDirect3DIndexBuffer9* ib = nullptr;
            void* lockdata = nullptr;

            auto vbStart = DistantLoadInstrumentation::counter_now();
            HRESULT hr = device->CreateVertexBuffer(vertexUploadBytes, D3DUSAGE_WRITEONLY, 0, D3DPOOL_DEFAULT, &vb, 0);
            if (FAILED(hr)) {
                LOG::logline(
                    "!! Failed to create distant static vertex buffer: static=%lu subset=%lu verts=%d bytes=%u hr=0x%08lx",
                    L.staticIndex,
                    subsetTableIndex,
                    subset.verts,
                    vertexUploadBytes,
                    hr
                );
                return false;
            }
            hr = vb->Lock(0, 0, &lockdata, 0);
            if (FAILED(hr)) {
                LOG::logline(
                    "!! Failed to lock distant static vertex buffer: static=%lu subset=%lu verts=%d bytes=%u hr=0x%08lx",
                    L.staticIndex,
                    subsetTableIndex,
                    subset.verts,
                    vertexUploadBytes,
                    hr
                );
                vb->Release();
                return false;
            }
            if (!L.activeShard->mapping.copyRange(subsetRecord.vertex_offset, vertexBytes, lockdata)) {
                vb->Unlock();
                vb->Release();
                MappedFileUtil::LogMappingFailure("Distant statics: failed to map static shard vertex data", L.activeShard->mapping);
                LOG::flush();
                return false;
            }
            vb->Unlock();
            L.createVertexBuffersMs += DistantLoadInstrumentation::elapsed_ms(vbStart);

            auto ibStart = DistantLoadInstrumentation::counter_now();
            hr = device->CreateIndexBuffer(indexUploadBytes, D3DUSAGE_WRITEONLY, D3DFMT_INDEX16, D3DPOOL_DEFAULT, &ib, 0);
            if (FAILED(hr)) {
                LOG::logline(
                    "!! Failed to create distant static index buffer: static=%lu subset=%lu faces=%d bytes=%u hr=0x%08lx",
                    L.staticIndex,
                    subsetTableIndex,
                    subset.faces,
                    indexUploadBytes,
                    hr
                );
                vb->Release();
                return false;
            }
            hr = ib->Lock(0, 0, &lockdata, 0);
            if (FAILED(hr)) {
                LOG::logline(
                    "!! Failed to lock distant static index buffer: static=%lu subset=%lu faces=%d bytes=%u hr=0x%08lx",
                    L.staticIndex,
                    subsetTableIndex,
                    subset.faces,
                    indexUploadBytes,
                    hr
                );
                ib->Release();
                vb->Release();
                return false;
            }
            if (subsetRecord.component_count == 0) {
                if (!L.activeShard->mapping.copyRange(subsetRecord.index_offset, indexBytes, lockdata)) {
                    ib->Unlock();
                    ib->Release();
                    vb->Release();
                    MappedFileUtil::LogMappingFailure("Distant statics: failed to map static shard index data", L.activeShard->mapping);
                    LOG::flush();
                    return false;
                }
            } else {
                L.indexScratch.resize(static_cast<std::size_t>(indexBytes));
                if (!L.activeShard->mapping.copyRange(subsetRecord.index_offset, indexBytes, L.indexScratch.data())) {
                    ib->Unlock();
                    ib->Release();
                    vb->Release();
                    MappedFileUtil::LogMappingFailure("Distant statics: failed to map static shard index data", L.activeShard->mapping);
                    LOG::flush();
                    return false;
                }

                auto* output = static_cast<std::uint8_t*>(lockdata);
                auto copyTier = [&](int tier) {
                    for (std::uint32_t componentIndex = 0; componentIndex < subsetRecord.component_count; ++componentIndex) {
                        const auto& component = components[componentIndex];
                        if (classifyStaticComponent(component, Configuration.DL.FarStaticMinSize, Configuration.DL.VeryFarStaticMinSize) != tier) {
                            continue;
                        }
                        const std::size_t sourceOffset =
                            static_cast<std::size_t>(component.first_triangle) * 3u * StaticMeshesBin::IndexElementSize;
                        const std::size_t byteCount =
                            static_cast<std::size_t>(component.triangle_count) * 3u * StaticMeshesBin::IndexElementSize;
                        std::memcpy(output, L.indexScratch.data() + sourceOffset, byteCount);
                        output += byteCount;
                    }
                };
                copyTier(2);
                copyTier(1);
                copyTier(0);
            }
            ib->Unlock();
            L.createIndexBuffersMs += DistantLoadInstrumentation::elapsed_ms(ibStart);

            auto textureStart = DistantLoadInstrumentation::counter_now();
            IDirect3DTexture9* tex = BSA::loadTexture(device, reinterpret_cast<const char*>(texturePathBytesView));
            if (!tex) {
                LOG::logline("Cannot load texture %s", reinterpret_cast<const char*>(texturePathBytesView));
                L.errorTexture->AddRef();
                tex = L.errorTexture;
            }
            L.loadTexturesMs += DistantLoadInstrumentation::elapsed_ms(textureStart);

            subset.vbuffer = vb;
            subset.ibuffer = ib;
            subset.tex = tex;
            L.loadedMeshResources.push_back(MeshResources(vb, ib, tex));
            L.distantSubsets.push_back(subset);
            L.expectedGeometryOffset = nextGeometryOffset;
            L.expectedFirstComponentIndex += subsetRecord.component_count;

            ++L.subsetOffset;

            // Yield when the per-frame budget is spent; cursors resume this static
            // mid-way on the next tick (subsetOffset > 0 skips re-validation).
            if (DistantLoadInstrumentation::elapsed_ms(stepStart) >= static_cast<double>(budgetMs)) {
                L.parseTotalMs += DistantLoadInstrumentation::elapsed_ms(stepStart);
                return true;
            }
        }

            L.expectedFirstSubsetIndex += staticRecord.subset_count;
            L.distantStatics.push_back(L.runtimeStatic);
            ++L.staticIndex;
            L.subsetOffset = 0;
        }

        if (L.expectedFirstSubsetIndex != L.header.subset_count
            || L.expectedFirstComponentIndex != L.header.component_count
            || L.expectedGeometryOffset != L.geometryBlobEnd) {
            LOG::logline(
                "!! %s did not fully consume its declared subset, component, or geometry ranges.",
                L.activeShard->path.c_str()
            );
            LOG::flush();
            return false;
        }

        L.activeShard->mapping.releaseSlidingView();
        L.globalSubsetBase += L.header.subset_count;
        const std::uint32_t nextShard = L.shardIndex + 1;
        if (nextShard < StaticMeshShardCount) {
            L.activateShard(nextShard);
        } else {
            L.shardIndex = StaticMeshShardCount;
            L.activeShard = nullptr;
        }
    }

    phaseDone = true;
    L.parseTotalMs += DistantLoadInstrumentation::elapsed_ms(stepStart);
    return true;
}

// Phase completion (single slice): contiguity check, client vis groups, stream
// the shared-memory statics records into IPC vecs, kick initDistantStatics on
// the host, then commit the loaded resources to meshCollectionStatics and emit
// the load summary.
bool DistantLand::finishStaticsPhase() {
    StaticsLoader& L = *staticsLoader;

    if (L.skipPhase) {
        return L.skipResult;
    }

    if (L.distantStatics.size() != L.totalStaticCount
        || L.distantSubsets.size() != L.totalSubsetCount
        || L.globalSubsetBase != L.totalSubsetCount) {
        LOG::logline("!! Fixed static shards did not produce the preflighted global static/subset totals.");
        LOG::flush();
        return false;
    }

    // Dynamic vis groups are read client-side on both paths (cheap, bounded blob).
    if (!loadVisGroupsClient(L.h)) {
        return false;
    }

    // The server reopens usage.data itself, so close our stream handle now.
    CloseHandle(L.h);
    L.h = INVALID_HANDLE_VALUE;

    {
        DistantLoadInstrumentation::ScopedLoadTimer timer("statics.shared_memory_client_total");
        auto staticsId = IPC::InvalidVector;
        auto subsetsId = IPC::InvalidVector;
        {
            auto maybeStatics = ipcClient.allocVecBlocking<DistantStatic>(1, 500000, 1);
            if (!maybeStatics.has_value()) {
                return false;
            }

            auto maybeSubsets = ipcClient.allocVecBlocking<DistantSubset>(1, 500000, 1);
            if (!maybeSubsets.has_value()) {
                return false;
            }

            auto& statics = maybeStatics.value();
            auto& subsets = maybeSubsets.value();
            statics.reserve(static_cast<std::uint32_t>(L.distantStatics.size()));
            for (const auto& s : L.distantStatics) {
                statics.push_back(s);
            }
            subsets.reserve(static_cast<std::uint32_t>(L.distantSubsets.size()));
            for (const auto& s : L.distantSubsets) {
                subsets.push_back(s);
            }

            staticsId = statics.id();
            subsetsId = subsets.id();
            if (!ipcClient.initDistantStatics(
                staticsId,
                subsetsId,
                Configuration.DL.FarStaticMinSize,
                Configuration.DL.VeryFarStaticMinSize
            )) {
                return false;
            }

            // The host consumes these asynchronously. Keep the server allocations alive
            // until StaticsHostWait observes RPC completion.
            staticsHostVecId = staticsId;
            subsetsHostVecId = subsetsId;
        }
    }

    vector<IDirect3DTexture9*> atlasPages;
    for (const auto& mesh : L.loadedMeshResources) {
        if (mesh.tex && mesh.tex != L.errorTexture
            && std::find(atlasPages.begin(), atlasPages.end(), mesh.tex) == atlasPages.end()) {
            atlasPages.push_back(mesh.tex);
        }
    }

    // Commit (infallible): transfer ownership of the per-subset resources to
    // meshCollectionStatics so release() frees them; abort must not touch them now.
    if (L.errorTexture) {
        L.errorTexture->Release();
        L.errorTexture = nullptr;
    }
    meshCollectionStatics.insert(meshCollectionStatics.end(), L.loadedMeshResources.begin(), L.loadedMeshResources.end());
    L.loadedMeshResources.clear();

    DistantLoadInstrumentation::log_timing("static_meshes.parse_total", L.parseTotalMs);
    DistantLoadInstrumentation::log_timing("static_meshes.create_vertex_buffers", L.createVertexBuffersMs);
    DistantLoadInstrumentation::log_timing("static_meshes.create_index_buffers", L.createIndexBuffersMs);
    DistantLoadInstrumentation::log_timing("static_meshes.load_textures", L.loadTexturesMs);
    const std::uint64_t totalGeometryBytes = L.totalVertexBytes + L.totalIndexBytes;
    LOG::logline(
        "-- Distant load summary: static_meshes metadata_prefix_bytes=%llu geometry_window_bytes=%llu geometry_bytes_copied=%llu",
        static_cast<unsigned long long>(L.totalMetadataPrefixBytes),
        static_cast<unsigned long long>(StaticMeshesGeometryWindowBytes),
        static_cast<unsigned long long>(totalGeometryBytes)
    );
    LOG::logline(
        "-- Distant load summary: static_meshes statics=%lu subsets=%zu total_vertices=%zu total_faces=%zu total_geometry_bytes=%llu",
        L.totalStaticCount,
        L.totalSubsets,
        L.totalVertices,
        L.totalFaces,
        static_cast<unsigned long long>(totalGeometryBytes)
    );
    LOG::logline(
        "-- Distant load summary: static_meshes far_faces=%zu very_far_faces=%zu far_reduction=%.1f%% very_far_reduction=%.1f%%",
        L.totalFarFaces,
        L.totalVeryFarFaces,
        L.totalFaces != 0 ? 100.0 * static_cast<double>(L.totalFaces - L.totalFarFaces) / static_cast<double>(L.totalFaces) : 0.0,
        L.totalFaces != 0 ? 100.0 * static_cast<double>(L.totalFaces - L.totalVeryFarFaces) / static_cast<double>(L.totalFaces) : 0.0
    );

    LOG::logline("-- Distant static geometry memory use: %llu MB", static_cast<unsigned long long>(totalGeometryBytes / (1ull << 20)));
    std::uint64_t totalTextureBytes = 0;
    for (size_t pageIndex = 0; pageIndex < atlasPages.size(); ++pageIndex) {
        D3DSURFACE_DESC desc = {};
        UINT levelCount = 0;
        std::uint64_t bytes = 0;
        if (!inspectTextureFootprint(atlasPages[pageIndex], desc, levelCount, bytes)) {
            LOG::logline("!! Could not measure distant static texture page %zu", pageIndex);
            continue;
        }

        const char* formatName = d3dFormatName(desc.Format);
        LOG::logline(
            "-- Distant static texture page: page=%zu width=%lu height=%lu format=%s format_raw=0x%08lx mip_levels=%lu bytes=%llu",
            pageIndex,
            desc.Width,
            desc.Height,
            formatName ? formatName : "unknown",
            static_cast<unsigned long>(desc.Format),
            levelCount,
            static_cast<unsigned long long>(bytes)
        );
        totalTextureBytes += bytes;
    }
    LOG::logline(
        "-- Distant static texture memory use: pages=%zu bytes=%llu total_mb=%.2f",
        atlasPages.size(),
        static_cast<unsigned long long>(totalTextureBytes),
        static_cast<double>(totalTextureBytes) / static_cast<double>(1 << 20)
    );
    LOG::flush();

    DistantLandShare::hasCurrentWorldSpace = false;
    staticsUploaded = true;
    return true;
}

// Synchronous statics load: drives begin/step/finish to completion in one call.
// Used by the in-world renderer-restart path (uploadDistantLand); the menu pump
// drives the same phase functions incrementally instead.
bool DistantLand::initDistantStaticsClient() {
    DistantLoadInstrumentation::ScopedLoadTimer totalTimer("statics.total");

    if (!beginStaticsPhase()) {
        abortStaticsPhase();
        return false;
    }
    if (staticsLoader->skipPhase) {
        const bool ok = staticsLoader->skipResult;
        abortStaticsPhase();
        return ok && finishLandscapeUpload();
    }

    bool done = false;
    while (!done) {
        if (!stepStaticsPhase(std::numeric_limits<int>::max(), done)) {
            abortStaticsPhase();
            return false;
        }
    }

    // Collect the terrain result now that the statics upload has overlapped the host's build,
    // and before finishStaticsPhase issues the next RPC.
    if (!finishLandscapeUpload()) {
        abortStaticsPhase();
        return false;
    }

    const bool ok = finishStaticsPhase();
    abortStaticsPhase();   // success leaves the loader empty; this just drops it
    return ok;
}

bool DistantLand::loadVisGroupsClient(HANDLE h) {
    DistantLoadInstrumentation::ScopedLoadTimer timer("dynamic_vis.read_client");

    // Load dynamic vis groups
    DWORD dynamicVisGroupCount = 0;
    if (!DistantLoadInstrumentation::ReadExact(h, &dynamicVisGroupCount, sizeof(dynamicVisGroupCount), "dynamic_vis.group_count_client")) {
        return false;
    }
    dynamicVisGroups.clear();

    if (dynamicVisGroupCount > 0) {
        const size_t visGroupRecordSize = 130;
        size_t visDataSize = visGroupRecordSize * dynamicVisGroupCount;
        auto visData = std::make_unique<char[]>(visDataSize);
        if (!DistantLoadInstrumentation::ReadExact(h, visData.get(), visDataSize, "dynamic_vis.group_records_client")) {
            return false;
        }
        membuf_reader visReader(visData.get());

        // VisGroup indexes use a 1-based index, group 0 is reserved for testing
        dynamicVisGroups.resize(dynamicVisGroupCount + 1);

        for (size_t nVisGroup = 1; nVisGroup <= dynamicVisGroupCount; ++nVisGroup) {
            DynamicVisGroup& dvg = dynamicVisGroups[nVisGroup];
            visReader.read(&dvg.source, 1);
            dvg.enabled = true;
            dvg.gameObject = nullptr;

            char id[64];
            visReader.read(&id, sizeof(id));
            dvg.id = id;

            uint8_t rangeCount;
            visReader.read(&rangeCount, sizeof(rangeCount));

            DynamicVisGroup::Range ranges[8];
            visReader.read(&ranges, sizeof(ranges));
            dvg.ranges.assign(ranges, ranges + rangeCount);
        }

        visData.reset();
    }
    LOG::logline("-- Distant load summary: dynamic_vis.client_group_count=%lu", dynamicVisGroupCount);
    return true;
}

// Arm the frame-budgeted upload pump at startup (createScene), before the menu.
namespace DistantLoaders {

float staticsProgressRatio() {
    if (!DistantLand::staticsLoader || DistantLand::staticsLoader->totalStaticCount == 0) {
        return 0.0f;
    }

    const float completed = static_cast<float>(DistantLand::staticsLoader->distantStatics.size());
    const float total = static_cast<float>(DistantLand::staticsLoader->totalStaticCount);
    return std::min(completed / total, 1.0f);
}

bool queryStaticsSkipResult(bool& result) {
    if (!DistantLand::staticsLoader || !DistantLand::staticsLoader->skipPhase) {
        return false;
    }

    result = DistantLand::staticsLoader->skipResult;
    return true;
}

void releaseStaticsResources() {
    for (auto& mesh : meshCollectionStatics) {
        if (mesh.vb) { mesh.vb->Release(); }
        if (mesh.ib) { mesh.ib->Release(); }
        if (mesh.tex) { mesh.tex->Release(); }
    }
    meshCollectionStatics.clear();
}

}
