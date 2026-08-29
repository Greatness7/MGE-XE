#pragma once

#include "ipc/vecwrap.h"

#include <cstdint>
#include <unordered_map>
#include <vector>

// Per-subset UV-bound palettes owned by the statics loader, keyed on the subset's vertex buffer.
using StaticUvBoundPaletteMap = std::unordered_map<IDirect3DVertexBuffer9*, std::vector<D3DXVECTOR4>>;

namespace DistantLoaders {
    const StaticUvBoundPaletteMap& staticUvBoundPaletteMap();
}



// VisibleSet - Draw loop over a set of visible meshes streamed from the 64-bit host.
// Holds the batching state (buffer/texture/UV animation) that lets a run of meshes
// share device state, and iterates via IpcClientVector's windowed cursor.
class VisibleSet {
public:
    void Render(IDirect3DDevice9* device,
                unsigned int vertex_size,
                bool parallelRead = false);

    // `uv_bound_palette_handle` is non-null only for distant statics, whose vertex shaders
    // resolve an atlas rect from a shared palette array indexed by position.w. Terrain and
    // grass pass nullptr.
    void Render(IDirect3DDevice9* device,
                ID3DXEffect* effect,
                ID3DXEffect* effectPool,
                const D3DXHANDLE* texture_handle,
                const D3DXHANDLE* has_alpha_handle,
                const D3DXHANDLE* animate_uv_handle,
                const D3DXHANDLE* world_matrix_handle,
                const D3DXHANDLE* uv_bound_palette_handle,
                unsigned int vertex_size,
                bool parallelRead = false);

    void RemoveAll() {
        visible_set.clear();
    }

    void SetVector(const IpcClientVector& vector) {
        visible_set = vector;
    }

    std::uint32_t Size() const {
        return visible_set.size();
    }

    bool Empty() const {
        return visible_set.size() == 0;
    }

    void Truncate(std::uint32_t count) {
        visible_set.truncate(count);
    }

    void Reset() {
        visible_set.restart();
    }

    const RenderMesh& First() {
        return visible_set.first();
    }

    const RenderMesh& Next() {
        return visible_set.next();
    }

    bool AtEnd() {
        return visible_set.at_end();
    }

private:
    IpcClientVector visible_set;
};
