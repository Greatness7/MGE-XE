
#include "morrowindskinning.h"

#include <windows.h>

#include <algorithm>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <unordered_map>

#include "configuration.h"
#include "mgeindexedskinning.h"
#include "proxydx/d3d8interface.h"
#include "support/log.h"

// Adapted from the MWSE fork's indexed-skinning implementation (GPLv2):
// SharedSE/NIDX8Renderer.{h,cpp}, SharedSE/NISkinInstance.{h,cpp} and the
// MWSE_INDEXED_SKINNING block of MWSE/PatchUtil.cpp. The engine-facing behavior is unchanged;
// the NetImmerse layouts are re-declared locally so MGE XE carries no dependency on MWSE.

namespace {

//---------------------------------------------------------------------------
// Minimal NetImmerse ABI
//
// Only the layouts the four hooks actually touch. Every type carries a size
// assertion; these describe the supported 32-bit Morrowind executable, so a
// layout drift is a build break rather than a runtime corruption.
//---------------------------------------------------------------------------

namespace NI {

struct Object;

struct ObjectVTable {
    void(__thiscall* destructor)(Object*, int);  // 0x0
};

struct Object {
    ObjectVTable* vTable;  // 0x0
    int refCount;          // 0x4
};
static_assert(sizeof(Object) == 0x8, "NI::Object failed size validation");

// The engine's intrusive refcounted handle (NiPointer). Assigning through it
// is what releases an incompatible partition back to the engine.
template <class T>
class Pointer {
public:
    Pointer(T* pointer = nullptr) {
        claim(pointer);
    }

    Pointer(const Pointer<T>& other) {
        claim(other.m_pointer);
    }

    ~Pointer() {
        release();
    }

    Pointer<T>& operator=(const Pointer<T>& other) {
        if (m_pointer != other.m_pointer) {
            claim(other.m_pointer);
        }
        return *this;
    }

    Pointer<T>& operator=(T* pointer) {
        if (m_pointer != pointer) {
            claim(pointer);
        }
        return *this;
    }

    operator T*() const {
        return m_pointer;
    }

    T* operator->() const {
        return m_pointer;
    }

    T* get() const {
        return m_pointer;
    }

private:
    T* m_pointer = nullptr;

    void release() {
        if (m_pointer) {
            T* const released = m_pointer;
            m_pointer = nullptr;
            if (--released->refCount == 0) {
                released->vTable->destructor(static_cast<Object*>(released), 1);
            }
        }
    }

    void claim(T* pointer) {
        release();
        m_pointer = pointer;
        if (m_pointer) {
            m_pointer->refCount++;
        }
    }
};
static_assert(sizeof(Pointer<Object>) == 0x4, "NI::Pointer failed size validation");

struct Point2 {
    float x, y;
};
static_assert(sizeof(Point2) == 0x8, "NI::Point2 failed size validation");

struct Point3 {
    float x, y, z;
};
static_assert(sizeof(Point3) == 0xC, "NI::Point3 failed size validation");

// Matches D3DCOLOR; the packer copies it straight into a D3DFVF_DIFFUSE slot.
struct PackedColor {
    unsigned char r, g, b, a;
};
static_assert(sizeof(PackedColor) == 0x4, "NI::PackedColor failed size validation");

struct SkinPartition : Object {
    struct Partition {
        void* vtbl;                        // 0x0
        unsigned short* bones;             // 0x4
        float* weights;                    // 0x8
        unsigned short* vertices;          // 0xC
        unsigned char* bonePalette;        // 0x10
        void* triangles;                   // 0x14
        unsigned short* stripLengths;      // 0x18
        unsigned short numVertices;        // 0x1C
        unsigned short numTriangles;       // 0x1E
        unsigned short numBones;           // 0x20
        unsigned short numStripLengths;    // 0x22
        unsigned short numBonesPerVertex;  // 0x24
        void* bufferData;                  // 0x28
    };

