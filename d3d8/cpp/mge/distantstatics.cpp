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
#include <condition_variable>
#include <cstdint>
#include <cstdio>
#include <deque>
#include <limits>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <unordered_map>
#include <vector>

using std::vector;

namespace {
    struct IndexCopySpan {
        std::uint32_t sourceOffset;
        std::uint32_t byteCount;
    };

    enum class StaticResourceState : std::uint8_t {
        Unloaded,
        IoQueued,
        ReadyForGpu,
        CommitPending,
        Resident,
        EvictQueued,
        RemovalInFlight,
        Unavailable,
    };

    struct StaticResource {
        IDirect3DVertexBuffer9* vb;
        IDirect3DIndexBuffer9* ib;
        IDirect3DTexture9* tex;
        std::uint32_t resourceId = 0;
        std::uint32_t shardId = 0;
        std::uint64_t vertexOffset = 0;
        std::uint64_t indexOffset = 0;
        std::uint32_t vertexBytes = 0;
        std::uint32_t indexBytes = 0;
        bool merged = false;
        bool streamed = false;   // published without buffers; admission owns its lifetime
        bool readmitRequested = false;
        StaticResourceState state = StaticResourceState::Resident;
        std::uint32_t planEpoch = 0;
        std::vector<IndexCopySpan> indexCopyPlan;
        std::vector<D3DXVECTOR4> palette;

        StaticResource(IDirect3DVertexBuffer9* _vb, IDirect3DIndexBuffer9* _ib, IDirect3DTexture9* _tex)
            : vb(_vb), ib(_ib), tex(_tex) {}

        std::uint64_t geometryBytes() const {
            return static_cast<std::uint64_t>(vertexBytes) + indexBytes;
        }
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

    vector<StaticResource> staticResourceCatalog;

    // Per-subset UV-bound palettes, keyed on the subset's vertex buffer.
    //
    // The vertex buffer is a valid subset identity because stepStaticsPhase creates exactly one
    // VB per subset and never shares or recreates one within a device session; RenderMesh already
    // carries that pointer, so no IPC field is needed. If a second consumer ever needs explicit
    // subset identity, add subset_index to RenderMesh then.
    StaticUvBoundPaletteMap staticUvBoundPalettes;

    constexpr std::uint64_t StaticMeshesGeometryWindowBytes = 64ull * 1024ull * 1024ull;
    constexpr std::uint32_t StaticMeshShardCount = MGE_STATIC_MESH_SHARD_COUNT;

    // Provisional calibration inputs, not contracts (plan section "Spatial planner with no
    // cell-change burst"). The caller supplies the per-tick byte/time/record budgets.
    constexpr std::uint64_t kReadyQueueLimitBytes = 20ull * 1024 * 1024;
    constexpr std::uint32_t kPlannerMaxCells = 64;
    constexpr std::uint32_t kPlannerMaxResources = 64;

    std::string staticMeshShardPath(std::uint32_t shardId);

    // Bounded background reader for capped admission. Owns nothing D3D: it produces already
    // reordered vertex/index bytes in heap buffers that the main thread uploads from.
    //
    // The startup loader's memory-mapped shard views are deliberately not reused here. Retaining
    // 128 geometry windows would exhaust the 32-bit address space, so this worker keeps a small
    // handle cache and issues positional ReadFile calls instead.
    class ResidencyIoWorker {
    public:
        struct Request {
            std::uint32_t resourceId = 0;
            std::uint32_t planEpoch = 0;
            std::uint32_t shardId = 0;
            std::uint64_t vertexOffset = 0;
            std::uint64_t indexOffset = 0;
            std::uint32_t vertexBytes = 0;
            std::uint32_t indexBytes = 0;
            std::vector<IndexCopySpan> indexCopyPlan;
        };

        struct Result {
            std::uint32_t resourceId = 0;
            std::uint32_t planEpoch = 0;
            bool ok = false;
            std::vector<std::uint8_t> vertexData;
            std::vector<std::uint8_t> indexData;
        };

        ~ResidencyIoWorker() { stop(); }

        void start() {
            if (thread.joinable()) {
                return;
            }
            stopping = false;
            thread = std::thread([this] { run(); });
        }

        void stop() {
            if (!thread.joinable()) {
                return;
            }
            {
                std::lock_guard<std::mutex> lock(mutex);
                stopping = true;
                pending.clear();
            }
            wake.notify_all();
            thread.join();
            std::lock_guard<std::mutex> lock(mutex);
            ready.clear();
            readyBytes = 0;
        }

        bool running() const { return thread.joinable(); }

        void submit(Request&& request) {
            {
                std::lock_guard<std::mutex> lock(mutex);
                pending.push_back(std::move(request));
            }
            wake.notify_one();
        }

        // Drops queued work for superseded plan epochs. In-flight output is discarded on
        // collection instead, so the worker never has to be interrupted mid-read.
        void cancelOlderEpochs(std::uint32_t epoch, std::vector<std::uint32_t>& cancelled) {
            std::lock_guard<std::mutex> lock(mutex);
            for (auto it = pending.begin(); it != pending.end();) {
                if (it->planEpoch != epoch) {
                    cancelled.push_back(it->resourceId);
                    it = pending.erase(it);
                } else {
                    ++it;
                }
            }
            for (auto it = ready.begin(); it != ready.end();) {
                if (it->planEpoch != epoch) {
                    cancelled.push_back(it->resourceId);
                    readyBytes -= std::min<std::uint64_t>(readyBytes, it->vertexData.size() + it->indexData.size());
                    it = ready.erase(it);
                } else {
                    ++it;
                }
            }
        }

        bool tryCollect(Result& out) {
            std::lock_guard<std::mutex> lock(mutex);
            if (ready.empty()) {
                return false;
            }
            out = std::move(ready.front());
            ready.pop_front();
            readyBytes -= std::min<std::uint64_t>(readyBytes, out.vertexData.size() + out.indexData.size());
            return true;
        }

        std::uint64_t queuedBytes() {
            std::lock_guard<std::mutex> lock(mutex);
            return readyBytes;
        }

        bool idle() {
            std::lock_guard<std::mutex> lock(mutex);
            return pending.empty() && ready.empty() && !working;
        }

    private:
        void run() {
            for (;;) {
                Request request;
                {
                    std::unique_lock<std::mutex> lock(mutex);
                    wake.wait(lock, [this] { return stopping || !pending.empty(); });
                    if (stopping) {
                        break;
                    }
                    request = std::move(pending.front());
                    pending.pop_front();
                    working = true;
                }

                Result result;
                result.resourceId = request.resourceId;
                result.planEpoch = request.planEpoch;
                result.ok = fulfil(request, result);

                {
                    std::lock_guard<std::mutex> lock(mutex);
                    readyBytes += result.vertexData.size() + result.indexData.size();
                    ready.push_back(std::move(result));
                    working = false;
                }
            }
            closeHandles();
        }

        HANDLE shardHandle(std::uint32_t shardId) {
            for (auto& entry : handleCache) {
                if (entry.handle != INVALID_HANDLE_VALUE && entry.shardId == shardId) {
                    return entry.handle;
                }
            }
            const std::string path = staticMeshShardPath(shardId);
            HANDLE handle = CreateFile(path.c_str(), GENERIC_READ, FILE_SHARE_READ, 0, OPEN_EXISTING, 0, 0);
            if (handle == INVALID_HANDLE_VALUE) {
                return INVALID_HANDLE_VALUE;
            }
            if (handleCache[handleCursor].handle != INVALID_HANDLE_VALUE) {
                CloseHandle(handleCache[handleCursor].handle);
            }
            handleCache[handleCursor] = { shardId, handle };
            handleCursor = (handleCursor + 1) % handleCache.size();
            return handle;
        }

        void closeHandles() {
            for (auto& entry : handleCache) {
                if (entry.handle != INVALID_HANDLE_VALUE) {
                    CloseHandle(entry.handle);
                    entry.handle = INVALID_HANDLE_VALUE;
                }
            }
        }

