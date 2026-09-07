
#include "camerarelative.h"

#include <windows.h>

#include <cmath>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <unordered_map>

#include "configuration.h"
#include "mwpatches.h"
#include "support/log.h"

namespace {

//---------------------------------------------------------------------------
// Minimal NetImmerse ABI
//
// Only the layouts the hooks touch, each with size or offset assertions
// against the 32-bit Morrowind executable (cross-checked with MWSE's SharedSE
// headers), so layout drift is a build break rather than a bad read.
//---------------------------------------------------------------------------

namespace NI {

struct Point3 {
    float x, y, z;
};
static_assert(sizeof(Point3) == 0xC, "NI::Point3 failed size validation");

// Row-major. rotation * v = (m[0] . v, m[1] . v, m[2] . v).
struct Matrix33 {
    float m[3][3];
};
static_assert(sizeof(Matrix33) == 0x24, "NI::Matrix33 failed size validation");

struct Transform {
    Matrix33 rotation;   // 0x0
    Point3 translation;  // 0x24
    float scale;         // 0x30
};
static_assert(sizeof(Transform) == 0x34, "NI::Transform failed size validation");

struct AVObject {
    void* vTable;                      // 0x0
    int refCount;                      // 0x4
    const char* name;                  // 0x8
    unsigned char objectNet_0xC[0x8];  // 0xC  (extra data, controllers)
    unsigned short flags;              // 0x14
    unsigned short padding_0x16;       // 0x16
    AVObject* parentNode;              // 0x18
    Point3 worldBoundCenter;           // 0x1C
    float worldBoundRadius;            // 0x28
    Matrix33* localRotation;           // 0x2C
    Point3 localTranslate;             // 0x30
    float localScale;                  // 0x3C
    Transform worldTransform;          // 0x40
    unsigned char padding_0x74[0x1C];  // 0x74
};
static_assert(sizeof(AVObject) == 0x90, "NI::AVObject failed size validation");
static_assert(offsetof(AVObject, parentNode) == 0x18, "NI::AVObject::parentNode offset");
static_assert(offsetof(AVObject, localTranslate) == 0x30, "NI::AVObject::localTranslate offset");
static_assert(offsetof(AVObject, worldTransform) == 0x40, "NI::AVObject::worldTransform offset");

struct SkinPartition {
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
};
static_assert(sizeof(SkinPartition::Partition) == 0x2C, "NI::SkinPartition::Partition failed size validation");

struct SkinData {
    struct BoneData {
        Transform transform;         // 0x0   bone space to skin space at bind pose
        unsigned char bounds[0x10];  // 0x34
        void* weights;               // 0x44
        unsigned short weightCount;  // 0x48
        unsigned short padding_0x4A; // 0x4A
    };