    unsigned int partitionCount;  // 0x8
    Partition* partitions;        // 0xC
};
static_assert(sizeof(SkinPartition) == 0x10, "NI::SkinPartition failed size validation");
static_assert(
    sizeof(SkinPartition::Partition) == 0x2C,
    "NI::SkinPartition::Partition failed size validation");

struct SkinData : Object {
    Pointer<SkinPartition> partition;  // 0x8
    unsigned char transform[0x34];     // 0xC  (NiTransform, passed through only)
    unsigned int numBones;             // 0x40
    void* boneData;                    // 0x44
};
static_assert(sizeof(SkinData) == 0x48, "NI::SkinData failed size validation");

struct SkinInstance : Object {
    Pointer<SkinData> skinData;  // 0x8
    void* rootParent;            // 0xC
    void** bones;                // 0x10
    int unknown_0x14;            // 0x14
};
static_assert(sizeof(SkinInstance) == 0x18, "NI::SkinInstance failed size validation");

struct GeometryData : Object {
    unsigned short vertexCount;  // 0x8
    unsigned short textureSets;  // 0xA
    unsigned char bounds[0x10];  // 0xC  (NiBound, unused here)
    Point3* vertex;              // 0x1C
    Point3* normal;              // 0x20
    PackedColor* color;          // 0x24
    Point2* textureCoords;       // 0x28
    unsigned int uniqueID;       // 0x2C
    unsigned short revisionID;   // 0x30
    bool unknown_0x32;           // 0x32
};
static_assert(sizeof(GeometryData) == 0x34, "NI::GeometryData failed size validation");

struct DX8VertexBufferManager {
    void* vTable;                 // 0x0
    int unknown_0x4;              // 0x4
    IDirect3DDevice8* d3dDevice;  // 0x8
};
static_assert(
    sizeof(DX8VertexBufferManager) == 0xC,
    "NI::DX8VertexBufferManager failed size validation");

// Only the device pointer is needed. The full NiDX8Renderer is 0x6A0 bytes;
// declaring the remainder would be maintenance with no consumer.
struct DX8Renderer {
    unsigned char padding_0x0[0x24];
    IDirect3DDevice8* d3dDevice;  // 0x24
};
static_assert(
    offsetof(DX8Renderer, d3dDevice) == 0x24,
    "NI::DX8Renderer::d3dDevice failed offset validation");

struct CriticalSection;

}  // namespace NI

//---------------------------------------------------------------------------
// Engine entry points
//---------------------------------------------------------------------------

const auto NI_CriticalSection_lock =
    reinterpret_cast<void(__thiscall*)(NI::CriticalSection*, const char*)>(0x693F00);
const auto NI_CriticalSection_unlock =
    reinterpret_cast<void(__thiscall*)(NI::CriticalSection*)>(0x693F10);
NI::CriticalSection* const vertexBufferCriticalSection =
    reinterpret_cast<NI::CriticalSection*>(0x7DEA78);

const auto NI_SkinPartition_makePartitions = reinterpret_cast<bool(__thiscall*)(
    NI::SkinPartition*, NI::GeometryData*, NI::SkinData*, unsigned char, unsigned char)>(
    0x6C78F0);

// `transform` and `worldBound` are forwarded untouched, so their layouts do
// not need declaring.
const auto NI_DX8Renderer_drawSkinnedPrimitive = reinterpret_cast<void(__thiscall*)(
    NI::DX8Renderer*, NI::GeometryData*, NI::SkinInstance*, void*, void*)>(0x6ADE70);

// Guards NiDX8VertexBufferManager's buffer creation; the stock packer takes it
// around CreateVertexBuffer and the replacement must do the same.
struct VertexBufferLock {
    VertexBufferLock() {
        NI_CriticalSection_lock(vertexBufferCriticalSection, nullptr);
    }
    ~VertexBufferLock() {
        NI_CriticalSection_unlock(vertexBufferCriticalSection);
    }
};

//---------------------------------------------------------------------------
// Checked patch and trampoline helpers
//
// MGE XE's existing MWBridge writers change protected memory but never verify
// what they are replacing. These hooks must fail closed instead: an older
// development MWSE build patches the same four sites, and silently stacking on
// top of it would corrupt both owners.
//---------------------------------------------------------------------------

constexpr std::uintptr_t PACK_SKINNED_VB_ADDRESS = 0x6BE2B0;
constexpr std::uintptr_t PACK_SKINNED_VB_RESUME_ADDRESS = 0x6BE2B6;
constexpr std::size_t PACK_SKINNED_VB_PROLOGUE_SIZE = 6;
constexpr unsigned char PACK_SKINNED_VB_PROLOGUE[PACK_SKINNED_VB_PROLOGUE_SIZE] = {
    0x81, 0xEC, 0x38, 0x01, 0x00, 0x00  // sub esp, 138h
};

constexpr std::uintptr_t MAKE_PARTITIONS_CALL_SITE = 0x6ADEDF;
constexpr std::uintptr_t MAKE_PARTITIONS_TARGET = 0x6C78F0;
constexpr std::uintptr_t DRAW_SKINNED_TRISHAPE_CALL_SITE = 0x6ACF36;
constexpr std::uintptr_t DRAW_SKINNED_TRISTRIPS_CALL_SITE = 0x6AD006;
constexpr std::uintptr_t DRAW_SKINNED_TARGET = 0x6ADE70;

bool readRelativeCallTarget(std::uintptr_t address, std::uintptr_t* target) {
    if (*reinterpret_cast<const unsigned char*>(address) != 0xE8) {
        return false;
    }
    const std::int32_t displacement = *reinterpret_cast<const std::int32_t*>(address + 1);
    *target = address + 5 + static_cast<std::uintptr_t>(displacement);
    return true;
}

bool writeRelativeCall(std::uintptr_t address, std::uintptr_t target) {
    DWORD oldProtect = 0;
    if (!VirtualProtect(reinterpret_cast<void*>(address), 5, PAGE_EXECUTE_READWRITE, &oldProtect)) {
        return false;
    }

    *reinterpret_cast<unsigned char*>(address) = 0xE8;
    *reinterpret_cast<std::int32_t*>(address + 1) =
        static_cast<std::int32_t>(target - (address + 5));

    VirtualProtect(reinterpret_cast<void*>(address), 5, oldProtect, &oldProtect);
    FlushInstructionCache(GetCurrentProcess(), reinterpret_cast<void*>(address), 5);
    return true;
}

// Redirects an existing relative CALL, but only if it still points where the
// stock executable points it.
bool patchCallEnforced(
    std::uintptr_t address,
    std::uintptr_t expectedTarget,
    const void* replacement,
    const char* siteName) {
    std::uintptr_t currentTarget = 0;
    if (!readRelativeCallTarget(address, &currentTarget)) {
        LOG::logline(
            "!! Indexed skinning: %s at 0x%08X is not a relative CALL; feature disabled.",
            siteName,
            static_cast<unsigned int>(address));
        return false;
    }

    if (currentTarget != expectedTarget) {
        LOG::logline(
            "!! Indexed skinning: %s at 0x%08X calls 0x%08X, expected 0x%08X. Another mod "
            "already owns this site; feature disabled.",
            siteName,
            static_cast<unsigned int>(address),
            static_cast<unsigned int>(currentTarget),
            static_cast<unsigned int>(expectedTarget));
        return false;
    }

    if (!writeRelativeCall(address, reinterpret_cast<std::uintptr_t>(replacement))) {
        LOG::logline(
            "!! Indexed skinning: %s at 0x%08X could not be made writable; feature disabled.",
            siteName,
            static_cast<unsigned int>(address));
        return false;
    }
    return true;
}

//---------------------------------------------------------------------------
// Feature state
//---------------------------------------------------------------------------

using PackSkinnedVBFn = IDirect3DVertexBuffer8*(__thiscall*)(
    NI::DX8VertexBufferManager*,
    NI::GeometryData*,
    NI::SkinInstance*,
    NI::SkinPartition::Partition*,
    IDirect3DVertexBuffer8*,
    int*,
    DWORD,
    int,
    int*,
    int*);

bool hookInstallAttempted = false;
bool allHooksInstalled = false;
PackSkinnedVBFn originalPackSkinnedVB = nullptr;

IDirect3DDevice8* negotiatedDevice = nullptr;
bool indexedSkinningEnabled = false;

// Partitions the indexed builder could not handle, which must not be retried
// every frame. Keyed by raw pointer but holding an engine reference: without
// that reference a released partition's address could be reused by an
// unrelated allocation and misread as a previous fallback.
using StockPartitionCache =
    std::unordered_map<NI::SkinPartition*, NI::Pointer<NI::SkinPartition>>;

// Deliberately leaked. The entries own engine references, and running their
// destructors from a static destructor would call into Morrowind during DLL or
// process teardown, when the engine may already be gone. onDeviceReleased()
// clears the cache while the runtime is still alive.
StockPartitionCache& stockPartitions() {
    static StockPartitionCache* const cache = new StockPartitionCache();
    return *cache;
}

//---------------------------------------------------------------------------
// The indexed vertex packer
//---------------------------------------------------------------------------

// Emits XYZB4 + LASTBETA_UBYTE4: position, three float weights (the fourth is
// implied), then four palette indices packed into the UBYTE4 beta slot.
//
// MGE XE does not wrap vertex buffers -- ProxyDevice::CreateVertexBuffer hands
// back the real IDirect3DVertexBuffer9 -- so the buffer is locked and released
// through the D3D9 interface even though the engine ABI names it as D3D8.
IDirect3DVertexBuffer8* packIndexedSkinnedVB(
    NI::DX8VertexBufferManager* manager,
    NI::GeometryData* geometryData,
    NI::SkinPartition::Partition* partition,
    DWORD pool,
    int* vertexStride,
    int* fvf) {
    if (!manager || !manager->d3dDevice || !geometryData || !partition || !partition->vertices ||
        !partition->weights || !partition->bonePalette) {
        return nullptr;
    }

    const unsigned int textureSetCount =
        std::min<unsigned int>(geometryData->textureSets, 8);
    if (!geometryData->vertex || (textureSetCount && !geometryData->textureCoords)) {
        return nullptr;
    }

    DWORD packedFvf = D3DFVF_XYZB4 | D3DFVF_LASTBETA_UBYTE4;
    int packedStride = 28;  // 12 position + 12 weights + 4 palette indices

    const int normalOffset = geometryData->normal ? packedStride : -1;
    if (geometryData->normal) {
        packedFvf |= D3DFVF_NORMAL;
        packedStride += sizeof(NI::Point3);
    }

    const int colorOffset = geometryData->color ? packedStride : -1;
    if (geometryData->color) {
        packedFvf |= D3DFVF_DIFFUSE;
        packedStride += sizeof(NI::PackedColor);
    }

    const int textureOffset = packedStride;
    if (textureSetCount) {
        packedFvf |= textureSetCount << D3DFVF_TEXCOUNT_SHIFT;
        packedStride += textureSetCount * sizeof(NI::Point2);
    } else {
        // The stock packer always declares at least one coordinate set; match
        // it so downstream FVF handling sees the same shape.
        packedFvf |= D3DFVF_TEX1;
        packedStride += sizeof(NI::Point2);
    }

    if (vertexStride) {
        *vertexStride = packedStride;
    }
    if (fvf) {
        *fvf = static_cast<int>(packedFvf);
    }

    IDirect3DVertexBuffer9* vertexBuffer = nullptr;
    const unsigned int bufferLength = partition->numVertices * packedStride;
    HRESULT createResult;
    {
        VertexBufferLock lock;
        createResult = manager->d3dDevice->CreateVertexBuffer(
            bufferLength,
            0,
            packedFvf,
            static_cast<D3DPOOL>(pool),
            reinterpret_cast<IDirect3DVertexBuffer8**>(&vertexBuffer));
    }

    if (FAILED(createResult) || !vertexBuffer) {
        return nullptr;
    }

    void* mapped = nullptr;
    if (FAILED(vertexBuffer->Lock(0, 0, &mapped, 0)) || !mapped) {
        vertexBuffer->Release();
        return nullptr;
    }

    unsigned char* const buffer = static_cast<unsigned char*>(mapped);
    std::memset(buffer, 0, bufferLength);
    bool repairedPaletteData = false;

    for (unsigned int i = 0; i < partition->numVertices; ++i) {
        const unsigned int sourceIndex = partition->vertices[i];
        if (sourceIndex >= geometryData->vertexCount) {
            LOG::logline(
                "!! Indexed skinning rejected partition %p with out-of-range vertex %u.",
                partition,
                sourceIndex);
            vertexBuffer->Unlock();
            vertexBuffer->Release();
            return nullptr;
        }
        unsigned char* const destination = buffer + i * packedStride;

        std::memcpy(destination, &geometryData->vertex[sourceIndex], sizeof(NI::Point3));
        float weights[4];
        unsigned char palette[4];
        std::memcpy(weights, partition->weights + i * 4, sizeof(weights));
        std::memcpy(palette, partition->bonePalette + i * 4, sizeof(palette));

        bool repairedInfluence = false;
        float weightSum = 0.0f;
        for (unsigned int influence = 0; influence < 4; ++influence) {
            if (!std::isfinite(weights[influence]) || weights[influence] < 0.0f) {
                weights[influence] = 0.0f;
                repairedInfluence = true;
            }
            if (palette[influence] >= partition->numBones
                || palette[influence] >= MGE_INDEXED_SKINNING_PALETTE_SIZE) {
                palette[influence] = 0;
                weights[influence] = 0.0f;
                repairedInfluence = true;
            }
            weightSum += weights[influence];
        }

        if (repairedInfluence) {
            repairedPaletteData = true;
            if (weightSum > 0.0f && std::isfinite(weightSum)) {
                for (float& weight : weights) {
                    weight /= weightSum;
                }
            } else {
                weights[0] = 1.0f;
                weights[1] = weights[2] = weights[3] = 0.0f;
                palette[0] = palette[1] = palette[2] = palette[3] = 0;
            }
        }

        std::memcpy(destination + 12, weights, 3 * sizeof(float));
        std::memcpy(destination + 24, palette, sizeof(palette));

        if (normalOffset >= 0) {
            std::memcpy(
                destination + normalOffset,
                &geometryData->normal[sourceIndex],
                sizeof(NI::Point3));
        }
        if (colorOffset >= 0) {
            std::memcpy(
                destination + colorOffset,
                &geometryData->color[sourceIndex],
                sizeof(NI::PackedColor));
        }
        for (unsigned int textureSet = 0; textureSet < textureSetCount; ++textureSet) {
            const NI::Point2* const textureCoordinates =
                geometryData->textureCoords + textureSet * geometryData->vertexCount;
            std::memcpy(
                destination + textureOffset + textureSet * sizeof(NI::Point2),
                &textureCoordinates[sourceIndex],
                sizeof(NI::Point2));
        }
    }

    if (FAILED(vertexBuffer->Unlock())) {
        vertexBuffer->Release();
        return nullptr;
    }
    if (repairedPaletteData) {
        LOG::logline(
            "!! Indexed skinning repaired invalid weights or palette indices in partition %p.",
            partition);
    }
    return reinterpret_cast<IDirect3DVertexBuffer8*>(vertexBuffer);
}

//---------------------------------------------------------------------------
// Capability negotiation and partition validation
//---------------------------------------------------------------------------

// Negotiated once per distinct device pointer, so a renderer restart
// re-queries but a steady-state frame does not.
void negotiateIndexedSkinning(NI::DX8Renderer* renderer) {
    IDirect3DDevice8* const device = renderer ? renderer->d3dDevice : nullptr;
    if (device == negotiatedDevice) {
        return;
    }

    negotiatedDevice = device;
    indexedSkinningEnabled = false;
    if (!allHooksInstalled || !device) {
        return;
    }

    IMgeIndexedSkinningCaps* capability = nullptr;
    if (FAILED(device->QueryInterface(
            IID_IMgeIndexedSkinningCaps,
            reinterpret_cast<void**>(&capability))) ||
        !capability) {
        LOG::logline(
            "-- Indexed skinning unavailable: the D3D8 device does not expose MGE XE's "
            "capability interface.");
        return;
    }

    MgeIndexedSkinningCaps caps = {};
    const HRESULT hr = capability->GetIndexedSkinningCaps(&caps);
    capability->Release();

    if (hr == E_PENDING) {
        // Shader initialization happens after some early engine draws. Retry
        // instead of caching startup as an unavailable device.
        negotiatedDevice = nullptr;
        return;
    }

    // maxPaletteBones is the composite authorization token: MGEProxyDevice
    // reports zero unless the runtime setting is on and the underlying device
    // has enough indexed matrices.
    indexedSkinningEnabled = SUCCEEDED(hr) &&
        caps.structVersion == MGE_INDEXED_SKINNING_CAPS_VERSION &&
        caps.maxPaletteBones >= MGE_INDEXED_SKINNING_PALETTE_SIZE;

    if (indexedSkinningEnabled) {
        LOG::logline("-- Indexed skinning enabled: %u-bone palette.", caps.maxPaletteBones);
    } else {
        LOG::logline("-- Indexed skinning unavailable: stock skinning path in use.");
    }
}

bool needsIndexedPartitionRebuild(NI::SkinPartition* skinPartition) {
    // A partition the indexed builder already rejected stays as-is.
    if (stockPartitions().find(skinPartition) != stockPartitions().end()) {
        return false;
    }
    // No partition yet: leave it null for the engine's lazy creation path.
    if (!skinPartition || !skinPartition->partitions || skinPartition->partitionCount == 0) {
        return true;
    }
    // A dense partition has no palette and must be rebuilt.
    if (!skinPartition->partitions[0].bonePalette) {
        return true;
    }

    for (unsigned int i = 0; i < skinPartition->partitionCount; ++i) {
        const NI::SkinPartition::Partition& partition = skinPartition->partitions[i];
        if (!partition.bones
            || !partition.weights
            || !partition.vertices
            || !partition.bonePalette
            || partition.numBones == 0
            || partition.numBonesPerVertex != 4
            || partition.numBones > MGE_INDEXED_SKINNING_PALETTE_SIZE) {
            return true;
        }
    }
    return false;
}

//---------------------------------------------------------------------------
// The four hooks
//
// Each stays transparent while the feature is unauthorized, because patch
// installation is not fully transactional: an earlier write cannot always be
// rolled back once a later site fails its check.
//---------------------------------------------------------------------------

void __fastcall PatchDrawSkinnedPrimitive(
    NI::DX8Renderer* renderer,
    DWORD /*edx*/,
    NI::GeometryData* geometryData,
    NI::SkinInstance* skinInstance,
    void* transform,
    void* worldBound) {
    negotiateIndexedSkinning(renderer);

    if (indexedSkinningEnabled && skinInstance && skinInstance->skinData) {
        NI::SkinData* const skinData = skinInstance->skinData.get();
        if (skinData->partition && needsIndexedPartitionRebuild(skinData->partition.get())) {
            // Releases MGE XE's claim through normal refcounting; the engine
            // then rebuilds through the MakePartitions hook below.
            skinData->partition = nullptr;
        }
    }

    NI_DX8Renderer_drawSkinnedPrimitive(
        renderer, geometryData, skinInstance, transform, worldBound);
}

bool __fastcall PatchMakeSkinPartitions(
    NI::SkinPartition* skinPartition,
    DWORD /*edx*/,
    NI::GeometryData* geometryData,
    NI::SkinData* skinData,
    unsigned char bonesPerPartition,
    unsigned char bonesPerVertex) {
    if (!indexedSkinningEnabled) {
        return NI_SkinPartition_makePartitions(
            skinPartition, geometryData, skinData, bonesPerPartition, bonesPerVertex);
    }

    const bool indexedResult = NI_SkinPartition_makePartitions(
        skinPartition,
        geometryData,
        skinData,
        MGE_INDEXED_SKINNING_PALETTE_SIZE,
        4);
    if (indexedResult && skinPartition->partitions && skinPartition->partitionCount != 0) {
        return true;
    }

    LOG::logline(
        "-- Indexed skinning repartition produced no usable partitions; using the stock path "
        "for this skin.");
    const bool stockResult = NI_SkinPartition_makePartitions(
        skinPartition, geometryData, skinData, bonesPerPartition, bonesPerVertex);
    const bool stockPartitionUsable =
        stockResult && skinPartition->partitions && skinPartition->partitionCount != 0;
    if (stockPartitionUsable) {
        // Retains a reference so the address cannot be recycled underneath the
        // cache while the entry lives.
        stockPartitions()[skinPartition] = NI::Pointer<NI::SkinPartition>(skinPartition);
    }
    return stockPartitionUsable;
}

// Installed over the function entry itself rather than a call site, so it must
// reproduce the engine's __thiscall shape: __fastcall consumes ECX as `this`,
// the unused EDX slot absorbs no argument, and the remaining nine stay on the
// stack with callee cleanup, exactly as __thiscall passes them.
IDirect3DVertexBuffer8* __fastcall PatchPackSkinnedVB(
    NI::DX8VertexBufferManager* manager,
    DWORD /*edx*/,
    NI::GeometryData* geometryData,
    NI::SkinInstance* skinInstance,
    NI::SkinPartition::Partition* partition,
    IDirect3DVertexBuffer8* existingBuffer,
    int* bufferSize,
    DWORD pool,
    int flags,
    int* vertexStride,
    int* fvf) {
    if (!indexedSkinningEnabled || !partition || !partition->bonePalette) {
        if (!originalPackSkinnedVB) {
            return nullptr;
        }
        return originalPackSkinnedVB(
            manager,
            geometryData,
            skinInstance,
            partition,
            existingBuffer,
            bufferSize,
            pool,
            flags,
            vertexStride,
            fvf);
    }

    // The stock packer and all three of its callers ignore or pass inert
    // values for existingBuffer, bufferSize and flags, so the indexed path
    // does not consume them either.
    return packIndexedSkinnedVB(manager, geometryData, partition, pool, vertexStride, fvf);
}

// Copies the six-byte prologue into an executable trampoline that jumps back
// into the function body, then redirects the entry point at the hook.
bool installPackSkinnedVBHook() {
    if (std::memcmp(
            reinterpret_cast<const void*>(PACK_SKINNED_VB_ADDRESS),
            PACK_SKINNED_VB_PROLOGUE,
            PACK_SKINNED_VB_PROLOGUE_SIZE) != 0) {
        LOG::logline(
            "!! Indexed skinning: PackSkinnedVB at 0x%08X does not have the expected prologue. "
            "Another mod already owns this function; feature disabled.",
            static_cast<unsigned int>(PACK_SKINNED_VB_ADDRESS));
        return false;
    }

    constexpr std::size_t trampolineSize = PACK_SKINNED_VB_PROLOGUE_SIZE + 5;
    unsigned char* const trampoline = static_cast<unsigned char*>(VirtualAlloc(
        nullptr, trampolineSize, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE));
    if (!trampoline) {
        LOG::logline("!! Indexed skinning: could not allocate a trampoline; feature disabled.");
        return false;
    }

    std::memcpy(trampoline, PACK_SKINNED_VB_PROLOGUE, PACK_SKINNED_VB_PROLOGUE_SIZE);
    trampoline[PACK_SKINNED_VB_PROLOGUE_SIZE] = 0xE9;  // jmp rel32
    *reinterpret_cast<std::int32_t*>(trampoline + PACK_SKINNED_VB_PROLOGUE_SIZE + 1) =
        static_cast<std::int32_t>(
            PACK_SKINNED_VB_RESUME_ADDRESS -
            (reinterpret_cast<std::uintptr_t>(trampoline) + trampolineSize));
    FlushInstructionCache(GetCurrentProcess(), trampoline, trampolineSize);

    // Publish the trampoline before the entry point starts routing here: once
    // the JMP lands, a call can arrive immediately, and it must find a valid
    // original to delegate to.
    originalPackSkinnedVB = reinterpret_cast<PackSkinnedVBFn>(trampoline);

    DWORD oldProtect = 0;
    if (!VirtualProtect(
            reinterpret_cast<void*>(PACK_SKINNED_VB_ADDRESS),
            PACK_SKINNED_VB_PROLOGUE_SIZE,
            PAGE_EXECUTE_READWRITE,
            &oldProtect)) {
        originalPackSkinnedVB = nullptr;
        VirtualFree(trampoline, 0, MEM_RELEASE);
        LOG::logline(
            "!! Indexed skinning: PackSkinnedVB could not be made writable; feature disabled.");
        return false;
    }

    unsigned char* const entry = reinterpret_cast<unsigned char*>(PACK_SKINNED_VB_ADDRESS);
    entry[0] = 0xE9;  // jmp rel32
    *reinterpret_cast<std::int32_t*>(entry + 1) = static_cast<std::int32_t>(
        reinterpret_cast<std::uintptr_t>(&PatchPackSkinnedVB) - (PACK_SKINNED_VB_ADDRESS + 5));
    entry[5] = 0x90;  // nop out the tail of the replaced prologue

    VirtualProtect(
        reinterpret_cast<void*>(PACK_SKINNED_VB_ADDRESS),
        PACK_SKINNED_VB_PROLOGUE_SIZE,
        oldProtect,
        &oldProtect);
    FlushInstructionCache(
        GetCurrentProcess(),
        reinterpret_cast<void*>(PACK_SKINNED_VB_ADDRESS),
        PACK_SKINNED_VB_PROLOGUE_SIZE);

    return true;
}

}  // namespace