        static bool readAt(HANDLE handle, std::uint64_t offset, void* destination, std::uint32_t bytes) {
            auto* output = static_cast<std::uint8_t*>(destination);
            std::uint32_t remaining = bytes;
            while (remaining != 0) {
                OVERLAPPED overlapped = {};
                overlapped.Offset = static_cast<DWORD>(offset & 0xFFFFFFFFull);
                overlapped.OffsetHigh = static_cast<DWORD>(offset >> 32);
                DWORD read = 0;
                if (!ReadFile(handle, output, remaining, &read, &overlapped) || read == 0) {
                    return false;
                }
                output += read;
                offset += read;
                remaining -= read;
            }
            return true;
        }

        bool fulfil(const Request& request, Result& result) {
            HANDLE handle = shardHandle(request.shardId);
            if (handle == INVALID_HANDLE_VALUE) {
                return false;
            }

            result.vertexData.resize(request.vertexBytes);
            if (!readAt(handle, request.vertexOffset, result.vertexData.data(), request.vertexBytes)) {
                return false;
            }

            result.indexData.resize(request.indexBytes);
            if (request.indexCopyPlan.empty()) {
                return readAt(handle, request.indexOffset, result.indexData.data(), request.indexBytes);
            }

            // Merged subsets are reordered into very-far/far/near tier runs, matching the
            // startup path's copyTier(2)/copyTier(1)/copyTier(0) sequence.
            scratch.resize(request.indexBytes);
            if (!readAt(handle, request.indexOffset, scratch.data(), request.indexBytes)) {
                return false;
            }
            std::size_t written = 0;
            for (const auto& span : request.indexCopyPlan) {
                if (static_cast<std::uint64_t>(span.sourceOffset) + span.byteCount > request.indexBytes
                    || written + span.byteCount > request.indexBytes) {
                    return false;
                }
                std::memcpy(result.indexData.data() + written, scratch.data() + span.sourceOffset, span.byteCount);
                written += span.byteCount;
            }
            return written == request.indexBytes;
        }

        struct CachedHandle {
            std::uint32_t shardId = 0;
            HANDLE handle = INVALID_HANDLE_VALUE;
        };

        std::thread thread;
        std::mutex mutex;
        std::condition_variable wake;
        std::deque<Request> pending;
        std::deque<Result> ready;
        std::vector<std::uint8_t> scratch;
        std::array<CachedHandle, 4> handleCache = {};
        std::size_t handleCursor = 0;
        std::uint64_t readyBytes = 0;
        bool stopping = false;
        bool working = false;
    };

    ResidencyIoWorker residencyIo;

    // Client-authoritative byte ledger. Covers every merged resource that owns or has reserved
    // GPU bytes: io-queued, ready-for-gpu, commit-pending, resident, evict-queued and
    // removal-in-flight. Decremented only after Release, never on eviction request.
    std::uint64_t logicalReservedMergedBytes = 0;
    // Live merged VB/IB bytes only. This deliberately excludes queued and prepared data: DXVK's
    // memoryUsed rises with this value when buffers are created and falls when they are released.
    std::uint64_t logicalGpuMergedBytes = 0;
    std::uint64_t mergedGeometryBytesTotal = 0;
    std::uint32_t residencyPlanEpoch = 0;
    bool residencyIdle = true;              // no cap binds: the planner is never asked to run
    bool residencyFrozen = false;           // an unacknowledged removal poisoned the session
    std::vector<std::uint32_t> residencyEvictQueue;
    std::vector<std::uint32_t> residencyRemovalInFlight;
    // Resources holding live buffers whose commit RPC has not been acknowledged yet. Tracked
    // explicitly so the per-frame tick never scans the whole catalog.
    std::vector<std::uint32_t> residencyCommitPending;

    struct FullDrainState {
        bool active = false;
        std::size_t cursor = 0;
        std::uint32_t shardId = std::numeric_limits<std::uint32_t>::max();
        std::unique_ptr<ReadOnlyMappedFile> mapping;
        std::vector<std::uint8_t> indexScratch;

        void reset() {
            active = false;
            cursor = 0;
            shardId = std::numeric_limits<std::uint32_t>::max();
            mapping.reset();
            indexScratch.clear();
        }
    };
    FullDrainState fullDrain;

    // Permanent low-cost session counters, summarised at teardown.
    struct ResidencyStats {
        std::uint64_t admittedBytes = 0;
        std::uint64_t evictedBytes = 0;
        std::uint32_t admittedCount = 0;
        std::uint32_t evictedCount = 0;
        std::uint32_t unavailableCount = 0;
        std::uint32_t cancelledCount = 0;
        std::uint32_t nonCompleteRemovals = 0;
        std::uint32_t paletteMisses = 0;
        std::uint64_t peakReservedBytes = 0;
        std::uint64_t peakGpuBytes = 0;
    };
    ResidencyStats residencyStats;

    enum class ResidencyTransitionTrigger {
        Bootstrap,
        Cell,
        Load,
    };

    enum class ResidencySampleSource {
        Pump,
        Present,
        Resolve,
    };

    struct ResidencyTransitionStats {
        bool active = false;
        ResidencyTransitionTrigger trigger = ResidencyTransitionTrigger::Bootstrap;
        ResidencySampleSource sampleSource = ResidencySampleSource::Pump;
        std::uint32_t epoch = 0;
        D3DXVECTOR3 destination = {};
        std::uint64_t samplePresent = 0;
        bool firstAdmissionCommitted = false;
        std::uint32_t firstAdmissionResource = 0;
        std::uint64_t firstAdmissionPresent = 0;
        bool usedStage0Eviction = false;
        bool usedPresentEviction = false;
        double maxPlannerMs = 0.0;
        double maxUploadMs = 0.0;
        std::uint32_t oversizeCount = 0;
        std::uint32_t largestOversizeResource = 0;
        std::uint64_t largestOversizeBytes = 0;
    };
    ResidencyTransitionStats residencyTransition;
    std::uint64_t residencyPresentSerial = 0;
    bool liveLoadTransitionPending = false;
    bool liveLoadDestinationValid = false;
    D3DXVECTOR3 liveLoadDestination = {};
    std::uint64_t liveLoadSamplePresent = 0;

    const char* transitionTriggerName(ResidencyTransitionTrigger trigger) {
        switch (trigger) {
        case ResidencyTransitionTrigger::Bootstrap: return "bootstrap";
        case ResidencyTransitionTrigger::Cell: return "cell";
        case ResidencyTransitionTrigger::Load: return "load";
        default: return "unknown";
        }
    }

    const char* sampleSourceName(ResidencySampleSource source) {
        switch (source) {
        case ResidencySampleSource::Pump: return "pump";
        case ResidencySampleSource::Present: return "present";
        case ResidencySampleSource::Resolve: return "resolve";
        default: return "unknown";
        }
    }

    void logResidencyTransitionSummary(const char* reason) {
        if (!residencyTransition.active) {
            return;
        }
        const std::uint64_t leadFrames = residencyTransition.firstAdmissionCommitted
            ? residencyTransition.firstAdmissionPresent - residencyTransition.samplePresent
            : 0;
        LOG::logline(
            "-- Distant static transition summary: epoch=%lu trigger=%s reason=%s sample_source=%s "
            "sample_present=%llu destination=(%.2f,%.2f,%.2f) first_admit=%d first_admit_resource=%lu "
            "lead_frames=%llu evict_stage0=%d evict_present=%d planner_max=%.2fms upload_max=%.2fms "
            "oversize_count=%lu oversize_resource=%lu oversize_bytes=%llu",
            residencyTransition.epoch,
            transitionTriggerName(residencyTransition.trigger),
            reason,
            sampleSourceName(residencyTransition.sampleSource),
            static_cast<unsigned long long>(residencyTransition.samplePresent),
            residencyTransition.destination.x,
            residencyTransition.destination.y,
            residencyTransition.destination.z,
            residencyTransition.firstAdmissionCommitted ? 1 : 0,
            residencyTransition.firstAdmissionResource,
            static_cast<unsigned long long>(leadFrames),
            residencyTransition.usedStage0Eviction ? 1 : 0,
            residencyTransition.usedPresentEviction ? 1 : 0,
            residencyTransition.maxPlannerMs,
            residencyTransition.maxUploadMs,
            residencyTransition.oversizeCount,
            residencyTransition.largestOversizeResource,
            static_cast<unsigned long long>(residencyTransition.largestOversizeBytes)
        );
    }

