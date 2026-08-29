#include "visibleset.h"



void VisibleSet::Render(IDirect3DDevice9* device,
                        unsigned int vertex_size,
                        bool parallelRead) {
    IDirect3DVertexBuffer9* last_buffer = 0;

    if (parallelRead) {
        visible_set.start_read();
    }
    visible_set.restart();
    while (!visible_set.at_end()) {
        const RenderMesh& mesh = visible_set.next();
        if (mesh.faces <= 0) {
            continue;
        }
        if (last_buffer != mesh.vBuffer) {
            device->SetIndices(mesh.iBuffer);
            device->SetStreamSource(0, mesh.vBuffer, 0, vertex_size);
            last_buffer = mesh.vBuffer;
        }

        device->DrawIndexedPrimitive(D3DPT_TRIANGLELIST, 0, 0, mesh.verts, 0, mesh.faces);
    }

    if (parallelRead) {
        visible_set.end_read();
    }
}

void VisibleSet::Render(IDirect3DDevice9* device,
                        ID3DXEffect* effect,
                        ID3DXEffect* effectPool,
                        const D3DXHANDLE* texture_handle,
                        const D3DXHANDLE* has_alpha_handle,
                        const D3DXHANDLE* animate_uv_handle,
                        const D3DXHANDLE* world_matrix_handle,
                        const D3DXHANDLE* uv_bound_palette_handle,
                        unsigned int vertex_size,
                        bool parallelRead) {
    IDirect3DTexture9* last_texture = nullptr;
    IDirect3DVertexBuffer9* last_buffer = nullptr;
    bool last_animateUV = false;
    bool last_hasAlpha = false;

    // Identity rect, in the shader's lane order [min_v, max_u, min_u, max_v]. Bound when a
    // subset has no palette entry, so the atlas clamp degrades to a passthrough sample.
    static const D3DXVECTOR4 identityUvBound(0.0f, 1.0f, 0.0f, 1.0f);
    const StaticUvBoundPaletteMap* palettes =
        uv_bound_palette_handle ? &DistantLoaders::staticUvBoundPaletteMap() : nullptr;

    if (animate_uv_handle) {
        effectPool->SetBool(*animate_uv_handle, false);
    }
    if (has_alpha_handle) {
        effectPool->SetBool(*has_alpha_handle, false);
    }
    else {
        device->SetRenderState(D3DRS_ALPHATESTENABLE, FALSE);
    }
    effect->CommitChanges();

    if (parallelRead) {
        visible_set.start_read();
    }
    visible_set.restart();
    while (!visible_set.at_end()) {
        const RenderMesh& mesh = visible_set.next();
        if (mesh.faces <= 0) {
            continue;
        }

        if (texture_handle && last_texture != mesh.tex) {
            effectPool->SetTexture(*texture_handle, mesh.tex);
            last_texture = mesh.tex;
        }

        // Alpha classification belongs to the subset, not the texture, for the same reason
        // animateUV does below. Subsets that skip the atlas keep their source texture path,
        // which carries no alpha/opaque prefix, so two subsets can share one cached texture
        // pointer and still disagree here.
        if (mesh.hasAlpha != last_hasAlpha) {
            if (has_alpha_handle) {
                // Depth-only rendering, control if texture alpha channel reads are required in shader
                effectPool->SetBool(*has_alpha_handle, mesh.hasAlpha);
            }
            else {
                // World rendering, alpha test state is compatible with transparency supersampling, while clip() isn't
                device->SetRenderState(D3DRS_ALPHATESTENABLE, mesh.hasAlpha);
            }
            last_hasAlpha = mesh.hasAlpha;
        }

        // Set UV animation variables. Different objects may use the same texture, but animate differently
        if (animate_uv_handle && mesh.animateUV != last_animateUV) {
            effectPool->SetBool(*animate_uv_handle, mesh.animateUV);
            last_animateUV = mesh.animateUV;
        }

        if (last_buffer != mesh.vBuffer) {
            device->SetIndices(mesh.iBuffer);
            device->SetStreamSource(0, mesh.vBuffer, 0, vertex_size);
            last_buffer = mesh.vBuffer;

            // The palette is per-subset, and the vertex buffer is the subset's identity, so this
            // boundary is exactly where it changes. It already re-sets stream source, indices,
            // and usually the texture, so one small SetVectorArray rides along.
            if (palettes) {
                auto entry = palettes->find(mesh.vBuffer);
                if (entry != palettes->end() && !entry->second.empty()) {
                    effectPool->SetVectorArray(
                        *uv_bound_palette_handle,
                        entry->second.data(),
                        static_cast<UINT>(entry->second.size())
                    );
                }
                else {
                    if (mesh.vBuffer) {
                        DistantLoaders::noteMissingPalette();
                    }
                    effectPool->SetVectorArray(*uv_bound_palette_handle, &identityUvBound, 1);
                }
            }
        }

        effectPool->SetMatrix(*world_matrix_handle, &mesh.transform);

        effect->CommitChanges();
        device->DrawIndexedPrimitive(D3DPT_TRIANGLELIST, 0, 0, mesh.verts, 0, mesh.faces);
    }

    if (parallelRead) {
        visible_set.end_read();
    }

    // These are shared across every effect in the pool, so leaving them at the last mesh's
    // value leaks into whichever pass runs next, and not all of them set it first
    if (has_alpha_handle) {
        effectPool->SetBool(*has_alpha_handle, false);
    }
    if (animate_uv_handle) {
        effectPool->SetBool(*animate_uv_handle, false);
    }
}