    void* vTable;           // 0x0
    int refCount;           // 0x4
    void* partition;        // 0x8
    Transform transform;    // 0xC   root parent to skin
    unsigned int numBones;  // 0x40
    BoneData* boneData;     // 0x44
};
static_assert(sizeof(SkinData) == 0x48, "NI::SkinData failed size validation");
static_assert(sizeof(SkinData::BoneData) == 0x4C, "NI::SkinData::BoneData failed size validation");

struct SkinInstance {
    void* vTable;          // 0x0
    int refCount;          // 0x4
    SkinData* skinData;    // 0x8
    AVObject* rootParent;  // 0xC
    AVObject** bones;      // 0x10
    int unknown_0x14;      // 0x14
};
static_assert(sizeof(SkinInstance) == 0x18, "NI::SkinInstance failed size validation");

// The renderer fields SetModelTransform and SetSkinnedModelTransforms update
// besides the D3D world matrix: the camera axes expressed in model space.
struct DX8Renderer {
    unsigned char padding_0x0[0x2AC];
    Point3 cameraRight;       // 0x2AC
    Point3 cameraUp;          // 0x2B8
    Point3 modelCameraRight;  // 0x2C4
    Point3 modelCameraUp;     // 0x2D0
};
static_assert(offsetof(DX8Renderer, cameraRight) == 0x2AC, "NI::DX8Renderer::cameraRight offset");
static_assert(offsetof(DX8Renderer, modelCameraUp) == 0x2D0, "NI::DX8Renderer::modelCameraUp offset");

}  // namespace NI

//---------------------------------------------------------------------------
// Engine addresses (Morrowind.exe, image base 0x400000)
//---------------------------------------------------------------------------

constexpr std::uintptr_t NIDX8RENDERER_VTABLE = 0x74F4D0;
constexpr std::uintptr_t SET_CAMERA_DATA_SLOT = NIDX8RENDERER_VTABLE + 0xB8;
constexpr std::uintptr_t RENDER_SHAPE_SLOT = NIDX8RENDERER_VTABLE + 0xBC;
constexpr std::uintptr_t RENDER_TRISTRIPS_SLOT = NIDX8RENDERER_VTABLE + 0xC0;
constexpr std::uintptr_t SET_CAMERA_DATA_ADDRESS = 0x6AC620;
constexpr std::uintptr_t RENDER_SHAPE_ADDRESS = 0x6ACEF0;
constexpr std::uintptr_t RENDER_TRISTRIPS_ADDRESS = 0x6ACFC0;
constexpr std::uintptr_t SET_MODEL_TRANSFORM_ADDRESS = 0x6AC9C0;
constexpr std::uintptr_t SET_BONE_TRANSFORM_ADDRESS = 0x6ACB10;
constexpr std::uintptr_t SET_SKINNED_MODEL_TRANSFORMS_ADDRESS = 0x6ACBE0;
constexpr std::uintptr_t UPDATE_CAMERA_TRANSFORMS_ADDRESS = 0x542E60;

// WorldController::{worldCamera 0x124, armCamera 0x150, shadowCamera 0x2B0}
// + WorldControllerRenderCamera::sgCameraRoot (0xC): the roots the engine
// copies the first-person eye into.
constexpr std::uintptr_t WORLD_CONTROLLER_POINTER = 0x7C67DC;
constexpr DWORD WORLD_CONTROLLER_CAMERA_ROOT_OFFSETS[] = { 0x130, 0x15C, 0x2BC };
constexpr int CAMERA_ROOT_COUNT = 3;
// WorldController::{worldCamera 0x124, armCamera 0x150}
// + WorldControllerRenderCamera::cameraData.camera (0x10): the two NiCameras
// whose scenes this module converts.
constexpr DWORD WORLD_CONTROLLER_OWNED_CAMERA_OFFSETS[] = { 0x134, 0x160 };
// PlayerAnimController: the first-person model's "Camera" node, found by name
// in MACP::setupCameras. In first person the engine copies its stored world
// position into every camera root (PlayerAnimController::updateCameraTransforms).
constexpr DWORD PLAYER_ANIM_CONTROLLER_HEAD_CAMERA_OFFSET = 0xD4;

struct CallSite {
    std::uintptr_t address;
    const char* name;
};
// Every CALL to NiDX8Renderer::SetModelTransform. The batch and screen-poly
// callers never pass a node's own transform, so the replacement falls back to
// stock behavior there; they are patched for completeness of the space.
constexpr CallSite SET_MODEL_TRANSFORM_SITES[] = {
    { 0x6AED2B, "DrawPrimitive -> SetModelTransform" },
    { 0x6AD0ED, "RenderPoints -> SetModelTransform" },
    { 0x6AD8C2, "RenderLines -> SetModelTransform" },
    { 0x6AE404, "EndBatch -> SetModelTransform" },
    { 0x65451F, "sub_6544F0 -> SetModelTransform" },
};
constexpr CallSite SET_SKINNED_MODEL_TRANSFORMS_SITES[] = {
    { 0x6AF188, "DrawSkinnedPrimitive2 -> SetSkinnedModelTransforms" },
    { 0x6AECE7, "DrawPrimitive -> SetSkinnedModelTransforms" },
    { 0x6AE3B9, "EndBatch -> SetSkinnedModelTransforms" },
};
constexpr CallSite UPDATE_CAMERA_TRANSFORMS_SITES[] = {
    { 0x41C029, "TES3Game::renderNextFrame -> updateCameraTransforms" },
    { 0x567A17, "MACP::updateScenegraph -> updateCameraTransforms" },
    { 0x56871C, "MACP::setPosition -> updateCameraTransforms" },
    { 0x45CDC3, "cellChangeWithCompanion -> updateCameraTransforms" },
    { 0x48F64A, "DataHandler::sub_48F5F0 -> updateCameraTransforms" },
};

using SetCameraDataFn = void(__thiscall*)(
    void* renderer, const float* worldLocation, const float* worldDirection, const float* worldUp,
    const float* worldRight, const void* frustum, const void* viewport);
using RenderShapeFn = void(__thiscall*)(
    void* renderer, void* geometryData, void* skinInstance, NI::Transform* transform, void* worldBound);
using SetModelTransformFn = void(__thiscall*)(void* renderer, const NI::Transform* transform);
using SetBoneTransformFn = void(__thiscall*)(void* renderer, const NI::Transform* transform, int boneIndex);
using SetSkinnedModelTransformsFn = void(__thiscall*)(
    void* renderer, NI::SkinInstance* skinInstance, NI::SkinPartition::Partition* partition,
    NI::Transform* transform, void* bound);
using UpdateCameraTransformsFn = void(__thiscall*)(void* playerAnimController);

SetCameraDataFn originalSetCameraData = nullptr;
RenderShapeFn originalRenderShape = nullptr;
RenderShapeFn originalRenderTriStrips = nullptr;
const SetModelTransformFn engineSetModelTransform = reinterpret_cast<SetModelTransformFn>(SET_MODEL_TRANSFORM_ADDRESS);
const SetBoneTransformFn engineSetBoneTransform = reinterpret_cast<SetBoneTransformFn>(SET_BONE_TRANSFORM_ADDRESS);
const SetSkinnedModelTransformsFn engineSetSkinnedModelTransforms =
    reinterpret_cast<SetSkinnedModelTransformsFn>(SET_SKINNED_MODEL_TRANSFORMS_ADDRESS);
const UpdateCameraTransformsFn engineUpdateCameraTransforms =
    reinterpret_cast<UpdateCameraTransformsFn>(UPDATE_CAMERA_TRANSFORMS_ADDRESS);

bool installAttempted = false;
bool installSkippedByConfig = false;
bool cameraHookInstalled = false;
bool rigidHooksInstalled = false;
bool skinnedHooksInstalled = false;
bool eyeHooksInstalled = false;

//---------------------------------------------------------------------------
// Patch helpers
//---------------------------------------------------------------------------

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
    *reinterpret_cast<std::int32_t*>(address + 1) = static_cast<std::int32_t>(target - (address + 5));
    VirtualProtect(reinterpret_cast<void*>(address), 5, oldProtect, &oldProtect);
    FlushInstructionCache(GetCurrentProcess(), reinterpret_cast<void*>(address), 5);
    return true;
}

bool writeVtableSlot(std::uintptr_t slot, std::uintptr_t value) {
    DWORD oldProtect = 0;
    if (!VirtualProtect(reinterpret_cast<void*>(slot), sizeof(void*), PAGE_READWRITE, &oldProtect)) {
        return false;
    }
    *reinterpret_cast<std::uintptr_t*>(slot) = value;
    VirtualProtect(reinterpret_cast<void*>(slot), sizeof(void*), oldProtect, &oldProtect);
    return true;
}

bool patchCallEnforced(const CallSite& site, std::uintptr_t expectedTarget, const void* replacement) {
    std::uintptr_t currentTarget = 0;
    if (!readRelativeCallTarget(site.address, &currentTarget)) {
        LOG::logline("!! Camera-relative rendering: %s at 0x%08X is not a relative CALL.",
            site.name, static_cast<unsigned int>(site.address));
        return false;
    }
    if (currentTarget != expectedTarget) {
        LOG::logline("!! Camera-relative rendering: %s at 0x%08X calls 0x%08X, expected 0x%08X. "
            "Another mod already owns this site.",
            site.name, static_cast<unsigned int>(site.address),
            static_cast<unsigned int>(currentTarget), static_cast<unsigned int>(expectedTarget));
        return false;
    }
    if (!writeRelativeCall(site.address, reinterpret_cast<std::uintptr_t>(replacement))) {
        LOG::logline("!! Camera-relative rendering: %s at 0x%08X could not be made writable.",
            site.name, static_cast<unsigned int>(site.address));
        return false;
    }
    return true;
}