namespace MorrowindIndexedSkinning {

void installHooks() {
    if (hookInstallAttempted) {
        return;
    }
    hookInstallAttempted = true;

    const bool packerInstalled = installPackSkinnedVBHook();
    const bool makePartitionsInstalled = patchCallEnforced(
        MAKE_PARTITIONS_CALL_SITE,
        MAKE_PARTITIONS_TARGET,
        reinterpret_cast<const void*>(&PatchMakeSkinPartitions),
        "MakePartitions call site");
    const bool triShapeInstalled = patchCallEnforced(
        DRAW_SKINNED_TRISHAPE_CALL_SITE,
        DRAW_SKINNED_TARGET,
        reinterpret_cast<const void*>(&PatchDrawSkinnedPrimitive),
        "DrawSkinnedPrimitive (TriShape) call site");
    const bool triStripsInstalled = patchCallEnforced(
        DRAW_SKINNED_TRISTRIPS_CALL_SITE,
        DRAW_SKINNED_TARGET,
        reinterpret_cast<const void*>(&PatchDrawSkinnedPrimitive),
        "DrawSkinnedPrimitive (TriStrips) call site");

    allHooksInstalled =
        packerInstalled && makePartitionsInstalled && triShapeInstalled && triStripsInstalled;

    if (allHooksInstalled) {
        LOG::logline("-- Indexed skinning engine hooks installed");
    } else {
        // Any hook that did install stays transparent: each one delegates to
        // stock engine behavior while allHooksInstalled is false.
        LOG::logline(
            "!! Indexed skinning engine hooks incomplete; the stock skinning path remains in "
            "use for this session.");
    }
}

bool hooksInstalled() {
    return allHooksInstalled;
}

void onDeviceReleased() {
    negotiatedDevice = nullptr;
    indexedSkinningEnabled = false;

    // Released here, while Morrowind is still running, rather than from a
    // static destructor during teardown. A fallback mesh may be retried once
    // after device recreation; that is cheaper than an unsafe release.
    stockPartitions().clear();
}

}  // namespace MorrowindIndexedSkinning