    void resetResidencyTransitionRuntime() {
        residencyTransition = ResidencyTransitionStats();
        residencyPresentSerial = 0;
        liveLoadTransitionPending = false;
        liveLoadDestinationValid = false;
        liveLoadDestination = D3DXVECTOR3();
        liveLoadSamplePresent = 0;
    }

    void noteTransitionAdmission(const StaticResource& resource) {
        if (!residencyTransition.active || resource.planEpoch != residencyTransition.epoch ||
            residencyTransition.firstAdmissionCommitted) {
            return;
        }
        residencyTransition.firstAdmissionCommitted = true;
        residencyTransition.firstAdmissionResource = resource.resourceId;
        residencyTransition.firstAdmissionPresent = residencyPresentSerial;
    }

    void noteTransitionOversize(const StaticResource& resource) {
        if (!residencyTransition.active || resource.planEpoch != residencyTransition.epoch) {
            return;
        }
        ++residencyTransition.oversizeCount;
        if (resource.geometryBytes() > residencyTransition.largestOversizeBytes) {
            residencyTransition.largestOversizeResource = resource.resourceId;
            residencyTransition.largestOversizeBytes = resource.geometryBytes();
        }
    }

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
    D3DDECL_END()
};


}

struct StaticShardView {
    ReadOnlyMappedFile mapping{ StaticMeshesGeometryWindowBytes };
    StaticMeshesBin::StaticMeshesFileHeader header = {};
    const StaticMeshesBin::StaticRecord* staticRecords = nullptr;
    const StaticMeshesBin::SubsetRecord* subsetRecords = nullptr;
    const StaticMeshesBin::ComponentRecord* componentRecords = nullptr;
    const StaticMeshesBin::PaletteRecord* paletteRecords = nullptr;
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
    const StaticMeshesBin::PaletteRecord* paletteRecords = nullptr;
    IDirect3DTexture9* errorTexture = nullptr;

    std::vector<DistantStatic> distantStatics;
    std::vector<DistantSubset> distantSubsets;
    std::vector<StaticResource> loadedResources;
    std::vector<std::uint8_t> indexScratch;