// Patches every site or none: a site that fails after earlier ones succeeded
// puts those back to the stock target, so a hook conflict never leaves a
// group half-owned.
template <std::size_t N>
bool patchCallSites(const CallSite (&sites)[N], std::uintptr_t stockTarget, const void* replacement) {
    for (std::size_t i = 0; i < N; ++i) {
        if (!patchCallEnforced(sites[i], stockTarget, replacement)) {
            for (std::size_t j = 0; j < i; ++j) {
                writeRelativeCall(sites[j].address, stockTarget);
            }
            return false;
        }
    }
    return true;
}

// Replaces a vtable slot only if it still holds the stock function; returns
// the stock function through `original`.
bool patchVtableSlotEnforced(std::uintptr_t slot, std::uintptr_t expected, const void* replacement,
    const char* name, void** original) {
    const std::uintptr_t current = *reinterpret_cast<const std::uintptr_t*>(slot);
    if (current != expected) {
        LOG::logline("!! Camera-relative rendering: %s slot at 0x%08X holds 0x%08X, expected 0x%08X. "
            "Another mod already owns this slot.",
            name, static_cast<unsigned int>(slot), static_cast<unsigned int>(current),
            static_cast<unsigned int>(expected));
        return false;
    }
    if (!writeVtableSlot(slot, reinterpret_cast<std::uintptr_t>(replacement))) {
        LOG::logline("!! Camera-relative rendering: %s slot could not be made writable.", name);
        return false;
    }
    *original = reinterpret_cast<void*>(current);
    return true;
}

//---------------------------------------------------------------------------
// Double-precision affine transforms (NetImmerse conventions)
//---------------------------------------------------------------------------

struct DTransform {
    double r[3][3];
    double t[3];
    double s;
};

DTransform fromNi(const NI::Transform& in) {
    DTransform out;
    for (int i = 0; i < 3; ++i) {
        for (int j = 0; j < 3; ++j) {
            out.r[i][j] = in.rotation.m[i][j];
        }
    }
    out.t[0] = in.translation.x;
    out.t[1] = in.translation.y;
    out.t[2] = in.translation.z;
    out.s = in.scale;
    return out;
}

// NiTransform::operator*: (a * b)(p) = a(b(p)).
DTransform combine(const DTransform& a, const DTransform& b) {
    DTransform out;
    for (int i = 0; i < 3; ++i) {
        for (int j = 0; j < 3; ++j) {
            out.r[i][j] = a.r[i][0] * b.r[0][j] + a.r[i][1] * b.r[1][j] + a.r[i][2] * b.r[2][j];
        }
        out.t[i] = (a.r[i][0] * b.t[0] + a.r[i][1] * b.t[1] + a.r[i][2] * b.t[2]) * a.s + a.t[i];
    }
    out.s = a.s * b.s;
    return out;
}

// NiTransform::Invert for an orthonormal rotation.
bool invert(const DTransform& a, DTransform* out) {
    if (a.s == 0.0) {
        return false;
    }
    for (int i = 0; i < 3; ++i) {
        for (int j = 0; j < 3; ++j) {
            out->r[i][j] = a.r[j][i];
        }
    }
    out->s = 1.0 / a.s;
    for (int i = 0; i < 3; ++i) {
        out->t[i] = -(out->r[i][0] * a.t[0] + out->r[i][1] * a.t[1] + out->r[i][2] * a.t[2]) * out->s;
    }
    return true;
}

//---------------------------------------------------------------------------
// Exact world translation, memoized per scene
//---------------------------------------------------------------------------

constexpr int MAX_CHAIN_DEPTH = 64;

// Whether the engine produced the stored translation from these inputs by its
// own float update (NiAVObject::UpdateWorldData:
// world.t = parent.t + parent.s * (parent.R * local.t)). When it did not, the
// engine placed this node some other way (root motion, a direct write, a stale
// local) and the stored value is the only truth; the chain then anchors on it
// and continues exactly from there. The check repeats the engine's arithmetic
// in float and in the engine's operation order (rotate, then scale, then add),
// so ordinary rounding stays within a few float steps; the tolerance is
// sixteen steps at the magnitude of the coordinate.
constexpr int GUARD_STEPS = 16;

float floatStep(float magnitude) {
    int exponent = 0;
    std::frexp(magnitude, &exponent);
    return std::ldexp(1.0f, exponent - 24);
}

bool withinGuard(float computed, float stored) {
    const float delta = std::fabs(computed - stored);
    const float magnitude = std::fabs(stored) > std::fabs(computed) ? std::fabs(stored) : std::fabs(computed);
    return delta <= GUARD_STEPS * floatStep(magnitude);
}

bool storedFollowsParent(const NI::AVObject* node, const NI::AVObject* parent) {
    const float s = parent->worldTransform.scale;
    const NI::Point3& l = node->localTranslate;
    const float (*m)[3] = parent->worldTransform.rotation.m;
    const NI::Point3& pt = parent->worldTransform.translation;
    const NI::Point3& st = node->worldTransform.translation;
    const float rx = m[0][0] * l.x + m[0][1] * l.y + m[0][2] * l.z;
    const float ry = m[1][0] * l.x + m[1][1] * l.y + m[1][2] * l.z;
    const float rz = m[2][0] * l.x + m[2][1] * l.y + m[2][2] * l.z;
    const float fx = pt.x + rx * s;
    const float fy = pt.y + ry * s;
    const float fz = pt.z + rz * s;
    return withinGuard(fx, st.x) && withinGuard(fy, st.y) && withinGuard(fz, st.z);
}

