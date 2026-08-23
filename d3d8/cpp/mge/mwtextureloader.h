#pragma once

namespace MWTextureLoader {

// Records whether the active D3D9 runtime supports MGE XE's BC7 format.
// Must be called once device capabilities are known, before textures load.
void setBC7TextureSupport(bool supported);

// Changes texture creation from D3DPOOL_MANAGED to D3DPOOL_DEFAULT, by loading through a staging texture
// This should reduce process memory footprint by removing managed textures.
// Also extends NiDDSReader to decode BC7 (DX10 extended header) DDS textures.
void patchLoadTexture2D();

} // namespace MWTextureLoader
