#pragma once

// Camera-relative rendering for the Morrowind near scene.
//
// Far from the world origin, Morrowind's world-space coordinates are large
// enough that float32 arithmetic on them rounds by a visible amount (one float
// step is 0.0625 units at 64 cells out). The rounding happens in three places
// before a vertex reaches the screen: the engine's scene-graph update rounds
// every node's world translation; the engine's view matrix and skin palette
// are composed in float at that magnitude; and MGE XE multiplied world by
// view in float. This module rebuilds all of that relative to the exact camera
// position, in double precision, at the points where the engine's float
// transforms become D3D matrices. The scene graph and every other consumer of
// world-space data are never written.
//
// Exact positions come from the scene graph's own inputs: a node's world
// translation is the sum, down its parent chain, of each local translation
// rotated and scaled by the parent's stored world rotation and scale. Local
// translations are small and exact, and rotations do not degrade with
// distance, so that sum in double is exact where the engine's stored float
// world translation is not. Results are memoized per scene, so a skeleton is
// walked once however many body parts hang from it.
//
// Space convention while the world or first-person scene is active:
//   - the real device and MGE's FFE/PPL shaders see world matrices whose
//     translation is (world - camera) and a rotation-only view;
//   - RenderedState::worldTransforms stays absolute for the passes that replay
//     Morrowind geometry against MGE's own absolute view (sky, water);
//   - DistantLand::mwView stays absolute through absoluteView().
// View space itself is unchanged: the camera is at the origin either way, so
// every consumer of view-space positions (depth, shadow receivers, lighting) is
// unaffected.

#include <d3d9.h>
#include <d3dx9.h>

#include "proxydx/d3d8header.h"

namespace CameraRelative {

// Installs the engine hooks when render.camera_relative is on: the
// NiDX8Renderer::SetCameraData vtable slot (camera pose), the
// RenderShape/RenderTriStrips slots (which node is being drawn), the
// SetModelTransform / SetSkinnedModelTransforms call sites (exact per-draw and
// per-bone positions), and the PlayerAnimController camera update call sites
// (exact first-person eye). One-shot for the process lifetime, so turning the
// option on takes a restart; turning it off applies from the next scene. Every
// site verifies what it replaces, and each hook group installs all of its
// sites or none.
void installHooks();

// Called by the proxy for every D3DTS_VIEW it receives, before the recorder
// captures it. Activates camera-relative space when the feature is enabled,
// the render target is the back buffer, the last recorded pose belongs to
// the engine's world or first-person camera, and `engineView` carries that
// pose's rotation; deactivates otherwise. Scenes are told apart by which
// NiCamera the engine clicked, never by the shape of the matrix.
void onViewTransform(const D3DMATRIX* engineView, bool renderTargetNormal);

bool active();

// The view the recorder should use while active: rotation only.
const D3DXMATRIX* recorderView();

// The view the real device should use while active: rotation only, with the
// proxy's zoom/shake matrix applied the same way the proxy applies it to the
// engine view.
void deviceView(const D3DXMATRIX* cameraEffects, D3DXMATRIX* out);

// Records the proxy's zoom/shake matrix for the active scene, so that
// absoluteView() carries it. Called once per main-view activation.
void setCameraEffects(const D3DXMATRIX* cameraEffects);

// Absolute view for MGE's own world-space passes, with camera effects
// applied, matching what GetTransform(D3DTS_VIEW) returned before this
// feature existed. Returns false while inactive.
bool absoluteView(D3DXMATRIX* out);

// Same rotation and scale as `world`, translation minus the camera position.
// The subtraction is exact: both operands are floats and it is done in double.
void relativeWorld(const D3DMATRIX* world, D3DXMATRIX* out);

// Inverse of relativeWorld, for keeping the recorder's absolute copy.
void absoluteFromRelative(const D3DMATRIX* relative, D3DXMATRIX* out);

// True, once, when the world matrix the proxy is about to receive was already
// made camera-relative by one of this module's engine hooks. The proxy must
// then skip its own subtraction.
bool takeWorldRelative();

// out = world * view in double precision, rounded once to float.
void multiplyWorldView(const D3DXMATRIX* world, const D3DXMATRIX* view, D3DXMATRIX* out);

// Light position minus the camera position, in double.
void relativePosition(const D3DVECTOR* position, D3DVECTOR* out);

// Fixed-function lights. The engine uploads a light to the device only when
// the light's own revision changes and otherwise just re-enables it, so a
// position made camera-relative at upload would stay on the device while the
// camera moved on. The proxy records every light it forwards, in absolute
// space, together with the space and origin it was uploaded under, and asks
// at each view and each LightEnable whether that still matches; when it does
// not, it uploads the recorded light again.
void recordLightUpload(DWORD index, const D3DLIGHT8* absolute);
bool lightUploadStale(DWORD index, D3DLIGHT8* absolute);

// Frame boundary: retires the per-scene position cache.
void onPresent();

// The proxy device is destroyed and recreated on fullscreen Alt-Tab. Nothing
// uploaded to the old device survives, the recorded pose is dropped, and no
// scene is active on the new one until its first owned camera is clicked.
void onDeviceReleased();

}  // namespace CameraRelative