// Open-addressed table keyed by node pointer, retired by generation at each
// main-view activation and at Present rather than cleared. A miss after the
// probe limit just recomputes. The slot count is headroom, not a measurement:
// 48 bytes a slot, 384 KB resident whether or not the feature is on.
//
// A hit assumes the parent chain that produced the entry is unchanged. The
// key covers the node's own stored translation, not the parent's rotation,
// scale or exact translation, so a dependency that changes without moving
// the stored float returns the exact value of the earlier pose. Retiring per
// scene keeps that window to one scene, and the error is the accumulated
// float rounding of the chain, the same as not having the feature.
constexpr unsigned CACHE_SLOTS = 8192;
constexpr unsigned CACHE_PROBE_LIMIT = 16;

struct CacheEntry {
    const NI::AVObject* node;
    unsigned generation;
    float stored[3];
    double exact[3];
};

CacheEntry cache[CACHE_SLOTS];
unsigned cacheGeneration = 1;

void retireCache() {
    if (++cacheGeneration == 0) {
        ++cacheGeneration;
    }
}

CacheEntry* cacheSlot(const NI::AVObject* node) {
    const std::uintptr_t key = reinterpret_cast<std::uintptr_t>(node);
    unsigned index = static_cast<unsigned>((key >> 4) * 2654435761u) & (CACHE_SLOTS - 1);
    for (unsigned probe = 0; probe < CACHE_PROBE_LIMIT; ++probe) {
        CacheEntry& entry = cache[index];
        if (entry.generation != cacheGeneration || entry.node == node) {
            return &entry;
        }
        index = (index + 1) & (CACHE_SLOTS - 1);
    }
    return nullptr;
}

bool exactWorldTranslationUnguarded(const NI::AVObject* node, double out[3], int depth) {
    if (!node || depth > MAX_CHAIN_DEPTH) {
        return false;
    }

    const NI::Point3& stored = node->worldTransform.translation;
    CacheEntry* slot = cacheSlot(node);
    if (slot && slot->generation == cacheGeneration && slot->node == node
        && slot->stored[0] == stored.x && slot->stored[1] == stored.y && slot->stored[2] == stored.z) {
        out[0] = slot->exact[0];
        out[1] = slot->exact[1];
        out[2] = slot->exact[2];
        return true;
    }

    double t[3];
    const NI::AVObject* parent = node->parentNode;
    double pt[3];
    if (parent && storedFollowsParent(node, parent) && exactWorldTranslationUnguarded(parent, pt, depth + 1)) {
        // world.t(child) = world.t(parent) + world.R(parent) * (local.t(child) * world.s(parent))
        const double s = parent->worldTransform.scale;
        const double lx = node->localTranslate.x * s;
        const double ly = node->localTranslate.y * s;
        const double lz = node->localTranslate.z * s;
        const float (*m)[3] = parent->worldTransform.rotation.m;
        t[0] = pt[0] + static_cast<double>(m[0][0]) * lx + static_cast<double>(m[0][1]) * ly + static_cast<double>(m[0][2]) * lz;
        t[1] = pt[1] + static_cast<double>(m[1][0]) * lx + static_cast<double>(m[1][1]) * ly + static_cast<double>(m[1][2]) * lz;
        t[2] = pt[2] + static_cast<double>(m[2][0]) * lx + static_cast<double>(m[2][1]) * ly + static_cast<double>(m[2][2]) * lz;
    } else {
        // A root, or a node the engine placed without its parent chain: its
        // stored translation is the only truth. Anchor there; descendants
        // still get exact offsets from it.
        t[0] = stored.x;
        t[1] = stored.y;
        t[2] = stored.z;
    }

    if (slot) {
        slot->node = node;
        slot->generation = cacheGeneration;
        slot->stored[0] = stored.x;
        slot->stored[1] = stored.y;
        slot->stored[2] = stored.z;
        slot->exact[0] = t[0];
        slot->exact[1] = t[1];
        slot->exact[2] = t[2];
    }

    out[0] = t[0];
    out[1] = t[1];
    out[2] = t[2];
    return true;
}

// The node pointer is recovered from a transform pointer the engine handed
// us, so a wrong caller would fault; the guard turns that into a fallback.
bool exactWorldTranslation(const NI::AVObject* node, double out[3]) {
    __try {
        return exactWorldTranslationUnguarded(node, out, 0);
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        return false;
    }
}

const NI::AVObject* nodeFromWorldTransform(const NI::Transform* transform) {
    return reinterpret_cast<const NI::AVObject*>(
        reinterpret_cast<const char*>(transform) - offsetof(NI::AVObject, worldTransform));
}

//---------------------------------------------------------------------------
// First-person eye
//
// In first person the engine copies the stored (rounded) world position of the
// "Camera" node into the world, arm and shadow camera roots
// (PlayerAnimController::updateCameraTransforms). The skeleton can move again
// before the scene renders, and other code (crouch, MWSE camera mods) may move
// the camera afterwards, so the copy is paired with the exact position of the
// node at the moment it is made, together with the roots it went into. A
// camera that hangs from one of those roots then has the exact position of
// the pair plus the float offset between its location and the copy; any other
// camera uses its own parent chain.
//---------------------------------------------------------------------------

constexpr int EYE_MAX_PARENT_DEPTH = 3;

struct EyePair {
    bool valid;
    const NI::AVObject* roots[CAMERA_ROOT_COUNT];  // compared by value, never read
    float stored[3];
    double exact[3];
};
EyePair eyePair = {};

void captureEyeUnguarded(const char* playerAnimController) {
    eyePair.valid = false;
    const NI::AVObject* head = *reinterpret_cast<const NI::AVObject* const*>(
        playerAnimController + PLAYER_ANIM_CONTROLLER_HEAD_CAMERA_OFFSET);
    if (!head) {
        return;
    }
    const DWORD worldController = MWPatches::read_dword(WORLD_CONTROLLER_POINTER);
    if (!worldController) {
        return;
    }
    for (int i = 0; i < CAMERA_ROOT_COUNT; ++i) {
        eyePair.roots[i] = reinterpret_cast<const NI::AVObject*>(
            MWPatches::read_dword(worldController + WORLD_CONTROLLER_CAMERA_ROOT_OFFSETS[i]));
    }
    const NI::AVObject* worldRoot = eyePair.roots[0];
    if (!worldRoot) {
        return;
    }

    double exact[3];
    if (!exactWorldTranslationUnguarded(head, exact, 0)) {
        return;
    }
    const NI::Point3& headStored = head->worldTransform.translation;
    const NI::Point3& eye = worldRoot->localTranslate;
    eyePair.stored[0] = eye.x;
    eyePair.stored[1] = eye.y;
    eyePair.stored[2] = eye.z;
    eyePair.exact[0] = exact[0] + (static_cast<double>(eye.x) - headStored.x);
    eyePair.exact[1] = exact[1] + (static_cast<double>(eye.y) - headStored.y);
    eyePair.exact[2] = exact[2] + (static_cast<double>(eye.z) - headStored.z);
    eyePair.valid = true;
}

