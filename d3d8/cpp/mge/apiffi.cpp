// LuaJIT-FFI entry points for live terrain horizon-culling tuning.
//
// MWSE's `mge` Lua bindings live in a separate repository, so new tunables cannot be surfaced
// there without shipping a new MWSE release. Instead the MGE XE Options menu calls these plain
// exported C functions directly through LuaJIT's FFI (see assets\Data Files\mwse\mods\MGE XE
// Options\gui.lua and docs/architecture/horizon-culling.md). Values are owned by Configuration.Horizon (so
// the existing Save button persists them); a dirty flag asks the render thread to push them to the
// 64-bit host on the next exterior frame.
//
// The id/range contract is mirrored in mgeHost64/src/config.rs and the Lua menu; keep all
// three in sync (docs/architecture/horizon-culling.md §10.2). The names are exported undecorated via
// cpp\exports.def so ffi.load("d3d8") can resolve them.

#include "configuration.h"
#include "distantland.h"
#include "mwbridge.h"
#include "waterlook.h"

#include <algorithm>
#include <cmath>

// Set by MGE_HorizonSetParam when a value changes, consumed once per frame by
// DistantLand::cullDistantStatics. MWSE Lua (the only FFI caller) runs on the main render thread,
// so the setter and the render-thread consumer never run concurrently and a plain bool suffices.
bool g_horizonDirty = false;

namespace {
    // Clamp an incoming value to the valid range for its parameter id. Mirrors the bounds in
    // mgeHost64/src/config.rs; the host clamps again, this keeps Configuration.Horizon sane locally.
    float clampHorizonParam(int id, float value) {
        switch (id) {
        case 0: return value != 0.0f ? 1.0f : 0.0f;                    // enable (0/1)
        case 1: return std::clamp(value, 0.0f, 32768.0f);              // height bias
        case 2: return std::clamp(value, 0.0f, 32768.0f);              // object bias
        case 3: return std::clamp(value, 0.0f, 65536.0f);              // near exclude
        case 4: return std::clamp(value, 1.0f, 65536.0f);              // ring step
        case 5: return std::clamp(value, 1.0f, 1048576.0f);            // max range
        case 6: return std::clamp(std::round(value), 64.0f, 4096.0f);  // azimuth bins
        case 7: return std::clamp(value, 1.0f, 8192.0f);               // sample spacing
        case 8: return value != 0.0f ? 1.0f : 0.0f;                    // adaptive gate (0/1)
        default: return value;
        }
    }
}

extern "C" {

    // Returns the current value of horizon parameter `id` (see §4 table), or 0 for unknown ids.
    // Distances are returned in world units; the menu converts to cells for display.
    float MGE_HorizonGetParam(int id) {
        const auto& h = Configuration.Horizon;
        switch (id) {
        case 0: return h.Culling ? 1.0f : 0.0f;
        case 1: return h.BiasZ;
        case 2: return h.ObjectBiasZ;
        case 3: return h.NearUnits;
        case 4: return h.RingStep;
        case 5: return h.MaxRange;
        case 6: return static_cast<float>(h.Bins);
        case 7: return h.SampleSpacing;
        case 8: return h.AdaptiveGate ? 1.0f : 0.0f;
        case 9: return h.HierarchicalMarch ? 1.0f : 0.0f;
        default: return 0.0f;
        }
    }

    // Clamps and writes horizon parameter `id`, then flags the change for the next host push.
    // Unknown ids are ignored and do not mark the config dirty.
    void MGE_HorizonSetParam(int id, float value) {
        auto& h = Configuration.Horizon;
        const float clamped = clampHorizonParam(id, value);
        switch (id) {
        case 0: h.Culling = clamped != 0.0f; break;
        case 1: h.BiasZ = clamped; break;
        case 2: h.ObjectBiasZ = clamped; break;
        case 3: h.NearUnits = clamped; break;
        case 4: h.RingStep = clamped; break;
        case 5: h.MaxRange = clamped; break;
        case 6: h.Bins = static_cast<DWORD>(clamped); break;
        case 7: h.SampleSpacing = clamped; break;
        case 8: h.AdaptiveGate = clamped != 0.0f; break;
        default: return;
        }
        g_horizonDirty = true;
    }

    // Replaces the set of water volumes. `volumes` is `count` boxes of six floats each:
    // min x, y, z then max x, y, z. Called on the main thread by the mod that owns the volumes.
    void MGE_WaterVolumesSet(const MWBridge::WaterVolume* volumes, unsigned int count) {
        MWBridge::get()->setWaterVolumes(volumes, count);
    }

    // Sets the look of a slot, which a surface names in the specular power of its material
    // (100000 + slot). A null look clears the slot. Called on the main thread by the mod that
    // owns the volumes.
    void MGE_WaterLookSet(unsigned int slot, const WaterLook* look) {
        WaterLooks::set(slot, look);
    }

    // Replaces the dry spaces: closed meshes inside which there is no water. `corners` is
    // three points of three floats for each of `triangles` triangles, in the world. Called on
    // the main thread by the mod that owns the volumes, when the set changes.
    void MGE_WaterMasksSet(const float* corners, unsigned int triangles) {
        DistantLand::setWaterMasks(corners, triangles);
    }

    // Says whether the camera is in a dry space. There it is not under water, however deep
    // under the surface it is. Called on the main thread when that changes.
    void MGE_WaterDrySet(int dry) {
        MWBridge::get()->setCameraDry(dry != 0);
        DistantLand::cameraInDrySpace = dry != 0;
    }

}
