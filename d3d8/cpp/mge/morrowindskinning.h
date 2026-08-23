#pragma once

// Morrowind-side engine hooks for the indexed matrix-palette skinning
// optimization. MGE XE owns these patches; they were previously carried by a
// fork of MWSE (SharedSE/NIDX8Renderer.*, SharedSE/NISkinInstance.*, and the
// indexed-skinning block of MWSE/PatchUtil.cpp) and are adapted from that
// GPLv2 implementation.
//
// The hooks rebuild Morrowind's skin partitions with an eight-entry bone
// palette and pack the resulting vertices as XYZB4 + LASTBETA_UBYTE4, which
// lets d3d8.dll draw a skinned mesh in far fewer partition draws. They stay on
// stock engine behavior unless every patch installed and MGEProxyDevice
// authorizes the feature through IMgeIndexedSkinningCaps.

namespace MorrowindIndexedSkinning {

// Installs the four engine patches. One-shot for the process lifetime: later
// calls return immediately, so device recreation does not re-patch. Each patch
// verifies the bytes it is replacing and fails closed, leaving that hook -- and
// therefore the whole feature -- on stock behavior.
void installHooks();

// True only when all four patches installed successfully. This is one input to
// the capability gate in MGEProxyDevice::GetIndexedSkinningCaps.
bool hooksInstalled();

// Called when an MGEProxyDevice reaches a zero refcount. Drops the remembered
// device pointer and releases the engine references held by the stock-partition
// fallback cache, while the Morrowind runtime is still alive to service them.
// The executable patches themselves stay installed; they are process-lifetime.
void onDeviceReleased();

}  // namespace MorrowindIndexedSkinning