void __fastcall patchUpdateCameraTransforms(void* playerAnimController, void* /*edx*/) {
    engineUpdateCameraTransforms(playerAnimController);
    if (!Configuration.EnableCameraRelativeRendering) {
        eyePair.valid = false;
        return;
    }
    __try {
        captureEyeUnguarded(static_cast<const char*>(playerAnimController));
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        eyePair.valid = false;
    }
}

// The NiCamera whose world translation SetCameraData received. NiCamera::Click
// passes &camera->worldTransform.translation, and the renderer slot has no
// other caller in the engine. The pointer is only compared, never read,
// until cameraIsOwned has matched it against the engine's own cameras.
const NI::AVObject* cameraFromLocation(const float* worldLocation) {
    return reinterpret_cast<const NI::AVObject*>(
        reinterpret_cast<const char*>(worldLocation)
        - offsetof(NI::AVObject, worldTransform) - offsetof(NI::Transform, translation));
}

// Whether the scene this camera renders is one this module converts: the
// engine's world camera or its first-person arm camera. The menu, splash,
// shadow, reflection and any mod-created camera render scenes that stay
// absolute, and so does the identity view NiDX8Renderer::RenderScreenPoly
// (0x6ADD20) sets around loading-screen polygons, which belongs to no pose.
bool cameraIsOwned(const NI::AVObject* camera) {
    const DWORD worldController = MWPatches::read_dword(WORLD_CONTROLLER_POINTER);
    if (!worldController) {
        return false;
    }
    for (DWORD offset : WORLD_CONTROLLER_OWNED_CAMERA_OFFSETS) {
        if (reinterpret_cast<std::uintptr_t>(camera) == MWPatches::read_dword(worldController + offset)) {
            return true;
        }
    }
    return false;
}

// Whether the camera hangs from a root the engine wrote the copy into. The
// roots are compared by value only, so a stale pair simply fails to match.
bool hangsFromCopiedRoot(const NI::AVObject* camera) {
    const NI::AVObject* node = camera->parentNode;
    for (int depth = 0; node && depth < EYE_MAX_PARENT_DEPTH; ++depth) {
        for (int i = 0; i < CAMERA_ROOT_COUNT; ++i) {
            if (eyePair.roots[i] && eyePair.roots[i] == node) {
                return true;
            }
        }
        node = node->parentNode;
    }
    return false;
}

bool eyeExactUnguarded(const NI::AVObject* camera, const float* worldLocation, double out[3]) {
    if (eyePair.valid && hangsFromCopiedRoot(camera)) {
        out[0] = eyePair.exact[0] + (static_cast<double>(worldLocation[0]) - eyePair.stored[0]);
        out[1] = eyePair.exact[1] + (static_cast<double>(worldLocation[1]) - eyePair.stored[1]);
        out[2] = eyePair.exact[2] + (static_cast<double>(worldLocation[2]) - eyePair.stored[2]);
        return true;
    }
    return exactWorldTranslationUnguarded(camera, out, 0);
}

// Exact position of an owned camera; false when its chain cannot be walked.
bool eyeExact(const NI::AVObject* camera, const float* worldLocation, double out[3]) {
    __try {
        return eyeExactUnguarded(camera, worldLocation, out);
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        return false;
    }
}

//---------------------------------------------------------------------------
// Camera pose
//---------------------------------------------------------------------------

struct Pose {
    bool valid = false;
    bool owned = false;  // world or first-person camera
    bool exactValid = false;
    float location[3] = {};
    double exactLocation[3] = {};
    float direction[3] = {};
    float up[3] = {};
    float right[3] = {};
};

Pose pose;

void __fastcall hookSetCameraData(
    void* renderer, void* /*edx*/, const float* worldLocation, const float* worldDirection,
    const float* worldUp, const float* worldRight, const void* frustum, const void* viewport) {
    if (worldLocation && worldDirection && worldUp && worldRight) {
        std::memcpy(pose.location, worldLocation, sizeof(pose.location));
        std::memcpy(pose.direction, worldDirection, sizeof(pose.direction));
        std::memcpy(pose.up, worldUp, sizeof(pose.up));
        std::memcpy(pose.right, worldRight, sizeof(pose.right));
        pose.valid = true;
        pose.owned = false;
        pose.exactValid = false;

        if (Configuration.EnableCameraRelativeRendering) {
            const NI::AVObject* camera = cameraFromLocation(worldLocation);
            pose.owned = cameraIsOwned(camera);
            if (pose.owned) {
                pose.exactValid = eyeExact(camera, worldLocation, pose.exactLocation);
            }
        }
    } else {
        pose.valid = false;
        pose.owned = false;
        pose.exactValid = false;
    }

    originalSetCameraData(renderer, worldLocation, worldDirection, worldUp, worldRight, frustum, viewport);
}

//---------------------------------------------------------------------------
// Relative-space state
//---------------------------------------------------------------------------

bool isActive = false;
double origin[3] = {};
D3DXMATRIX viewRotationOnly;   // recorder view while active
D3DXMATRIX viewAbsoluteBase;   // absolute view rebuilt from the pose, no camera effects
D3DXMATRIX viewAbsoluteEffects;
bool loggedActivation = false;
bool loggedLateEnable = false;