    // Resumable cursors.
    std::uint32_t shardIndex = 0;
    std::uint32_t staticIndex = 0;
    std::uint32_t globalSubsetBase = 0;
    std::uint32_t subsetOffset = 0;          // subset within the current static
    DistantStatic runtimeStatic = {};        // in-progress static (valid while subsetOffset > 0)
    std::uint32_t expectedFirstSubsetIndex = 0;
    std::uint32_t expectedFirstComponentIndex = 0;
    std::uint32_t expectedFirstPaletteIndex = 0;
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
        paletteRecords = activeShard->paletteRecords;
        staticIndex = 0;
        subsetOffset = 0;
        expectedFirstSubsetIndex = 0;
        expectedFirstComponentIndex = 0;
        expectedFirstPaletteIndex = 0;
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
    // Of the totals above, the share published without buffers for the residency planner to
    // stream later. Everything else was uploaded here and is resident for the device session.
    std::uint64_t streamedGeometryBytes = 0;
    bool currentStaticMerged = false;
    size_t mergedStatics = 0;
    size_t mergedSubsets = 0;
    size_t mergedVertices = 0;
    size_t mergedFaces = 0;
    std::uint64_t mergedVertexBytes = 0;
    std::uint64_t mergedIndexBytes = 0;

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
    for (auto it = L.loadedResources.rbegin(); it != L.loadedResources.rend(); ++it) {
        if (it->tex) { it->tex->Release(); }
        if (it->ib) { it->ib->Release(); }
        if (it->vb) { it->vb->Release(); }
    }
    L.loadedResources.clear();
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
        // The palette table sits before geometry_blob_offset, so the prefix mapped above already
        // covers it.
        shard->paletteRecords = reinterpret_cast<const StaticMeshesBin::PaletteRecord*>(
            shard->mapping.getPersistentRange(shard->header.palette_table_offset, shard->header.palette_table_size)
        );
        if (!shard->staticRecords || !shard->subsetRecords || !shard->componentRecords || !shard->paletteRecords) {
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
    L.loadedResources.reserve(L.totalSubsetCount);

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
            const bool subsetIsMerged = subsetRecord.component_count != 0;
            if (L.subsetOffset == 0) {
                L.currentStaticMerged = subsetIsMerged;
            } else if (L.currentStaticMerged != subsetIsMerged) {
                LOG::logline(
                    "!! static_meshes static %lu mixes componentless and merged-provenance subsets.",
                    L.staticIndex
                );
                LOG::flush();
                return false;
            }

            if (subsetRecord.first_palette_index != L.expectedFirstPaletteIndex) {
                LOG::logline(
                    "!! static_meshes subset %lu starts at palette entry %lu, expected %lu for contiguous palette ownership.",
                    subsetTableIndex,
                    subsetRecord.first_palette_index,
                    L.expectedFirstPaletteIndex
                );
                LOG::flush();
                return false;
            }
            // These three predicates are the whole palette contract. The first is not implied by
            // the second: these are unsigned, so with first_palette_index > palette_count the
            // subtraction would wrap and the second would pass. Nothing else is checked here --
            // no rect finiteness, no tiling rule, no per-vertex ordinal scan. The writer already
            // hard-fails on an over-cap palette, publication is complete-or-absent, and a
            // wrong-but-in-range ordinal shows as a wrong atlas tile rather than a crash.
            if (subsetRecord.first_palette_index > L.header.palette_count
                || subsetRecord.palette_count > L.header.palette_count - subsetRecord.first_palette_index
                || subsetRecord.palette_count > StaticMeshesBin::MaxPaletteEntries) {
                LOG::logline(
                    "!! static_meshes subset %lu palette range %lu+%lu is outside palette_count=%lu or exceeds the cap of %lu.",
                    subsetTableIndex,
                    subsetRecord.first_palette_index,
                    subsetRecord.palette_count,
                    L.header.palette_count,
                    StaticMeshesBin::MaxPaletteEntries
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
            subset.geometryBytes = vertexBytes + indexBytes;
            subset.resourceId = static_cast<std::uint32_t>(L.distantSubsets.size());
            subset.resourceFlags = L.currentStaticMerged ? DISTANT_SUBSET_STREAMABLE_MERGED : 0;
            // Every merged subset is published without buffers. After fixed resources exist,
            // cap selection chooses either the ordered full drain or the capped radial
            // bootstrap. Textures stay resident in both schedules.
            const bool streamThisSubset = L.currentStaticMerged;

            ++L.totalSubsets;
            L.totalVertices += subsetRecord.vertex_count;
            L.totalFaces += subsetRecord.triangle_count;
            L.totalFarFaces += farFaceCount;
            L.totalVeryFarFaces += veryFarFaceCount;
            L.totalVertexBytes += vertexBytes;
            L.totalIndexBytes += indexBytes;
            if (streamThisSubset) {
                L.streamedGeometryBytes += static_cast<std::uint64_t>(vertexBytes) + indexBytes;
            }
            if (L.currentStaticMerged) {
                ++L.mergedSubsets;
                L.mergedVertices += subsetRecord.vertex_count;
                L.mergedFaces += subsetRecord.triangle_count;
                L.mergedVertexBytes += vertexBytes;
                L.mergedIndexBytes += indexBytes;
            }

            IDirect3DVertexBuffer9* vb = nullptr;
            IDirect3DIndexBuffer9* ib = nullptr;
            void* lockdata = nullptr;

            // Startup geometry upload from the shard's mapped window. Skipped for a merged
            // subset: the selected residency schedule reads those bytes after fixed uploads.
            auto uploadGeometryFromMapping = [&]() -> bool {
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
                return true;
            };
            if (!streamThisSubset && !uploadGeometryFromMapping()) {
                return false;
            }

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
            std::vector<D3DXVECTOR4> palette;
            palette.reserve(subsetRecord.palette_count);
            const auto* paletteEntries = L.paletteRecords + subsetRecord.first_palette_index;
            for (std::uint32_t paletteIndex = 0; paletteIndex < subsetRecord.palette_count; ++paletteIndex) {
                const auto& entry = paletteEntries[paletteIndex];
                palette.push_back(D3DXVECTOR4(entry.bound[0], entry.bound[1], entry.bound[2], entry.bound[3]));
            }
            StaticResource resource(vb, ib, tex);
            resource.resourceId = subset.resourceId;
            resource.shardId = L.shardIndex;
            resource.vertexOffset = subsetRecord.vertex_offset;
            resource.indexOffset = subsetRecord.index_offset;
            resource.vertexBytes = vertexUploadBytes;
            resource.indexBytes = indexUploadBytes;
            resource.merged = L.currentStaticMerged;
            resource.streamed = streamThisSubset;
            resource.state = streamThisSubset ? StaticResourceState::Unloaded : StaticResourceState::Resident;
            resource.palette = std::move(palette);
            if (resource.merged) {
                for (int tier = 2; tier >= 0; --tier) {
                    for (std::uint32_t componentIndex = 0; componentIndex < subsetRecord.component_count; ++componentIndex) {
                        const auto& component = components[componentIndex];
                        if (classifyStaticComponent(component, Configuration.DL.FarStaticMinSize, Configuration.DL.VeryFarStaticMinSize) != tier) {
                            continue;
                        }
                        resource.indexCopyPlan.push_back(IndexCopySpan {
                            component.first_triangle * 3u * StaticMeshesBin::IndexElementSize,
                            component.triangle_count * 3u * StaticMeshesBin::IndexElementSize,
                        });
                    }
                }
            }
            L.loadedResources.push_back(std::move(resource));

            L.distantSubsets.push_back(subset);
            L.expectedGeometryOffset = nextGeometryOffset;
            L.expectedFirstComponentIndex += subsetRecord.component_count;
            L.expectedFirstPaletteIndex += subsetRecord.palette_count;

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
            if (L.currentStaticMerged) {
                ++L.mergedStatics;
            }
            ++L.staticIndex;
            L.subsetOffset = 0;
        }

        if (L.expectedFirstSubsetIndex != L.header.subset_count
            || L.expectedFirstComponentIndex != L.header.component_count
            || L.expectedFirstPaletteIndex != L.header.palette_count
            || L.expectedGeometryOffset != L.geometryBlobEnd) {
            LOG::logline(
                "!! %s did not fully consume its declared subset, component, palette, or geometry ranges.",
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
    for (const auto& mesh : L.loadedResources) {
        if (mesh.tex && mesh.tex != L.errorTexture
            && std::find(atlasPages.begin(), atlasPages.end(), mesh.tex) == atlasPages.end()) {
            atlasPages.push_back(mesh.tex);
        }
    }

    // Commit (infallible): transfer ownership of the per-subset resources to
    // staticResourceCatalog so release() frees them; abort must not touch them now.
    if (L.errorTexture) {
        L.errorTexture->Release();
        L.errorTexture = nullptr;
    }
    staticUvBoundPalettes.reserve(staticUvBoundPalettes.size() + L.loadedResources.size());
    for (auto& resource : L.loadedResources) {
        // A streamed resource has no VB yet; its palette is inserted at admission and erased
        // before the buffer is released, keeping the map's keys and the live buffers paired.
        if (resource.vb) {
            staticUvBoundPalettes.emplace(resource.vb, resource.palette);
        }
        if (resource.merged && resource.state == StaticResourceState::Resident) {
            logicalReservedMergedBytes += resource.geometryBytes();
        }
    }
    residencyStats.peakReservedBytes = std::max(residencyStats.peakReservedBytes, logicalReservedMergedBytes);
    staticResourceCatalog.insert(
        staticResourceCatalog.end(),
        std::make_move_iterator(L.loadedResources.begin()),
        std::make_move_iterator(L.loadedResources.end())
    );
    L.loadedResources.clear();

    DistantLoadInstrumentation::log_timing("static_meshes.parse_total", L.parseTotalMs);
    DistantLoadInstrumentation::log_timing("static_meshes.create_vertex_buffers", L.createVertexBuffersMs);
    DistantLoadInstrumentation::log_timing("static_meshes.create_index_buffers", L.createIndexBuffersMs);
    DistantLoadInstrumentation::log_timing("static_meshes.load_textures", L.loadTexturesMs);
    const std::uint64_t totalGeometryBytes = L.totalVertexBytes + L.totalIndexBytes;
    const std::uint64_t uploadedGeometryBytes = totalGeometryBytes - L.streamedGeometryBytes;
    LOG::logline(
        "-- Distant load summary: static_meshes metadata_prefix_bytes=%llu geometry_window_bytes=%llu "
        "geometry_bytes_copied=%llu geometry_bytes_deferred=%llu",
        static_cast<unsigned long long>(L.totalMetadataPrefixBytes),
        static_cast<unsigned long long>(StaticMeshesGeometryWindowBytes),
        static_cast<unsigned long long>(uploadedGeometryBytes),
        static_cast<unsigned long long>(L.streamedGeometryBytes)
    );
    LOG::logline(
        "-- Distant load summary: static_meshes statics=%lu subsets=%zu total_vertices=%zu total_faces=%zu total_geometry_bytes=%llu",
        L.totalStaticCount,
        L.totalSubsets,
        L.totalVertices,
        L.totalFaces,
        static_cast<unsigned long long>(totalGeometryBytes)
    );
    const std::uint64_t mergedGeometryBytes = L.mergedVertexBytes + L.mergedIndexBytes;
    mergedGeometryBytesTotal = mergedGeometryBytes;
    LOG::logline(
        "-- Distant load summary: static_meshes ordinary_statics=%zu merged_statics=%zu merged_subsets=%zu merged_vertices=%zu merged_faces=%zu ordinary_geometry_bytes=%llu merged_geometry_bytes=%llu",
        static_cast<std::size_t>(L.totalStaticCount) - L.mergedStatics,
        L.mergedStatics,
        L.mergedSubsets,
        L.mergedVertices,
        L.mergedFaces,
        static_cast<unsigned long long>(totalGeometryBytes - mergedGeometryBytes),
        static_cast<unsigned long long>(mergedGeometryBytes)
    );
    LOG::logline(
        "-- Distant load summary: static_meshes far_faces=%zu very_far_faces=%zu far_reduction=%.1f%% very_far_reduction=%.1f%%",
        L.totalFarFaces,
        L.totalVeryFarFaces,
        L.totalFaces != 0 ? 100.0 * static_cast<double>(L.totalFaces - L.totalFarFaces) / static_cast<double>(L.totalFaces) : 0.0,
        L.totalFaces != 0 ? 100.0 * static_cast<double>(L.totalFaces - L.totalVeryFarFaces) / static_cast<double>(L.totalFaces) : 0.0
    );

    // Only ordinary geometry is in VRAM at this point. The selected residency schedule runs
    // after grass and host initialization; fitting sessions log their completed full drain.
    LOG::logline(
        "-- Distant static geometry memory use: %llu MB uploaded (%llu MB deferred to streaming, %llu MB total)",
        static_cast<unsigned long long>(uploadedGeometryBytes / (1ull << 20)),
        static_cast<unsigned long long>(L.streamedGeometryBytes / (1ull << 20)),
        static_cast<unsigned long long>(totalGeometryBytes / (1ull << 20))
    );
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

            // The 64-byte field is only NUL-terminated when the name is shorter than
            // the field, so bound the assignment instead of reading past the buffer.
            char id[64];
            visReader.read(&id, sizeof(id));
            dvg.id.assign(id, std::find(std::begin(id), std::end(id), '\0'));

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

void logResidencySummary();

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

const StaticUvBoundPaletteMap& staticUvBoundPaletteMap() {
    return staticUvBoundPalettes;
}

void releaseStaticsResources() {
    // Join the reader before its catalog and shard handles go away.
    residencyIo.stop();
    logResidencyTransitionSummary("teardown");
    DistantLoaders::logResidencySummary();

    for (auto& mesh : staticResourceCatalog) {
        if (mesh.vb) { mesh.vb->Release(); }
        if (mesh.ib) { mesh.ib->Release(); }
        if (mesh.tex) { mesh.tex->Release(); }
    }
    staticResourceCatalog.clear();
    // Keyed on the vertex buffers just released; the keys are dangling from here on.
    staticUvBoundPalettes.clear();

    logicalReservedMergedBytes = 0;
    logicalGpuMergedBytes = 0;
    mergedGeometryBytesTotal = 0;
    residencyPlanEpoch = 0;
    residencyIdle = true;
    residencyFrozen = false;
    residencyEvictQueue.clear();
    residencyRemovalInFlight.clear();
    residencyCommitPending.clear();
    fullDrain.reset();
    residencyStats = ResidencyStats();
    resetResidencyTransitionRuntime();
}

}

//-----------------------------------------------------------------------------
// Merged-static residency runtime.
//
// The client is the sole byte-ledger authority: the host tracks spatial priority and committed
// resident state, but never a byte total or an admission reservation. Every D3D call below runs
// on Morrowind's main thread; the reader thread only produces bytes.

namespace {

std::uint64_t activeMergedCapBytes() {
    return DistantLand::mergedStreamingCapBytes;
}

std::uint64_t availableMergedBytes() {
    const std::uint64_t cap = activeMergedCapBytes();
    return cap > logicalReservedMergedBytes ? cap - logicalReservedMergedBytes : 0;
}

std::uint64_t mergedCapDebtBytes() {
    const std::uint64_t cap = activeMergedCapBytes();
    return logicalReservedMergedBytes > cap ? logicalReservedMergedBytes - cap : 0;
}

StaticResource* findResource(std::uint32_t resourceId) {
    if (resourceId >= staticResourceCatalog.size()) {
        return nullptr;
    }
    // The catalog is filled in global subset order, so the index is the resource id. Verify it
    // rather than trusting a host-supplied index into a client-owned array.
    StaticResource& resource = staticResourceCatalog[resourceId];
    return resource.resourceId == resourceId ? &resource : nullptr;
}

// Submits one acknowledged batch of state transitions. Returns false when the RPC did not
// complete, in which case the caller must not release anything.
bool commitResidency(const std::vector<IPC::ResidencyCommit>& commits) {
    if (commits.empty()) {
        return true;
    }
    if (DistantLand::residencyCommitSharedId == IPC::InvalidVector) {
        return false;
    }

    DistantLand::residencyCommitShared.clear();
    for (const auto& commit : commits) {
        if (!DistantLand::residencyCommitShared.push_back(commit)) {
            return false;
        }
    }
    if (!DistantLand::ipcClient.updateResidency(DistantLand::residencyCommitSharedId)) {
        return false;
    }
    if (DistantLand::ipcClient.waitForCompletion() != IPC::Complete) {
        return false;
    }
    return DistantLand::ipcClient.lastUpdateResidencySucceeded();
}

void markUnavailable(StaticResource& resource, const char* reason) {
    resource.state = StaticResourceState::Unavailable;
    ++residencyStats.unavailableCount;
    LOG::logline(
        "!! Distant static resource %lu is unavailable for this device session (%s)",
        resource.resourceId,
        reason
    );

    IPC::ResidencyCommit commit = {};
    commit.resourceId = resource.resourceId;
    commit.state = IPC::ResidencyUnavailable;
    commit.vbuffer = nullptr;
    commit.ibuffer = nullptr;
    std::vector<IPC::ResidencyCommit> commits { commit };
    commitResidency(commits);
}

// Creates both buffers from prepared bytes, inserts the palette, then commits the pointers.
// The host may only restore face counts after this whole sequence succeeds.
bool admitPreparedResource(StaticResource& resource, ResidencyIoWorker::Result& prepared) {
    IDirect3DVertexBuffer9* vb = nullptr;
    IDirect3DIndexBuffer9* ib = nullptr;
    void* lockdata = nullptr;

    if (prepared.vertexData.size() != resource.vertexBytes || prepared.indexData.size() != resource.indexBytes) {
        markUnavailable(resource, "prepared byte count does not match the catalog");
        return false;
    }

    if (FAILED(DistantLand::device->CreateVertexBuffer(resource.vertexBytes, D3DUSAGE_WRITEONLY, 0, D3DPOOL_DEFAULT, &vb, 0))) {
        markUnavailable(resource, "vertex buffer creation failed");
        return false;
    }
    if (FAILED(vb->Lock(0, 0, &lockdata, 0))) {
        vb->Release();
        markUnavailable(resource, "vertex buffer lock failed");
        return false;
    }
    std::memcpy(lockdata, prepared.vertexData.data(), resource.vertexBytes);
    vb->Unlock();

    if (FAILED(DistantLand::device->CreateIndexBuffer(resource.indexBytes, D3DUSAGE_WRITEONLY, D3DFMT_INDEX16, D3DPOOL_DEFAULT, &ib, 0))) {
        vb->Release();
        markUnavailable(resource, "index buffer creation failed");
        return false;
    }
    if (FAILED(ib->Lock(0, 0, &lockdata, 0))) {
        ib->Release();
        vb->Release();
        markUnavailable(resource, "index buffer lock failed");
        return false;
    }
    std::memcpy(lockdata, prepared.indexData.data(), resource.indexBytes);
    ib->Unlock();

    resource.vb = vb;
    resource.ib = ib;
    logicalGpuMergedBytes += resource.geometryBytes();
    residencyStats.peakGpuBytes = std::max(residencyStats.peakGpuBytes, logicalGpuMergedBytes);
    resource.state = StaticResourceState::CommitPending;
    residencyCommitPending.push_back(resource.resourceId);
    staticUvBoundPalettes[vb] = resource.palette;

    IPC::ResidencyCommit commit = {};
    commit.resourceId = resource.resourceId;
    commit.state = IPC::ResidencyResident;
    commit.vbuffer = vb;
    commit.ibuffer = ib;
    std::vector<IPC::ResidencyCommit> commits { commit };
    if (!commitResidency(commits)) {
        // Retain the buffers in commit-pending and retry at a later boundary; the host may
        // still be holding the request, so the pointers must stay valid.
        return false;
    }

    residencyCommitPending.pop_back();
    resource.state = StaticResourceState::Resident;
    ++residencyStats.admittedCount;
    residencyStats.admittedBytes += resource.geometryBytes();
    noteTransitionAdmission(resource);
    return true;
}

void queueAdmission(StaticResource& resource) {
    if (resource.state != StaticResourceState::Unloaded) {
        return;
    }
    if (residencyIo.queuedBytes() >= kReadyQueueLimitBytes) {
        return;
    }

    ResidencyIoWorker::Request request;
    request.resourceId = resource.resourceId;
    request.planEpoch = residencyPlanEpoch;
    request.shardId = resource.shardId;
    request.vertexOffset = resource.vertexOffset;
    request.indexOffset = resource.indexOffset;
    request.vertexBytes = resource.vertexBytes;
    request.indexBytes = resource.indexBytes;
    request.indexCopyPlan = resource.indexCopyPlan;

    resource.state = StaticResourceState::IoQueued;
    resource.planEpoch = residencyPlanEpoch;
    logicalReservedMergedBytes += resource.geometryBytes();
    residencyStats.peakReservedBytes = std::max(residencyStats.peakReservedBytes, logicalReservedMergedBytes);
    residencyIo.submit(std::move(request));
}

void queueEviction(StaticResource& resource) {
    switch (resource.state) {
    case StaticResourceState::IoQueued:
    case StaticResourceState::ReadyForGpu:
        // Never committed to the host, so no removal RPC is needed.
        resource.state = StaticResourceState::Unloaded;
        logicalReservedMergedBytes -= std::min(logicalReservedMergedBytes, resource.geometryBytes());
        ++residencyStats.cancelledCount;
        return;
    case StaticResourceState::Resident:
        resource.state = StaticResourceState::EvictQueued;
        resource.planEpoch = residencyPlanEpoch;
        residencyEvictQueue.push_back(resource.resourceId);
        return;
    default:
        return;
    }
}

bool prepareMappedResource(StaticResource& resource, ResidencyIoWorker::Result& prepared) {
    if (!fullDrain.mapping || fullDrain.shardId != resource.shardId) {
        fullDrain.mapping = std::make_unique<ReadOnlyMappedFile>(StaticMeshesGeometryWindowBytes);
        fullDrain.shardId = resource.shardId;
        const std::string path = staticMeshShardPath(resource.shardId);
        HANDLE file = CreateFile(path.c_str(), GENERIC_READ, FILE_SHARE_READ, 0, OPEN_EXISTING, 0, 0);
        if (file == INVALID_HANDLE_VALUE) {
            LOG::winerror("Distant statics full drain: failed to open %s", path.c_str());
            return false;
        }
        std::uint64_t fileSize = 0;
        const std::string sizeError = "Distant statics full drain: failed to query size for " + path;
        const bool initialized = MappedFileUtil::QueryFileSize(file, fileSize, sizeError.c_str())
            && fullDrain.mapping->initialize(file, fileSize);
        CloseHandle(file);
        if (!initialized) {
            MappedFileUtil::LogMappingFailure("Distant statics full drain: failed to map shard", *fullDrain.mapping);
            return false;
        }
    }

    prepared = {};
    prepared.resourceId = resource.resourceId;
    prepared.planEpoch = residencyPlanEpoch;
    prepared.vertexData.resize(resource.vertexBytes);
    if (!fullDrain.mapping->copyRange(resource.vertexOffset, resource.vertexBytes, prepared.vertexData.data())) {
        MappedFileUtil::LogMappingFailure("Distant statics full drain: failed to map vertex data", *fullDrain.mapping);
        return false;
    }

    prepared.indexData.resize(resource.indexBytes);
    if (resource.indexCopyPlan.empty()) {
        if (!fullDrain.mapping->copyRange(resource.indexOffset, resource.indexBytes, prepared.indexData.data())) {
            MappedFileUtil::LogMappingFailure("Distant statics full drain: failed to map index data", *fullDrain.mapping);
            return false;
        }
    } else {
        fullDrain.indexScratch.resize(resource.indexBytes);
        if (!fullDrain.mapping->copyRange(resource.indexOffset, resource.indexBytes, fullDrain.indexScratch.data())) {
            MappedFileUtil::LogMappingFailure("Distant statics full drain: failed to map index data", *fullDrain.mapping);
            return false;
        }
        std::size_t written = 0;
        for (const auto& span : resource.indexCopyPlan) {
            if (static_cast<std::uint64_t>(span.sourceOffset) + span.byteCount > resource.indexBytes
                || written + span.byteCount > resource.indexBytes) {
                return false;
            }
            std::memcpy(prepared.indexData.data() + written, fullDrain.indexScratch.data() + span.sourceOffset, span.byteCount);
            written += span.byteCount;
        }
        if (written != resource.indexBytes) {
            return false;
        }
    }

    prepared.ok = true;
    return true;
}

int plannerCellX = INT_MIN;
int plannerCellY = INT_MIN;

void beginResidencyTransition(const D3DXVECTOR3& center) {
    logResidencyTransitionSummary("next_epoch");

    residencyTransition = ResidencyTransitionStats();
    residencyTransition.active = true;
    residencyTransition.epoch = residencyPlanEpoch;
    if (liveLoadTransitionPending) {
        residencyTransition.trigger = ResidencyTransitionTrigger::Load;
        if (liveLoadDestinationValid) {
            residencyTransition.sampleSource = ResidencySampleSource::Resolve;
            residencyTransition.destination = liveLoadDestination;
            residencyTransition.samplePresent = liveLoadSamplePresent;
        } else {
            residencyTransition.sampleSource = ResidencySampleSource::Present;
            residencyTransition.destination = center;
            residencyTransition.samplePresent = residencyPresentSerial;
        }
    } else if (residencyPresentSerial == 0) {
        residencyTransition.trigger = ResidencyTransitionTrigger::Bootstrap;
        residencyTransition.sampleSource = ResidencySampleSource::Pump;
        residencyTransition.destination = center;
    } else {
        residencyTransition.trigger = ResidencyTransitionTrigger::Cell;
        residencyTransition.sampleSource = ResidencySampleSource::Present;
        residencyTransition.destination = center;
        residencyTransition.samplePresent = residencyPresentSerial;
    }

    liveLoadTransitionPending = false;
    liveLoadDestinationValid = false;
}

void noteTransitionPlannerElapsed(LARGE_INTEGER start) {
    if (residencyTransition.active) {
        residencyTransition.maxPlannerMs = std::max(
            residencyTransition.maxPlannerMs,
            DistantLoadInstrumentation::elapsed_ms(start)
        );
    }
}

void noteTransitionUploadElapsed(LARGE_INTEGER start) {
    if (residencyTransition.active) {
        residencyTransition.maxUploadMs = std::max(
            residencyTransition.maxUploadMs,
            DistantLoadInstrumentation::elapsed_ms(start)
        );
    }
}

}   // namespace

namespace DistantLoaders {

void logResidencySummary() {
    if (residencyIdle && residencyStats.admittedCount == 0 && residencyStats.evictedCount == 0) {
        return;
    }
    LOG::logline(
        "-- Distant static residency summary: admitted=%lu/%llu B evicted=%lu/%llu B peak_reserved=%llu B "
        "peak_gpu=%llu B gpu_now=%llu B cap=%llu B epochs=%lu cancelled=%lu unavailable=%lu palette_misses=%lu removal_non_complete=%lu frozen=%d",
        residencyStats.admittedCount,
        static_cast<unsigned long long>(residencyStats.admittedBytes),
        residencyStats.evictedCount,
        static_cast<unsigned long long>(residencyStats.evictedBytes),
        static_cast<unsigned long long>(residencyStats.peakReservedBytes),
        static_cast<unsigned long long>(residencyStats.peakGpuBytes),
        static_cast<unsigned long long>(logicalGpuMergedBytes),
        static_cast<unsigned long long>(activeMergedCapBytes()),
        residencyPlanEpoch,
        residencyStats.cancelledCount,
        residencyStats.unavailableCount,
        residencyStats.paletteMisses,
        residencyStats.nonCompleteRemovals,
        residencyFrozen ? 1 : 0
    );
}

void noteMissingPalette() {
    ++residencyStats.paletteMisses;
}

void armLiveLoadResidencyTransition(const D3DXVECTOR3* destination) {
    if (residencyIdle || residencyFrozen) {
        return;
    }
    liveLoadTransitionPending = true;
    liveLoadDestinationValid = destination != nullptr;
    if (destination) {
        liveLoadDestination = *destination;
        liveLoadSamplePresent = residencyPresentSerial;
    }
    plannerCellX = INT_MIN;
    plannerCellY = INT_MIN;
}

void noteResidencyPresent() {
    ++residencyPresentSerial;
}

void noteResidencyEvictionBoundary(bool stage0) {
    if (!residencyTransition.active) {
        return;
    }
    if (stage0) {
        residencyTransition.usedStage0Eviction = true;
    } else {
        residencyTransition.usedPresentEviction = true;
    }
}

bool residencyActive() {
    return !residencyIdle && !residencyFrozen;
}

bool residencyHasPendingEviction() {
    return residencyActive() && (!residencyEvictQueue.empty() || !residencyRemovalInFlight.empty());
}

bool residencyQuiescent() {
    if (!residencyActive()) {
        return true;
    }
    if (!residencyEvictQueue.empty() || !residencyRemovalInFlight.empty()) {
        return false;
    }
    if (!residencyCommitPending.empty()) {
        return false;
    }
    return residencyIo.idle();
}

std::uint64_t totalMergedGeometryBytes() {
    return mergedGeometryBytesTotal;
}

std::uint64_t logicalGpuMergedGeometryBytes() {
    return logicalGpuMergedBytes;
}

bool residencyFullDrainActive() {
    return fullDrain.active;
}

// Idempotent teardown of the reader thread and every in-flight residency intent. Safe to call
// from a partial-init abort; the catalog and its buffers are released separately.
void haltResidency() {
    residencyIo.stop();
    residencyIdle = true;
    residencyEvictQueue.clear();
    residencyRemovalInFlight.clear();
    residencyCommitPending.clear();
    fullDrain.reset();
}

void beginResidency() {
    residencyIo.stop();
    fullDrain.reset();
    residencyFrozen = false;
    plannerCellX = INT_MIN;
    plannerCellY = INT_MIN;
    const std::uint64_t cap = activeMergedCapBytes();
    if (mergedGeometryBytesTotal == 0) {
        residencyIdle = true;
        LOG::logline("-- Merged-static residency idle: dataset has no merged geometry");
        return;
    }
    if (mergedGeometryBytesTotal <= cap) {
        residencyIdle = true;
        fullDrain.active = true;
        LOG::logline(
            "-- Merged-static residency schedule: full_drain cap=%llu B merged_total=%llu B order=shard_file_offset",
            static_cast<unsigned long long>(cap),
            static_cast<unsigned long long>(mergedGeometryBytesTotal)
        );
        return;
    }
    residencyIdle = false;
    residencyIo.start();
    LOG::logline(
        "-- Merged-static residency schedule: capped_bootstrap cap=%llu B merged_total=%llu B resident_merged=%llu B",
        static_cast<unsigned long long>(cap),
        static_cast<unsigned long long>(mergedGeometryBytesTotal),
        static_cast<unsigned long long>(logicalReservedMergedBytes)
    );
}

bool stepResidencyFullDrain(
    double budgetMs,
    std::uint64_t budgetBytes,
    std::uint32_t budgetResources,
    bool& done
) {
    done = !fullDrain.active;
    if (!fullDrain.active) {
        return true;
    }

    const auto start = DistantLoadInstrumentation::counter_now();
    std::uint64_t bytesThisTick = 0;
    std::uint32_t resourcesThisTick = 0;
    while (fullDrain.cursor < staticResourceCatalog.size()) {
        StaticResource& resource = staticResourceCatalog[fullDrain.cursor];
        if (!resource.streamed || resource.state == StaticResourceState::Resident) {
            ++fullDrain.cursor;
            continue;
        }
        if (resource.state != StaticResourceState::Unloaded) {
            LOG::logline(
                "!! Distant static full drain found resource %lu in unexpected state %u",
                resource.resourceId,
                static_cast<unsigned>(resource.state)
            );
            return false;
        }

        const std::uint64_t resourceBytes = resource.geometryBytes();
        if (resourcesThisTick != 0 && resourceBytes > budgetBytes - std::min(budgetBytes, bytesThisTick)) {
            return true;
        }
        logicalReservedMergedBytes += resourceBytes;
        residencyStats.peakReservedBytes = std::max(residencyStats.peakReservedBytes, logicalReservedMergedBytes);
        resource.state = StaticResourceState::ReadyForGpu;

        ResidencyIoWorker::Result prepared;
        if (!prepareMappedResource(resource, prepared)) {
            logicalReservedMergedBytes -= std::min(logicalReservedMergedBytes, resourceBytes);
            markUnavailable(resource, "ordered startup shard read failed");
            return false;
        }
        if (!admitPreparedResource(resource, prepared)) {
            if (resource.state == StaticResourceState::Unavailable) {
                logicalReservedMergedBytes -= std::min(logicalReservedMergedBytes, resourceBytes);
            }
            return false;
        }

        ++fullDrain.cursor;
        bytesThisTick += resourceBytes;
        ++resourcesThisTick;
        const double elapsed = DistantLoadInstrumentation::elapsed_ms(start);
        if (elapsed >= budgetMs || resourcesThisTick >= budgetResources || bytesThisTick >= budgetBytes) {
            if (resourceBytes > budgetBytes) {
                LOG::logline(
                    "-- Distant static full drain admitted an oversize resource alone: id=%lu bytes=%llu elapsed=%.2fms",
                    resource.resourceId,
                    static_cast<unsigned long long>(resourceBytes),
                    elapsed
                );
            }
            return true;
        }
    }

    fullDrain.mapping.reset();
    fullDrain.indexScratch.clear();
    fullDrain.active = false;
    residencyIdle = true;
    done = true;
    LOG::logline(
        "-- Merged-static full drain complete: resident=%llu B resources=%lu planner=idle",
        static_cast<unsigned long long>(logicalGpuMergedBytes),
        residencyStats.admittedCount
    );
    return true;
}

void wakeResidencyForCapDebt() {
    if (residencyFrozen || logicalReservedMergedBytes <= activeMergedCapBytes()) {
        return;
    }
    residencyIdle = false;
    plannerCellX = INT_MIN;
    plannerCellY = INT_MIN;
    residencyIo.start();
    LOG::logline(
        "-- Merged-static residency rearmed after cap ratchet: cap=%llu B reserved=%llu B debt=%llu B",
        static_cast<unsigned long long>(activeMergedCapBytes()),
        static_cast<unsigned long long>(logicalReservedMergedBytes),
        static_cast<unsigned long long>(mergedCapDebtBytes())
    );
}

// Asks the host for the next bounded batch of admit/evict requests around `center`.
// A quantized cell change starts a new plan epoch, cancelling superseded queued I/O.
void planResidency(const D3DXVECTOR3& center) {
    if (!residencyActive() || DistantLand::residencyPlanSharedId == IPC::InvalidVector) {
        return;
    }

    const int cellX = static_cast<int>(std::floor(center.x / DistantLand::kCellSize));
    const int cellY = static_cast<int>(std::floor(center.y / DistantLand::kCellSize));
    if (cellX != plannerCellX || cellY != plannerCellY) {
        plannerCellX = cellX;
        plannerCellY = cellY;
        ++residencyPlanEpoch;
        beginResidencyTransition(center);

        std::vector<std::uint32_t> cancelled;
        residencyIo.cancelOlderEpochs(residencyPlanEpoch, cancelled);
        for (std::uint32_t resourceId : cancelled) {
            StaticResource* resource = findResource(resourceId);
            if (resource && resource->state == StaticResourceState::IoQueued) {
                resource->state = StaticResourceState::Unloaded;
                logicalReservedMergedBytes -= std::min(logicalReservedMergedBytes, resource->geometryBytes());
                ++residencyStats.cancelledCount;
            }
        }
    }

    const auto plannerStart = DistantLoadInstrumentation::counter_now();
    const float admissionRadius = Configuration.DL.DrawDist * DistantLand::kCellSize + DistantLand::kCellSize;
    const float retainRadius = admissionRadius + DistantLand::kCellSize;
    if (!DistantLand::ipcClient.planResidency(
            DistantLand::residencyPlanSharedId,
            residencyPlanEpoch,
            center,
            admissionRadius,
            retainRadius,
            kPlannerMaxCells,
            kPlannerMaxResources,
            activeMergedCapBytes(),
            availableMergedBytes(),
            mergedCapDebtBytes())) {
        noteTransitionPlannerElapsed(plannerStart);
        return;
    }
    if (DistantLand::ipcClient.waitForCompletion() != IPC::Complete) {
        noteTransitionPlannerElapsed(plannerStart);
        return;
    }

    const std::uint32_t requestCount = DistantLand::residencyPlanShared.size();
    for (std::uint32_t i = 0; i < requestCount; ++i) {
        const IPC::ResidencyPlan& request = DistantLand::residencyPlanShared[i];
        StaticResource* resource = findResource(request.resourceId);
        if (!resource || !resource->streamed) {
            continue;
        }
        if (request.action == IPC::ResidencyAdmit) {
            if (resource->state == StaticResourceState::EvictQueued) {
                // Cancellable only while the removal RPC has not been submitted.
                resource->state = StaticResourceState::Resident;
                residencyEvictQueue.erase(
                    std::remove(residencyEvictQueue.begin(), residencyEvictQueue.end(), request.resourceId),
                    residencyEvictQueue.end()
                );
            } else if (resource->state == StaticResourceState::RemovalInFlight) {
                resource->readmitRequested = true;
                resource->planEpoch = residencyPlanEpoch;
            } else if (resource->geometryBytes() <= availableMergedBytes()) {
                queueAdmission(*resource);
            }
        } else if (request.action == IPC::ResidencyEvict) {
            queueEviction(*resource);
        }
    }
    noteTransitionPlannerElapsed(plannerStart);
}

// Bounded main-thread upload of prepared bytes. Textures are already resident, so no
// filesystem probing or D3DX texture creation happens here.
void tickResidencyAdmission(double budgetMs, std::uint64_t budgetBytes, std::uint32_t budgetResources) {
    if (!residencyActive()) {
        return;
    }

    const auto start = DistantLoadInstrumentation::counter_now();
    std::uint64_t bytesThisTick = 0;
    std::uint32_t resourcesThisTick = 0;

    // Retry any resource whose admission commit did not complete earlier; its buffers are live.
    while (!residencyCommitPending.empty()) {
        StaticResource* resource = findResource(residencyCommitPending.back());
        if (!resource || resource->state != StaticResourceState::CommitPending) {
            residencyCommitPending.pop_back();
            continue;
        }
        IPC::ResidencyCommit commit = {};
        commit.resourceId = resource->resourceId;
        commit.state = IPC::ResidencyResident;
        commit.vbuffer = resource->vb;
        commit.ibuffer = resource->ib;
        std::vector<IPC::ResidencyCommit> commits { commit };
        if (!commitResidency(commits)) {
            noteTransitionUploadElapsed(start);
            return;
        }
        residencyCommitPending.pop_back();
        resource->state = StaticResourceState::Resident;
        ++residencyStats.admittedCount;
        residencyStats.admittedBytes += resource->geometryBytes();
        noteTransitionAdmission(*resource);
        if (++resourcesThisTick >= budgetResources) {
            noteTransitionUploadElapsed(start);
            return;
        }
    }

    ResidencyIoWorker::Result prepared;
    while (resourcesThisTick < budgetResources && bytesThisTick < budgetBytes) {
        if (!residencyIo.tryCollect(prepared)) {
            noteTransitionUploadElapsed(start);
            return;
        }

        StaticResource* resource = findResource(prepared.resourceId);
        if (!resource || resource->state != StaticResourceState::IoQueued) {
            continue;
        }

        const std::uint64_t resourceBytes = resource->geometryBytes();
        if (!prepared.ok) {
            logicalReservedMergedBytes -= std::min(logicalReservedMergedBytes, resourceBytes);
            markUnavailable(*resource, "shard read failed");
            continue;
        }
        if (prepared.planEpoch != residencyPlanEpoch) {
            // Superseded before any GPU allocation.
            resource->state = StaticResourceState::Unloaded;
            logicalReservedMergedBytes -= std::min(logicalReservedMergedBytes, resourceBytes);
            ++residencyStats.cancelledCount;
            continue;
        }

        resource->state = StaticResourceState::ReadyForGpu;
        if (!admitPreparedResource(*resource, prepared)) {
            if (resource->state == StaticResourceState::Unavailable) {
                logicalReservedMergedBytes -= std::min(logicalReservedMergedBytes, resourceBytes);
            }
            noteTransitionUploadElapsed(start);
            return;
        }

        bytesThisTick += resourceBytes;
        ++resourcesThisTick;
        if (resourceBytes > budgetBytes) {
            noteTransitionOversize(*resource);
        }

        const double elapsed = DistantLoadInstrumentation::elapsed_ms(start);
        if (elapsed >= budgetMs) {
            if (resourceBytes > budgetBytes) {
                LOG::logline(
                    "-- Distant static residency admitted an oversize resource alone: id=%lu bytes=%llu elapsed=%.2fms",
                    resource->resourceId,
                    static_cast<unsigned long long>(resourceBytes),
                    elapsed
                );
            }
            noteTransitionUploadElapsed(start);
            return;
        }
    }
    noteTransitionUploadElapsed(start);
}

// Acknowledged removal at a quiescent boundary. Release is forbidden unless the removal RPC
// returns Complete: on timeout or server loss the buffers, palette and ledger bytes stay in
// removal-in-flight and residency freezes for the device session.
bool tickResidencyEviction(double budgetMs, std::uint32_t budgetResources) {
    if (!residencyActive() || (residencyEvictQueue.empty() && residencyRemovalInFlight.empty())) {
        return false;
    }

    const auto start = DistantLoadInstrumentation::counter_now();
    const std::uint32_t evictedBefore = residencyStats.evictedCount;

    if (residencyRemovalInFlight.empty()) {
        const std::size_t batch = std::min<std::size_t>(residencyEvictQueue.size(), budgetResources);
        residencyRemovalInFlight.assign(residencyEvictQueue.begin(), residencyEvictQueue.begin() + batch);
        residencyEvictQueue.erase(residencyEvictQueue.begin(), residencyEvictQueue.begin() + batch);
        for (std::uint32_t resourceId : residencyRemovalInFlight) {
            StaticResource* resource = findResource(resourceId);
            if (resource) {
                resource->state = StaticResourceState::RemovalInFlight;
            }
        }
    }

    std::vector<IPC::ResidencyCommit> commits;
    commits.reserve(residencyRemovalInFlight.size());
    for (std::uint32_t resourceId : residencyRemovalInFlight) {
        IPC::ResidencyCommit commit = {};
        commit.resourceId = resourceId;
        commit.state = IPC::ResidencyUnloaded;
        commit.vbuffer = nullptr;
        commit.ibuffer = nullptr;
        commits.push_back(commit);
    }

    if (!commitResidency(commits)) {
        // Do not cancel, return to resident, or submit a second RPC: the host may still
        // execute the queued removal later, which would leave it holding a released pointer.
        ++residencyStats.nonCompleteRemovals;
        if (residencyStats.nonCompleteRemovals >= 3) {
            residencyFrozen = true;
            LOG::logline("!! Distant static removal was not acknowledged; freezing residency for this device session");
        }
        return false;
    }

    for (std::uint32_t resourceId : residencyRemovalInFlight) {
        StaticResource* resource = findResource(resourceId);
        if (!resource) {
            continue;
        }
        if (resource->readmitRequested) {
            // The pointers and face counts are still live; re-commit without I/O or allocation.
            resource->readmitRequested = false;
            resource->state = StaticResourceState::CommitPending;
            residencyCommitPending.push_back(resourceId);
            continue;
        }

        staticUvBoundPalettes.erase(resource->vb);
        if (resource->ib) { resource->ib->Release(); resource->ib = nullptr; }
        if (resource->vb) { resource->vb->Release(); resource->vb = nullptr; }
        logicalReservedMergedBytes -= std::min(logicalReservedMergedBytes, resource->geometryBytes());
        logicalGpuMergedBytes -= std::min(logicalGpuMergedBytes, resource->geometryBytes());
        resource->state = StaticResourceState::Unloaded;
        ++residencyStats.evictedCount;
        residencyStats.evictedBytes += resource->geometryBytes();
    }
    residencyRemovalInFlight.clear();

    // Carry any remainder to the next quiescent point rather than overrunning further. One slow
    // Release can exceed the wall-clock limit, but it is processed alone.
    (void)budgetMs;
    (void)start;
    return residencyStats.evictedCount != evictedBefore;
}

}   // namespace DistantLoaders