// The engine writes the view rotation straight from the pose basis
// (NiDX8Renderer::SetCameraData), so a bitwise comparison is the right test:
// any other view that reaches the proxy is not this pose. Both sides are
// direct loads with no arithmetic between them, so /fp:fast cannot perturb
// the comparison.
bool viewMatchesPose(const D3DMATRIX* view) {
    return view->_11 == pose.right[0] && view->_12 == pose.up[0] && view->_13 == pose.direction[0]
        && view->_21 == pose.right[1] && view->_22 == pose.up[1] && view->_23 == pose.direction[1]
        && view->_31 == pose.right[2] && view->_32 == pose.up[2] && view->_33 == pose.direction[2];
}

double dot3(const float* a, const double* b) {
    return static_cast<double>(a[0]) * b[0]
         + static_cast<double>(a[1]) * b[1]
         + static_cast<double>(a[2]) * b[2];
}

void activate(const D3DMATRIX* engineView) {
    if (pose.exactValid) {
        origin[0] = pose.exactLocation[0];
        origin[1] = pose.exactLocation[1];
        origin[2] = pose.exactLocation[2];
    } else {
        origin[0] = pose.location[0];
        origin[1] = pose.location[1];
        origin[2] = pose.location[2];
    }

    viewRotationOnly = *engineView;
    viewRotationOnly._41 = 0.0f;
    viewRotationOnly._42 = 0.0f;
    viewRotationOnly._43 = 0.0f;
    viewRotationOnly._44 = 1.0f;

    // Same construction as the engine, but the dot products are done in double
    // and rounded once, instead of on float inputs already at world magnitude.
    viewAbsoluteBase = viewRotationOnly;
    viewAbsoluteBase._41 = static_cast<float>(-dot3(pose.right, origin));
    viewAbsoluteBase._42 = static_cast<float>(-dot3(pose.up, origin));
    viewAbsoluteBase._43 = static_cast<float>(-dot3(pose.direction, origin));
    viewAbsoluteEffects = viewAbsoluteBase;

    // Entries from the previous scene (or the eye capture before this one)
    // may describe a pose the engine has since rewritten in place.
    retireCache();

    isActive = true;
    if (!loggedActivation) {
        LOG::logline("-- Camera-relative rendering active (camera at %.1f, %.1f, %.1f).", origin[0], origin[1], origin[2]);
        loggedActivation = true;
    }
}

//---------------------------------------------------------------------------
// Fixed-function lights
//---------------------------------------------------------------------------

// Light ids belong to NiLight objects and grow over a session, and the
// lights of an unloaded cell are never enabled again. The records are
// capped; at capacity the least recently uploaded or enabled light is
// evicted. A live light loses its record only if more than the limit of
// other lights are touched between two of its own touches, and then keeps
// the device's last position until the engine uploads it again.
constexpr std::size_t LIGHT_RECORD_LIMIT = 1024;

struct LightUpload {
    D3DLIGHT8 absolute;
    bool relative;            // space the device holds it in
    double origin[3];         // origin subtracted, when relative
    std::uint64_t lastTouch;  // upload or enable order
};

std::unordered_map<DWORD, LightUpload> lightUploads;
std::uint64_t lightTouchCounter = 0;

LightUpload& lightRecord(DWORD index) {
    auto found = lightUploads.find(index);
    if (found == lightUploads.end()) {
        if (lightUploads.size() >= LIGHT_RECORD_LIMIT) {
            auto oldest = lightUploads.begin();
            for (auto it = lightUploads.begin(); it != lightUploads.end(); ++it) {
                if (it->second.lastTouch < oldest->second.lastTouch) {
                    oldest = it;
                }
            }
            lightUploads.erase(oldest);
        }
        found = lightUploads.emplace(index, LightUpload{}).first;
    }
    found->second.lastTouch = ++lightTouchCounter;
    return found->second;
}

//---------------------------------------------------------------------------
// Draw hooks
//---------------------------------------------------------------------------

// The transform pointer of the geometry currently inside RenderShape or
// RenderTriStrips. Both Display paths pass &geometry->worldTransform, which
// is the only way SetModelTransform can learn which node it is placing.
NI::Transform* currentGeometryTransform = nullptr;

// Set immediately before an engine call that will issue SetTransform with a
// matrix this module already made camera-relative; consumed by the proxy.
// One bit is enough because each producer sets it around a single engine
// call and both callees issue exactly one SetTransform each:
// NiDX8Renderer::SetModelTransform (0x6AC9C0) and SetBoneTransform (0x6ACB10).
bool worldRelativePending = false;

void __fastcall hookRenderShape(
    void* renderer, void* /*edx*/, void* geometryData, void* skinInstance, NI::Transform* transform, void* worldBound) {
    NI::Transform* previous = currentGeometryTransform;
    currentGeometryTransform = transform;
    originalRenderShape(renderer, geometryData, skinInstance, transform, worldBound);
    currentGeometryTransform = previous;
}

void __fastcall hookRenderTriStrips(
    void* renderer, void* /*edx*/, void* geometryData, void* skinInstance, NI::Transform* transform, void* worldBound) {
    NI::Transform* previous = currentGeometryTransform;
    currentGeometryTransform = transform;
    originalRenderTriStrips(renderer, geometryData, skinInstance, transform, worldBound);
    currentGeometryTransform = previous;
}

void __fastcall patchSetModelTransform(void* renderer, void* /*edx*/, const NI::Transform* transform) {
    if (isActive && rigidHooksInstalled && transform && transform == currentGeometryTransform) {
        double exact[3];
        if (exactWorldTranslation(nodeFromWorldTransform(transform), exact)) {
            NI::Transform relative = *transform;
            relative.translation.x = static_cast<float>(exact[0] - origin[0]);
            relative.translation.y = static_cast<float>(exact[1] - origin[1]);
            relative.translation.z = static_cast<float>(exact[2] - origin[2]);

            worldRelativePending = true;
            engineSetModelTransform(renderer, &relative);
            worldRelativePending = false;
            return;
        }
    }

    engineSetModelTransform(renderer, transform);
}

// Exact world transform of a node: stored rotation and scale, exact translation
// where the chain allows it, stored translation otherwise.
DTransform exactWorldTransform(const NI::AVObject* node) {
    DTransform out = fromNi(node->worldTransform);
    double exact[3];
    if (exactWorldTranslation(node, exact)) {
        out.t[0] = exact[0];
        out.t[1] = exact[1];
        out.t[2] = exact[2];
    }
    return out;
}

// The tail of the engine's SetModelTransform and SetSkinnedModelTransforms:
// the camera's right and up axes expressed in the model's rotated, scaled
// frame (matrix33_static::transpose_mul_vec3 of rotation * scale).
void setModelCameraAxes(void* renderer, const NI::Transform& transform) {
    NI::DX8Renderer* dx8 = static_cast<NI::DX8Renderer*>(renderer);
    const float s = transform.scale;
    const float (*m)[3] = transform.rotation.m;
    const NI::Point3 inputs[2] = { dx8->cameraRight, dx8->cameraUp };
    NI::Point3* outputs[2] = { &dx8->modelCameraRight, &dx8->modelCameraUp };
    for (int k = 0; k < 2; ++k) {
        const NI::Point3& v = inputs[k];
        outputs[k]->x = (m[0][0] * s) * v.x + (m[1][0] * s) * v.y + (m[2][0] * s) * v.z;
        outputs[k]->y = (m[0][1] * s) * v.x + (m[1][1] * s) * v.y + (m[2][1] * s) * v.z;
        outputs[k]->z = (m[0][2] * s) * v.x + (m[1][2] * s) * v.y + (m[2][2] * s) * v.z;
    }
}

void __fastcall patchSetSkinnedModelTransforms(
    void* renderer, void* /*edx*/, NI::SkinInstance* skinInstance, NI::SkinPartition::Partition* partition,
    NI::Transform* transform, void* bound) {
    const NI::SkinData* skinData = skinInstance ? skinInstance->skinData : nullptr;
    if (!isActive || !skinnedHooksInstalled || !partition || !transform || !skinData
        || !skinData->boneData || !skinInstance->rootParent || !skinInstance->bones || !partition->bones) {
        engineSetSkinnedModelTransforms(renderer, skinInstance, partition, transform, bound);
        return;
    }

    // palette[i] = shape * rootParentToSkin * inverse(rootParent) * bone * boneOffset,
    // composed in double with exact translations, then taken camera-relative.
    DTransform shape = fromNi(*transform);
    if (transform == currentGeometryTransform) {
        shape = exactWorldTransform(nodeFromWorldTransform(transform));
    }
    const DTransform rootParent = exactWorldTransform(skinInstance->rootParent);
    DTransform inverseRootParent;
    if (!invert(rootParent, &inverseRootParent)) {
        engineSetSkinnedModelTransforms(renderer, skinInstance, partition, transform, bound);
        return;
    }
    for (unsigned short i = 0; i < partition->numBones; ++i) {
        const unsigned short boneIndex = partition->bones[i];
        if (boneIndex >= skinData->numBones || !skinInstance->bones[boneIndex]) {
            engineSetSkinnedModelTransforms(renderer, skinInstance, partition, transform, bound);
            return;
        }
    }

    const DTransform skinToRoot = combine(combine(shape, fromNi(skinData->transform)), inverseRootParent);
    for (unsigned short i = 0; i < partition->numBones; ++i) {
        const unsigned short boneIndex = partition->bones[i];
        const DTransform bone = exactWorldTransform(skinInstance->bones[boneIndex]);
        const DTransform palette = combine(combine(skinToRoot, bone), fromNi(skinData->boneData[boneIndex].transform));

        NI::Transform relative;
        for (int r = 0; r < 3; ++r) {
            for (int c = 0; c < 3; ++c) {
                relative.rotation.m[r][c] = static_cast<float>(palette.r[r][c]);
            }
        }
        relative.translation.x = static_cast<float>(palette.t[0] - origin[0]);
        relative.translation.y = static_cast<float>(palette.t[1] - origin[1]);
        relative.translation.z = static_cast<float>(palette.t[2] - origin[2]);
        relative.scale = static_cast<float>(palette.s);

        worldRelativePending = true;
        engineSetBoneTransform(renderer, &relative, i);
        worldRelativePending = false;
    }

    setModelCameraAxes(renderer, *transform);
}

//---------------------------------------------------------------------------
// Installation
//---------------------------------------------------------------------------

// Both Display vtable slots and every SetModelTransform site, or none of them.
bool installRigidHooks() {
    void* original = nullptr;
    if (!patchVtableSlotEnforced(RENDER_SHAPE_SLOT, RENDER_SHAPE_ADDRESS,
            reinterpret_cast<const void*>(&hookRenderShape), "NiDX8Renderer::RenderShape", &original)) {
        return false;
    }
    originalRenderShape = reinterpret_cast<RenderShapeFn>(original);

    if (!patchVtableSlotEnforced(RENDER_TRISTRIPS_SLOT, RENDER_TRISTRIPS_ADDRESS,
            reinterpret_cast<const void*>(&hookRenderTriStrips), "NiDX8Renderer::RenderTriStrips", &original)) {
        writeVtableSlot(RENDER_SHAPE_SLOT, RENDER_SHAPE_ADDRESS);
        return false;
    }
    originalRenderTriStrips = reinterpret_cast<RenderShapeFn>(original);

    if (!patchCallSites(SET_MODEL_TRANSFORM_SITES, SET_MODEL_TRANSFORM_ADDRESS,
            reinterpret_cast<const void*>(&patchSetModelTransform))) {
        writeVtableSlot(RENDER_TRISTRIPS_SLOT, RENDER_TRISTRIPS_ADDRESS);
        writeVtableSlot(RENDER_SHAPE_SLOT, RENDER_SHAPE_ADDRESS);
        return false;
    }
    return true;
}

}  // namespace

namespace CameraRelative {

void installHooks() {
    if (installAttempted) {
        return;
    }
    installAttempted = true;

    if (!Configuration.EnableCameraRelativeRendering) {
        installSkippedByConfig = true;
        LOG::logline("-- Camera-relative rendering off (render.camera_relative); no engine hooks installed.");
        return;
    }

    void* original = nullptr;
    cameraHookInstalled = patchVtableSlotEnforced(SET_CAMERA_DATA_SLOT, SET_CAMERA_DATA_ADDRESS,
        reinterpret_cast<const void*>(&hookSetCameraData), "NiDX8Renderer::SetCameraData", &original);
    if (!cameraHookInstalled) {
        LOG::logline("!! Camera-relative rendering: feature disabled.");
        return;
    }
    originalSetCameraData = reinterpret_cast<SetCameraDataFn>(original);

    // Rigid draws: which node is being drawn, and its placement.
    rigidHooksInstalled = installRigidHooks();

    // Skinned draws: the bone palette.
    skinnedHooksInstalled = patchCallSites(SET_SKINNED_MODEL_TRANSFORMS_SITES, SET_SKINNED_MODEL_TRANSFORMS_ADDRESS,
        reinterpret_cast<const void*>(&patchSetSkinnedModelTransforms));

    // First-person eye: capture the head-node copy at the engine's camera update.
    eyeHooksInstalled = patchCallSites(UPDATE_CAMERA_TRANSFORMS_SITES, UPDATE_CAMERA_TRANSFORMS_ADDRESS,
        reinterpret_cast<const void*>(&patchUpdateCameraTransforms));

    LOG::logline("-- Camera-relative rendering: hooks installed (camera yes, rigid draws %s, skinned draws %s, first-person eye %s).",
        rigidHooksInstalled ? "yes" : "NO",
        skinnedHooksInstalled ? "yes" : "NO",
        eyeHooksInstalled ? "yes" : "NO");
}

void onViewTransform(const D3DMATRIX* engineView, bool renderTargetNormal) {
    isActive = false;

    if (!cameraHookInstalled) {
        if (installSkippedByConfig && Configuration.EnableCameraRelativeRendering && !loggedLateEnable) {
            LOG::logline("-- Camera-relative rendering was enabled at runtime; its engine hooks install at startup, "
                "so it takes effect after a restart.");
            loggedLateEnable = true;
        }
        return;
    }
    if (!Configuration.EnableCameraRelativeRendering || !renderTargetNormal || !pose.valid || !pose.owned) {
        return;
    }

    // Any other view that reaches the proxy while this pose is current (the
    // identity view around a screen polygon, for one) is not this scene and
    // stays absolute; the engine restores the pose's own view afterwards.
    if (!viewMatchesPose(engineView)) {
        return;
    }

    activate(engineView);
}

bool active() {
    return isActive;
}

const D3DXMATRIX* recorderView() {
    return &viewRotationOnly;
}

void deviceView(const D3DXMATRIX* cameraEffects, D3DXMATRIX* out) {
    D3DXMatrixMultiply(out, &viewRotationOnly, cameraEffects);
}

void setCameraEffects(const D3DXMATRIX* cameraEffects) {
    D3DXMatrixMultiply(&viewAbsoluteEffects, &viewAbsoluteBase, cameraEffects);
}

bool absoluteView(D3DXMATRIX* out) {
    if (!isActive) {
        return false;
    }
    *out = viewAbsoluteEffects;
    return true;
}

void relativeWorld(const D3DMATRIX* world, D3DXMATRIX* out) {
    *out = *world;
    out->_41 = static_cast<float>(static_cast<double>(world->_41) - origin[0]);
    out->_42 = static_cast<float>(static_cast<double>(world->_42) - origin[1]);
    out->_43 = static_cast<float>(static_cast<double>(world->_43) - origin[2]);
}

void absoluteFromRelative(const D3DMATRIX* relative, D3DXMATRIX* out) {
    *out = *relative;
    out->_41 = static_cast<float>(static_cast<double>(relative->_41) + origin[0]);
    out->_42 = static_cast<float>(static_cast<double>(relative->_42) + origin[1]);
    out->_43 = static_cast<float>(static_cast<double>(relative->_43) + origin[2]);
}

bool takeWorldRelative() {
    const bool pending = worldRelativePending;
    worldRelativePending = false;
    return pending;
}

void multiplyWorldView(const D3DXMATRIX* world, const D3DXMATRIX* view, D3DXMATRIX* out) {
    double a[4][4];
    double b[4][4];
    for (int r = 0; r < 4; ++r) {
        for (int c = 0; c < 4; ++c) {
            a[r][c] = world->m[r][c];
            b[r][c] = view->m[r][c];
        }
    }

    D3DXMATRIX result;
    for (int r = 0; r < 4; ++r) {
        for (int c = 0; c < 4; ++c) {
            const double sum = a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c] + a[r][3] * b[3][c];
            result.m[r][c] = static_cast<float>(sum);
        }
    }
    *out = result;
}

void relativePosition(const D3DVECTOR* position, D3DVECTOR* out) {
    D3DVECTOR result;
    result.x = static_cast<float>(static_cast<double>(position->x) - origin[0]);
    result.y = static_cast<float>(static_cast<double>(position->y) - origin[1]);
    result.z = static_cast<float>(static_cast<double>(position->z) - origin[2]);
    *out = result;
}

void recordLightUpload(DWORD index, const D3DLIGHT8* absolute) {
    LightUpload& upload = lightRecord(index);
    upload.absolute = *absolute;
    upload.relative = isActive;
    upload.origin[0] = origin[0];
    upload.origin[1] = origin[1];
    upload.origin[2] = origin[2];
}

bool lightUploadStale(DWORD index, D3DLIGHT8* absolute) {
    const auto found = lightUploads.find(index);
    if (found == lightUploads.end()) {
        return false;
    }
    LightUpload& upload = found->second;
    upload.lastTouch = ++lightTouchCounter;
    if (upload.absolute.Type == D3DLIGHT_DIRECTIONAL) {
        return false;
    }
    const bool stale = upload.relative != isActive
        || (isActive && (upload.origin[0] != origin[0] || upload.origin[1] != origin[1] || upload.origin[2] != origin[2]));
    if (stale) {
        *absolute = upload.absolute;
    }
    return stale;
}

void onPresent() {
    retireCache();
}

void onDeviceReleased() {
    isActive = false;
    worldRelativePending = false;
    pose.valid = false;
    pose.owned = false;
    pose.exactValid = false;
    lightUploads.clear();
    retireCache();
}

}  // namespace CameraRelative
