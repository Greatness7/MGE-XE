
#include "distantland.h"
#include "distantshader.h"
#include "configuration.h"
#include "doublesurface.h"
#include "mwbridge.h"
#include "postshaders.h"
#include "ffeshader.h"
#include "waterlook.h"

#include <cmath>



void DistantLand::renderWaterReflection(const D3DXMATRIX* view, const D3DXMATRIX* proj) {
    auto mwBridge = MWBridge::get();
    const bool reflectStatics = isDistantCell() && staticsUploaded && (Configuration.MGEFlags & REFLECT_NEAR);

    // The shadow pass streams through the reflected-static vector in parallel. Preserve the
    // previous complete reflection until that RPC finishes rather than clearing either resource.
    if (reflectStatics && ipcClient.pollRpcCompletion() != IPC::Complete) {
        return;
    }

    // Switch to render target
    RenderTargetSwitcher rtsw(texReflection, surfReflectionZ);
    device->Clear(0, NULL, D3DCLEAR_TARGET | D3DCLEAR_ZBUFFER, horizonCol, 1.0, 0);

    // Calculate reflected view matrix, mirror plane at water mesh level
    D3DXMATRIX reflView;
    D3DXPLANE plane(0, 0, 1.0f, -(mwBridge->WaterLevel() - 1.0f));
    D3DXMatrixReflect(&reflView, &plane);
    D3DXMatrixMultiply(&reflView, &reflView, view);
    effect->SetMatrix(ehView, &reflView);

    // Calculate new projection
    D3DXMATRIX reflProj = *proj;
    editProjectionZ(&reflProj, 4.0, Configuration.DL.DrawDist * kCellSize);
    effect->SetMatrix(ehProj, &reflProj);

    // Clipping setup
    D3DXMATRIX clipMat;

    // Clip geometry on opposite side of water plane
    plane *= mwBridge->IsUnderwater(eyePos.z) ? -1.0f : 1.0f;

    // If using dynamic ripples, the water level can be lowered by up to 0.5 * waveheight
    // so move clip plane downwards at the cost of some reflection errors
    if (Configuration.MGEFlags & DYNAMIC_RIPPLES) {
        plane.d += 0.5f * Configuration.DL.WaterWaveHeight;
    }

    // Doing inverses separately is a lot more numerically stable
    D3DXMatrixInverse(&clipMat, 0, &reflView);
    D3DXMatrixTranspose(&clipMat, &clipMat);
    D3DXPlaneTransform(&plane, &plane, &clipMat);

    D3DXMatrixInverse(&clipMat, 0, &reflProj);
    D3DXMatrixTranspose(&clipMat, &clipMat);
    D3DXPlaneTransform(&plane, &plane, &clipMat);

    if (visDistantShared.Empty()) {
        // Workaround for a Direct3D bug with clipping planes, where SetClipPlane
        // has no effect on the shader pipeline if the last rendered draw call was using
        // the fixed function pipeline. This is usually covered by distant statics, but
        // not in compact interiors where all distant statics may be culled.
        // Provoking a DrawPrimitive with shader here makes the following SetClipPlane work.
        effect->BeginPass(PASS_WORKAROUND);
        device->SetVertexDeclaration(WaterDecl);
        device->SetStreamSource(0, vbFullFrame, 0, 12);
        device->DrawPrimitive(D3DPT_TRIANGLESTRIP, 0, 2);
        effect->EndPass();
    }

    device->SetClipPlane(0, plane);
    device->SetRenderState(D3DRS_CLIPPLANEENABLE, 1);

    // Rendering
    if (mwBridge->IsExterior() && (Configuration.MGEFlags & REFLECTIVE_WATER)) {
        // Draw land reflection, with opposite culling
        effect->BeginPass(PASS_RENDERLANDREFL);
        device->SetRenderState(D3DRS_CULLMODE, D3DCULL_CCW);
        renderDistantLand(effect, &reflView, &reflProj);
        effect->EndPass();
    }

    if (reflectStatics) {
        // Draw statics reflection, with opposite culling and no dissolve
        DWORD p = (mwBridge->IntLikeExterior(true) && !mwBridge->IsUnderwater(eyePos.z)) ? PASS_RENDERSTATICSEXTERIOR : PASS_RENDERSTATICSINTERIOR;
        effect->SetFloat(ehNearViewRange, 0);
        effect->BeginPass(p);
        device->SetRenderState(D3DRS_CULLMODE, D3DCULL_CCW);
        renderReflectedStatics(&reflView, &reflProj);
        effect->EndPass();
        effect->SetFloat(ehNearViewRange, nearViewRange);
    }

    if ((Configuration.MGEFlags & REFLECT_SKY) && !recordSky.empty() && !mwBridge->IsUnderwater(eyePos.z)) {
        // Draw sky reflection, with opposite culling
        renderReflectedSky();
    }

    // Restore view state
    device->SetRenderState(D3DRS_CLIPPLANEENABLE, 0);
    effect->SetMatrix(ehView, view);
    effect->SetMatrix(ehProj, proj);
}

void DistantLand::renderReflectedSky() {
    // Sky objects are not correctly positioned at infinity, so correction is required
    const float adjustZ = -2.0f * eyePos.z;
    D3DXMATRIX skyScale, worldTransform;
    D3DXMatrixScaling(&skyScale, 1e6, 1e6, 1e6);

    // Recorded renders
    const auto& recordSky_const = recordSky;
    const int standardCloudVerts = 65, standardCloudTris = 112;
    const int standardMoonVerts = 4, standardMoonTris = 2;

    // Render sky without clouds first
    effect->BeginPass(PASS_RENDERSKY);
    device->SetRenderState(D3DRS_CULLMODE, D3DCULL_CCW);

    for (const auto& i : recordSky_const) {
        // Skip clouds
        if (i.texture && i.vertCount == standardCloudVerts && i.primCount == standardCloudTris) {
            continue;
        }

        // Adjust world transform, as skydome objects are positioned relative to the viewer
        worldTransform = i.worldTransforms[0];
        worldTransform._43 += adjustZ;
        if (i.texture == nullptr) {
            // Inflate sky mesh towards infinity, makes skypos in shader calculate correctly
            D3DXMatrixMultiply(&worldTransform, &skyScale, &worldTransform);
        }

        // Set variables in main effect; variables are shared via effect pool
        effect->SetTexture(ehTex0, i.texture);
        if (i.texture) {
            // Textured object; draw as normal in shader, with exceptions:
            // - Sun/moon billboards do not use mipmaps
            // - Moon shadow cutout (prevents stars shining through moons)
            //   which requires colour to be replaced with atmosphere scattering colour
            bool isBillboard = (i.vertCount == standardMoonVerts && i.primCount == standardMoonTris);
            bool isMoonShadow = i.destBlend == D3DBLEND_INVSRCALPHA && !i.useLighting;

            effect->SetBool(ehHasAlpha, true);
            effect->SetBool(ehHasBones, isBillboard);
            effect->SetBool(ehHasVCol, isMoonShadow);
            device->SetRenderState(D3DRS_ALPHABLENDENABLE, 1);
            device->SetRenderState(D3DRS_SRCBLEND, i.srcBlend);
            device->SetRenderState(D3DRS_DESTBLEND, i.destBlend);
            device->SetRenderState(D3DRS_ALPHATESTENABLE, 1);
        } else {
            // Sky; perform atmosphere scattering in shader
            effect->SetBool(ehHasAlpha, false);
            effect->SetBool(ehHasVCol, true);
            device->SetRenderState(D3DRS_ALPHABLENDENABLE, 0);
            device->SetRenderState(D3DRS_ALPHATESTENABLE, 0);
        }

        effect->SetMatrix(ehWorld, &worldTransform);
        effect->CommitChanges();

        device->SetStreamSource(0, i.vb, i.vbOffset, i.vbStride);
        device->SetIndices(i.ib);
        device->SetFVF(i.fvf);
        device->DrawIndexedPrimitive(i.primType, i.baseIndex, i.minIndex, i.vertCount, i.startIndex, i.primCount);
    }
    effect->EndPass();

    // Render clouds with a separate shader
    effect->BeginPass(PASS_RENDERCLOUDS);
    device->SetRenderState(D3DRS_CULLMODE, D3DCULL_CCW);

    for (const auto& i : recordSky_const) {
        // Clouds only
        if (!(i.texture && i.vertCount == standardCloudVerts && i.primCount == standardCloudTris)) {
            continue;
        }

        // Adjust world transform, as skydome objects are positioned relative to the viewer
        worldTransform = i.worldTransforms[0];
        worldTransform._43 += adjustZ;

        effect->SetTexture(ehTex0, i.texture);
        effect->SetBool(ehHasAlpha, true);
        device->SetRenderState(D3DRS_ALPHABLENDENABLE, 1);
        device->SetRenderState(D3DRS_SRCBLEND, i.srcBlend);
        device->SetRenderState(D3DRS_DESTBLEND, i.destBlend);
        device->SetRenderState(D3DRS_ALPHATESTENABLE, 1);
        effect->SetMatrix(ehWorld, &worldTransform);
        effect->CommitChanges();

        device->SetStreamSource(0, i.vb, i.vbOffset, i.vbStride);
        device->SetIndices(i.ib);
        device->SetFVF(i.fvf);
        device->DrawIndexedPrimitive(i.primType, i.baseIndex, i.minIndex, i.vertCount, i.startIndex, i.primCount);
    }
    effect->EndPass();
}

void DistantLand::renderReflectedStatics(const D3DXMATRIX* view, const D3DXMATRIX* proj) {
    // Select appropriate static clipping distance
    D3DXMATRIX ds_proj = *proj, ds_viewproj;
    const float nearStaticEnd = Configuration.DL.NearStaticEnd * kCellSize;
    const float farStaticEnd = Configuration.DL.FarStaticEnd * kCellSize;
    float zn = 4.0f, zf = nearStaticEnd;

    // Don't draw beyond fully fogged distance; early out if frustum is empty
    zf = std::min(fogEnd, zf);
    if (zf <= zn) {
        return;
    }

    // Create a clipping frustum for visibility determination
    editProjectionZ(&ds_proj, zn, zf);
    ds_viewproj = (*view) * ds_proj;

    // Cull sort and draw
    ViewFrustum range_frustum(&ds_viewproj);
    // Keep the real camera eye here: the gate combines main and reflected queries, and a mirrored
    // eye would corrupt its movement and resume-probe signals.
    // zf clips view-space z, but the host tests the sphere radially. Scale by the frustum corner
    // ray so the sphere stays a conservative bound instead of cutting off edge statics.
    const float tanX = 1.0f / proj->_11;
    const float tanY = 1.0f / proj->_22;
    const float cornerRay = std::sqrt(1.0f + tanX * tanX + tanY * tanY);
    D3DXVECTOR4 viewsphere(eyePos.x, eyePos.y, eyePos.z, zf * cornerRay);

    visExtraShared.RemoveAll();
    if (!ipcClient.getVisibleMeshes(visExtraSharedId, range_frustum, viewsphere, VIS_STATIC, nearStaticEnd, farStaticEnd, VisibleSetSort::ByState)) {
        return;
    }

    device->SetVertexDeclaration(StaticDecl);

    if (ipcClient.waitForCompletion() != IPC::Complete) {
        return;
    }
    visExtraShared.Render(device, effect, effect, &ehTex0, nullptr, &ehHasVCol, &ehWorld, &ehUvBoundPalette, SIZEOFSTATICVERT);
}

void DistantLand::clearReflection() {
    auto mwBridge = MWBridge::get();
    IDirect3DSurface9* target;
    DWORD baseColour;

    texReflection->GetSurfaceLevel(0, &target);
    if (mwBridge->IntLikeExterior(true) || mwBridge->IsUnderwater(eyePos.z)) {
        // Use fog colour as reflection
        baseColour = (DWORD)horizonCol;
    } else {
        // Interior fog colour is typically too bright
        // Guess a reflection colour based on cell lighting parameters
        const BYTE* sun = mwBridge->getInteriorSun();
        RGBVECTOR c(sun[0] / 255.0f, sun[1] / 255.0f, sun[2] / 255.0f);
        c += ambCol;
        baseColour = (DWORD)c;
    }
    device->ColorFill(target, 0, baseColour);
    target->Release();
}

void DistantLand::simulateDynamicWaves() {
    auto mwBridge = MWBridge::get();

    static bool resetRippleSurface = true;
    static float remainingWaveTime = 0;
    static const float waveStep = 0.0125f;  // time per wave simulation step (1/80 sec)

    // Simulation paused in menu mode
    if (mwBridge->IsMenu()) {
        return;
    }

    device->SetFVF(fvfWave);
    device->SetStreamSource(0, vbWaveSim, 0, 32);

    // Calc number of wave iterations to run this frame
    float frameTime = std::min(mwBridge->frameTime(), 0.5f);
    remainingWaveTime += frameTime;
    int numWaveSteps = (int)(remainingWaveTime / waveStep);
    remainingWaveTime -= numWaveSteps * waveStep;

    // Preciptation (rain/snow) ripples
    if (mwBridge->IntLikeExterior(true)) {
        static float remainingRipples = 0;

        // Reset surface when not needed next time
        resetRippleSurface = true;

        // Weather types: rain = 4; thunderstorm = 5; snow = 8; blizzard = 9
        // Thunderstorm causes 50% more ripples
        int w0 = mwBridge->GetCurrentWeather(), w1 = mwBridge->GetNextWeather();
        float precipitation0 = (w0 == 4 || w0 == 5 || w0 == 8 || w0 == 9) ? 1.0f : -1.5f;
        float precipitation1 = (w1 == 4 || w1 == 5 || w1 == 8 || w1 == 9) ? 1.0f : -1.5f;
        precipitation0 += (w0 == 5) ? 0.5f : 0;
        precipitation1 += (w1 == 5) ? 0.5f : 0;

        // 150 drops per second for normal precipitation
        float precipitation = (1.0f - mwBridge->GetWeatherRatio()) * precipitation0 + mwBridge->GetWeatherRatio() * precipitation1;
        float rippleFrequency = 150.0f * precipitation;

        if (rippleFrequency > 0) {
            static double randomizer = 0.546372819;
            int ripplePos[2];
            RECT drop;

            remainingRipples += rippleFrequency * frameTime;
            int n = int(std::floor(remainingRipples));
            remainingRipples -= n;

            while (n-- > 0) {
                // Place rain ripple at random location
                for (int i = 0; i != 2; ++i) {
                    randomizer = randomizer * (1337.134511337451 + 0.0001 * rand()) + 0.12351523;
                    randomizer -= floor(randomizer);
                    ripplePos[i] = (int)(randomizer * waveTexResolution);
                }

                drop.left = ripplePos[0] - 2;
                drop.right = ripplePos[0] + 2;
                drop.top = ripplePos[1] - 1;
                drop.bottom = ripplePos[1] + 1;
                device->ColorFill(surfRain, &drop, 0x6060);

                drop.left = ripplePos[0] - 1;
                drop.right = ripplePos[0] + 1;
                drop.top = ripplePos[1] - 2;
                drop.bottom = ripplePos[1] + 2;
                device->ColorFill(surfRain, &drop, 0x6060);

                drop.left = ripplePos[0] - 1;
                drop.right = ripplePos[0] + 1;
                drop.top = ripplePos[1] - 1;
                drop.bottom = ripplePos[1] + 1;
                device->ColorFill(surfRain, &drop, 0x4040);
            }
        }

        // Apply wave equation numWaveSteps times
        // Uses double buffering to avoid reads and writes on the same target
        RenderTargetSwitcher rtsw(surfRippleBuffer, NULL);
        SurfaceDoubleBuffer doublebuffer;
        doublebuffer.init(texRain, surfRain, texRippleBuffer, surfRippleBuffer);

        effect->BeginPass(PASS_WAVESTEP);
        for (int i = 0; i != numWaveSteps; ++i) {
            device->SetRenderTarget(0, doublebuffer.sinkSurface());
            effect->SetTexture(ehTex4, doublebuffer.sourceTexture());
            effect->CommitChanges();

            device->DrawPrimitive(D3DPT_TRIANGLESTRIP, 0, 1);
            doublebuffer.cycle();
        }
        effect->EndPass();

        if (doublebuffer.sourceSurface() != surfRain) {
            device->StretchRect(surfRippleBuffer, 0, surfRain, 0, D3DTEXF_NONE);
        }
    } else if (resetRippleSurface) {
        // No weather - clear rain ripples
        device->ColorFill(surfRain, NULL, 0);
        resetRippleSurface = false;
    }

    // Player local ripples
    // Move ripple texture with player; lock to texel alignment to prevent visible jitter
    const D3DXVECTOR3* playerPos = (const D3DXVECTOR3*)mwBridge->PlayerPositionPointer();
    if (playerPos == nullptr) {
        return;
    }
    static int lastXpos = (int)floor(playerPos->x / waveTexWorldRes);
    static int lastYpos = (int)floor(playerPos->y / waveTexWorldRes);

    int newXpos = (int)floor(playerPos->x / waveTexWorldRes);
    int newYpos = (int)floor(playerPos->y / waveTexWorldRes);

    int shiftX = newXpos - lastXpos;
    int shiftY = newYpos - lastYpos;

    lastXpos = newXpos;
    lastYpos = newYpos;

    int shiftXp = (shiftX > 0) ? +shiftX : 0;
    int shiftXn = (shiftX < 0) ? -shiftX : 0;
    int shiftYp = (shiftY > 0) ? +shiftY : 0;
    int shiftYn = (shiftY < 0) ? -shiftY : 0;

    // Shift texture by (shiftX, shiftY) pixels
    RECT source;
    source.left = 1 + shiftXp;
    source.right = waveTexResolution - shiftXn;
    source.top = 1 + shiftYp;
    source.bottom = waveTexResolution - shiftYn;

    RECT target;
    target.left = 1 + shiftXn;
    target.right = waveTexResolution - shiftXp;
    target.top = 1 + shiftYn;
    target.bottom = waveTexResolution - shiftYp;

    device->ColorFill(surfRippleBuffer, 0, 0);
    device->StretchRect(surfRipples, &source, surfRippleBuffer, &target, D3DTEXF_NONE);

    // Water simulation; realigned water starts in surfRippleBuffer
    // Uses double buffering to avoid reads and writes on the same target
    RenderTargetSwitcher rtsw(surfRipples, NULL);
    SurfaceDoubleBuffer doublebuffer;
    doublebuffer.init(texRippleBuffer, surfRippleBuffer, texRipples, surfRipples);

    float rippleOrigin[2];
    float dz = playerPos->z - mwBridge->WaterLevel();
    if (dz < 0 && dz > -mwBridge->PlayerHeight()) {
        // Create waves around the player
        effect->BeginPass(PASS_PLAYERWAVE);
        for (int i = 0; i != numWaveSteps; ++i) {
            // Interpolate between starting and ending point, so that low framerates do not cause less waves
            float w = -(float)i / (float)numWaveSteps / (float)waveTexResolution;
            rippleOrigin[0] = w * shiftX + 0.5f;
            rippleOrigin[1] = w * shiftY + 0.5f;

            device->SetRenderTarget(0, doublebuffer.sinkSurface());
            effect->SetTexture(ehTex4, doublebuffer.sourceTexture());
            effect->SetFloatArray(ehRippleOrigin, rippleOrigin, 2);
            effect->CommitChanges();

            device->DrawPrimitive(D3DPT_TRIANGLESTRIP, 0, 1);
            doublebuffer.cycle();
        }
        effect->EndPass();
    }

    // Apply wave equation numWaveSteps times
    effect->BeginPass(PASS_WAVESTEP);
    for (int i = 0; i != numWaveSteps; ++i) {
        device->SetRenderTarget(0, doublebuffer.sinkSurface());
        effect->SetTexture(ehTex4, doublebuffer.sourceTexture());
        effect->CommitChanges();

        device->DrawPrimitive(D3DPT_TRIANGLESTRIP, 0, 1);
        doublebuffer.cycle();
    }
    effect->EndPass();

    if (doublebuffer.sourceSurface() != surfRipples) {
        device->StretchRect(surfRippleBuffer, 0, surfRipples, 0, D3DTEXF_NONE);
    }

    // Set wave texture world origin
    static float halfWaveTexWorldSize = 0.5f * waveTexWorldRes * waveTexResolution;
    rippleOrigin[0] = lastXpos*waveTexWorldRes - halfWaveTexWorldSize;
    rippleOrigin[1] = lastYpos*waveTexWorldRes - halfWaveTexWorldSize;
    effect->SetFloatArray(ehRippleOrigin, rippleOrigin, 2);

    // Set weather-dependent wave height
    effect->SetFloat(ehWaveHeight, (float)Configuration.DL.WaterWaveHeight);
}

bool DistantLand::waterVolumeDrawn = false;
float DistantLand::waterPlaneTint[3] = { 0.0f, 0.0f, 0.0f };
bool DistantLand::distantWaterLoaded = false;
bool DistantLand::distantWaterInView = false;
bool DistantLand::distantDrySpacesInView = false;

// The cells that the game has loaded: the cell of the player and the eight cells around it, as
// a range of x and y in the world. In an interior the range is empty.
struct LoadedCells {
    float minX = 0.0f, minY = 0.0f, maxX = 0.0f, maxY = 0.0f;

    // Says whether the reference of a distant mesh lies in one of the cells.
    bool hold(const D3DXMATRIX& transform) const {
        const float x = transform._41, y = transform._42;
        return x >= minX && y >= minY && x < maxX && y < maxY;
    }
};

static LoadedCells loadedCells() {
    LoadedCells cells;
    int gridX, gridY;
    if (MWBridge::get()->getExteriorGrid(gridX, gridY)) {
        cells.minX = (gridX - 1) * DistantLand::kCellSize;
        cells.minY = (gridY - 1) * DistantLand::kCellSize;
        cells.maxX = (gridX + 2) * DistantLand::kCellSize;
        cells.maxY = (gridY + 2) * DistantLand::kCellSize;
    }
    return cells;
}

// The visible set of the distant water holds water surfaces and dry spaces. A dry space of
// a reference in a loaded cell does not count: the game has that mesh, and a mod reports its
// dry spaces (setWaterMasks). Two counts of the same faces would leave the bit as it was,
// and the water would be back in the boat.
void DistantLand::findDistantWaterInView() {
    distantWaterInView = false;
    distantDrySpacesInView = false;
    if (!MWBridge::get()->IsExterior()) {
        return;
    }
    const LoadedCells loaded = loadedCells();
    visWaterShared.Reset();
    while (!visWaterShared.AtEnd()) {
        const RenderMesh& mesh = visWaterShared.Next();
        if (mesh.water != WaterLooks::drySpace) {
            distantWaterInView = true;
        } else if (!loaded.hold(mesh.transform)) {
            distantDrySpacesInView = true;
        }
    }
}

// How many point lights a water volume surface takes. The water shading has rows for as many.
static const int kWaterVolumeLights = 4;

// A surface mesh of a water volume, held until the scene it was submitted in ends.
struct PendingWaterVolume {
    RenderedState rs;
    bool reflectsScene;
    // The colour of the water: the emissive colour of the material. Black for the usual colour.
    float tint[3];
    // The look slot of the material; 0 for the standard look
    unsigned int lookSlot;
    // The second texture of the mesh, for a water shader; null if it has none
    IDirect3DTexture9* secondTexture;
    // The stencil test and the depth test that the mesh asks for (NiStencilProperty,
    // NiZBufferProperty). A mesh can show its water only through a mask that another of its
    // shapes wrote into the stencil buffer, at any depth: the water in a well.
    struct Tests {
        DWORD stencil, func, ref, mask, writeMask, fail, zFail, pass;
        // False when the mesh is drawn whatever the depth is: the depth test off, or on with
        // the function "always", which is how the game does it.
        bool depthTest;
    } tests;
    // The point lights that the game lit the mesh with, for the option that puts them on the
    // water: place in the world, colour, and falloff (constant, linear, quadratic). The
    // fourth number of a place is 1 for a light and 0 for an empty row.
    struct Lights {
        float place[kWaterVolumeLights][4];
        float colour[kWaterVolumeLights][4];
        float falloff[kWaterVolumeLights][4];
    } lights;
};
static std::vector<PendingWaterVolume> pendingWaterVolumes;

// Takes the point lights that are on for the draw at hand, the nearest rows first as the game
// has them.
static void takePointLights(const LightState* state, PendingWaterVolume::Lights& out) {
    out = {};
    if (state == nullptr) {
        return;
    }
    // Decoding in reverse is load-bearing: a light can set the constant of the falloff that
    // the lights before it in the list use (see FixedFunctionShader).
    float sharedConstant = 1.0f;
    int count = 0;
    for (size_t n = state->active.size(); n-- > 0 && count < kWaterVolumeLights; ) {
        const auto found = state->lights.find(state->active[n]);
        if (found == state->lights.end() || found->second.type != D3DLIGHT_POINT) {
            continue;
        }
        const LightState::Light& light = found->second;
        const DecodedPointLight decoded = decodeMorrowindPointLight(light.diffuse, light.falloff, sharedConstant);
        out.place[count][0] = light.position.x;
        out.place[count][1] = light.position.y;
        out.place[count][2] = light.position.z;
        out.place[count][3] = 1.0f;
        // The glint is the mirror image of the light, so it has the hue of the light itself.
        // The decoding gives some lights a grey colour, for the way that the game lights a
        // mesh with them; from it comes how bright the glint is.
        const float brightness = std::max(decoded.diffuse.r, std::max(decoded.diffuse.g, decoded.diffuse.b));
        const float strongest = std::max(light.diffuse.r, std::max(light.diffuse.g, light.diffuse.b));
        if (strongest > 1e-4f) {
            out.colour[count][0] = brightness * light.diffuse.r / strongest;
            out.colour[count][1] = brightness * light.diffuse.g / strongest;
            out.colour[count][2] = brightness * light.diffuse.b / strongest;
        } else {
            out.colour[count][0] = decoded.diffuse.r;
            out.colour[count][1] = decoded.diffuse.g;
            out.colour[count][2] = decoded.diffuse.b;
        }
        out.falloff[count][0] = decoded.attenuation.x;
        out.falloff[count][1] = decoded.attenuation.y;
        out.falloff[count][2] = decoded.attenuation.z;
        ++count;
    }
}

// The dry spaces: three corners for each triangle of the closed meshes, in the world.
static std::vector<D3DXVECTOR3> waterMaskCorners;
bool DistantLand::cameraInDrySpace = false;

void DistantLand::setWaterMasks(const float* corners, unsigned int triangles) {
    waterMaskCorners.clear();
    if (corners != nullptr) {
        waterMaskCorners.reserve(3 * static_cast<size_t>(triangles));
        for (unsigned int i = 0; i < 3 * triangles; ++i) {
            waterMaskCorners.emplace_back(corners[3 * i], corners[3 * i + 1], corners[3 * i + 2]);
        }
    }
}

// The bit of the stencil buffer that says "inside a dry space" while the surfaces are drawn.
// The count sets it to zero over the whole screen first, and again after the draw: the game
// leaves the count of its shadow volumes in the buffer, any value, and reads it no more. The
// other bits are not touched. Meshes of mods that draw through a mask of their own (a well, a
// shield with a window into another place) write 500 by habit, 0xf4 in the eight bits of the
// buffer, and test for it later in the frame: this bit is not one of theirs.
static const DWORD kDrySpaceBit = 0x02;

// Draws a quad over the whole screen that sets the dry space bit to zero. A pass that takes
// vertices in the world must be open. The depth test and the stencil states are left changed.
void DistantLand::zeroDrySpaceBit() {
    D3DXMATRIX toClip = mwView * mwProj, fromClip;
    D3DXMatrixInverse(&fromClip, nullptr, &toClip);
    const D3DXVECTOR3 inClip[6] = {
        { -1.1f, -1.1f, 0.5f }, { -1.1f, 1.1f, 0.5f }, { 1.1f, 1.1f, 0.5f },
        { -1.1f, -1.1f, 0.5f }, { 1.1f, 1.1f, 0.5f }, { 1.1f, -1.1f, 0.5f },
    };
    D3DXVECTOR3 inWorld[6];
    D3DXVec3TransformCoordArray(inWorld, sizeof(D3DXVECTOR3), inClip, sizeof(D3DXVECTOR3), &fromClip, 6);

    DWORD clipPlanes = 0;
    device->GetRenderState(D3DRS_CLIPPLANEENABLE, &clipPlanes);
    device->SetRenderState(D3DRS_CLIPPLANEENABLE, 0);
    device->SetRenderState(D3DRS_COLORWRITEENABLE, 0);
    device->SetRenderState(D3DRS_ZWRITEENABLE, FALSE);
    device->SetRenderState(D3DRS_ZFUNC, D3DCMP_ALWAYS);
    device->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
    device->SetRenderState(D3DRS_STENCILENABLE, TRUE);
    device->SetRenderState(D3DRS_STENCILFUNC, D3DCMP_ALWAYS);
    device->SetRenderState(D3DRS_STENCILMASK, 0xffffffff);
    device->SetRenderState(D3DRS_STENCILWRITEMASK, kDrySpaceBit);
    device->SetRenderState(D3DRS_STENCILFAIL, D3DSTENCILOP_ZERO);
    device->SetRenderState(D3DRS_STENCILZFAIL, D3DSTENCILOP_ZERO);
    device->SetRenderState(D3DRS_STENCILPASS, D3DSTENCILOP_ZERO);
    device->SetFVF(D3DFVF_XYZ);
    device->DrawPrimitiveUP(D3DPT_TRIANGLELIST, 2, inWorld, sizeof(D3DXVECTOR3));
    device->SetRenderState(D3DRS_CLIPPLANEENABLE, clipPlanes);
}

// The count draws with the pass of the water volumes, for its vertex shader. That shader
// moves vertices with the waves when dynamic ripples are on, and its pixel shader marches for
// the reflection of the scene when the look says so: both are switched off here, and the
// pass leaves out nothing by depth.
float DistantLand::beginDrySpacePass() {
    D3DXMATRIX identity;
    D3DXMatrixIdentity(&identity);
    effect->SetMatrix(ehWorld, &identity);
    effect->SetFloat(ehWaterVolumeHandoff, 0.0f);
    // The wave height is a value of the effect only when dynamic ripples are on.
    float waveHeight = 0.0f;
    if (Configuration.MGEFlags & DYNAMIC_RIPPLES) {
        effect->GetFloat(ehWaveHeight, &waveHeight);
        effect->SetFloat(ehWaveHeight, 0.0f);
    }
    static const float plain[4] = { 0.0f, 1.0f, 0.0f, 0.0f };
    effect->SetFloatArray(ehWaterVolumeMix, plain, 4);
    effect->BeginPass(PASS_RENDERWATERVOLUME);
    return waveHeight;
}

void DistantLand::endDrySpacePass(float waveHeight) {
    effect->EndPass();
    if (Configuration.MGEFlags & DYNAMIC_RIPPLES) {
        effect->SetFloat(ehWaveHeight, waveHeight);
    }
}

void DistantLand::endDrySpaceCount() {
    const float waveHeight = beginDrySpacePass();
    zeroDrySpaceBit();
    endDrySpacePass(waveHeight);
    device->SetRenderState(D3DRS_ZFUNC, D3DCMP_LESSEQUAL);
    device->SetRenderState(D3DRS_COLORWRITEENABLE, 0x0f);
    device->SetRenderState(D3DRS_STENCILENABLE, FALSE);
}

bool DistantLand::hasDrySpaces() {
    return !waterMaskCorners.empty() || distantDrySpacesInView;
}

// A water surface is not drawn inside a dry space. Whether a point of a surface is inside one
// is counted in a bit of the stencil buffer: the surface writes its depth first, then every
// face of the dry spaces that lies in front of that depth turns the bit over. An odd number of
// faces between the eye and the point means that the point is inside, when the eye is outside;
// with the eye in a dry space it is the other way round. The bit is set to zero before the
// count, and again after the draw (endDrySpaceCount).
// For the water plane of the cell no depth is needed, and the faces behind the plane are
// counted in place of those in front of it: a point of the plane is inside a closed mesh when
// the ray through it goes on through an odd number of faces. Those are the faces on the far
// side of the plane, which a clip plane picks. That count does not ask where the eye is, and
// its faces are far from the eye: a face that the eye is about to pass, the top of the dry
// space as the eye goes down into a boat, is cut by the near plane and would be missed.
// A water volume surface has its faces counted the same way, behind the depth that it wrote
// (behindDepth). Only the caustics count the faces in front: there the depth is that of the
// scene, and the floor of a boat lies in a face of its dry space.
// The dry spaces are those that a mod reports, near the player, and those of the distant
// statics (countDistantDrySpaces). Both turn over the same bit.
void DistantLand::countDrySpaces(const float* level, bool behindDepth) {
    const float waveHeight = beginDrySpacePass();
    zeroDrySpaceBit();
    DWORD clipPlanes = 0;
    device->GetRenderState(D3DRS_CLIPPLANEENABLE, &clipPlanes);
    // The states of the count. They are set after the states of a pass. The clip plane is in
    // the clip space of the projection that the pass draws with.
    const auto setCountStates = [&](const D3DXMATRIX& proj) {
        device->SetRenderState(D3DRS_ZFUNC, level ? D3DCMP_ALWAYS : (behindDepth ? D3DCMP_GREATER : D3DCMP_LESS));
        if (level) {
            // The far side of the level plane, as a clip plane in clip space
            const float side = eyePos.z >= *level ? -1.0f : 1.0f;
            const D3DXPLANE inWorld(0.0f, 0.0f, side, -side * *level);
            D3DXMATRIX toClip = mwView * proj, inverse, inverseTranspose;
            D3DXMatrixInverse(&inverse, nullptr, &toClip);
            D3DXMatrixTranspose(&inverseTranspose, &inverse);
            D3DXPLANE inClip;
            D3DXPlaneTransform(&inClip, &inWorld, &inverseTranspose);
            device->SetClipPlane(1, inClip);
            device->SetRenderState(D3DRS_CLIPPLANEENABLE, clipPlanes | D3DCLIPPLANE1);
        }
        device->SetRenderState(D3DRS_COLORWRITEENABLE, 0);
        device->SetRenderState(D3DRS_ZWRITEENABLE, FALSE);
        device->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
        device->SetRenderState(D3DRS_STENCILENABLE, TRUE);
        device->SetRenderState(D3DRS_STENCILFUNC, D3DCMP_ALWAYS);
        device->SetRenderState(D3DRS_STENCILMASK, 0xffffffff);
        device->SetRenderState(D3DRS_STENCILWRITEMASK, kDrySpaceBit);
        device->SetRenderState(D3DRS_STENCILFAIL, D3DSTENCILOP_KEEP);
        device->SetRenderState(D3DRS_STENCILZFAIL, D3DSTENCILOP_KEEP);
        device->SetRenderState(D3DRS_STENCILPASS, D3DSTENCILOP_INVERT);
    };
    setCountStates(mwProj);
    if (!waterMaskCorners.empty()) {
        device->DrawPrimitiveUP(D3DPT_TRIANGLELIST, static_cast<UINT>(waterMaskCorners.size() / 3), waterMaskCorners.data(), sizeof(D3DXVECTOR3));
    }
    if (distantDrySpacesInView) {
        effect->EndPass();
        countDistantDrySpaces(setCountStates);
    }
    endDrySpacePass(waveHeight);
    if (level) {
        device->SetRenderState(D3DRS_CLIPPLANEENABLE, clipPlanes);
    }
    device->SetRenderState(D3DRS_ZFUNC, D3DCMP_LESSEQUAL);
    device->SetRenderState(D3DRS_COLORWRITEENABLE, 0x0f);
    device->SetRenderState(D3DRS_STENCILENABLE, FALSE);
}

// The dry spaces of the distant statics: the inside of a boat in a cell that the game has
// not loaded. The generator keeps the shape of a dry space as a subset that no pass draws,
// and the host gives it in the visible set of the distant water. Its faces turn over the
// same bit as the faces that a mod reports. They are drawn as the distant water is: with the
// vertices of a distant static, the matrix of each mesh and the depth of the distant land.
// The values that beginDrySpacePass set hold for this pass too.
// The dry space of a reference in a loaded cell is left out: see findDistantWaterInView.
// No pass must be open. The pass of the distant water is left open, for endDrySpacePass.
void DistantLand::countDistantDrySpaces(const std::function<void(const D3DXMATRIX&)>& setCountStates) {
    D3DXMATRIX distProj = mwProj;
    editProjectionZ(&distProj, kDistantNearPlane - 1e-2, Configuration.DL.DrawDist * kCellSize);
    effect->SetMatrix(ehProj, &distProj);
    effect->BeginPass(PASS_RENDERWATERVOLUME_DISTANT);
    setCountStates(distProj);
    device->SetVertexDeclaration(StaticDecl);
    const LoadedCells loaded = loadedCells();
    visWaterShared.RenderWater(device, SIZEOFSTATICVERT, [&](const RenderMesh& mesh) {
        if (mesh.water != WaterLooks::drySpace || loaded.hold(mesh.transform)) {
            return false;
        }
        effect->SetMatrix(ehWorld, &mesh.transform);
        effect->CommitChanges();
        return true;
    });
    // For the passes that come after this one
    effect->SetMatrix(ehProj, &mwProj);
}

void DistantLand::testOutsideDrySpaces(bool countedBehind) {
    device->SetRenderState(D3DRS_STENCILENABLE, TRUE);
    device->SetRenderState(D3DRS_STENCILFUNC, D3DCMP_EQUAL);
    device->SetRenderState(D3DRS_STENCILREF, (cameraInDrySpace && !countedBehind) ? kDrySpaceBit : 0);
    device->SetRenderState(D3DRS_STENCILMASK, kDrySpaceBit);
    device->SetRenderState(D3DRS_STENCILWRITEMASK, 0);
    device->SetRenderState(D3DRS_STENCILFAIL, D3DSTENCILOP_KEEP);
    device->SetRenderState(D3DRS_STENCILZFAIL, D3DSTENCILOP_KEEP);
    device->SetRenderState(D3DRS_STENCILPASS, D3DSTENCILOP_KEEP);
}

// renderWaterVolume - Takes the surface mesh of a water volume, as the game submits it, to draw
// it with the water shading instead of its own material. The draw happens in flushWaterVolumes,
// so that every surface refracts and reflects the same frame, without the other surfaces in it.
// Returns false when the draw should go ahead unchanged.
bool DistantLand::renderWaterVolume(const RenderedState* rs, bool reflectsScene, const D3DCOLORVALUE& tint, unsigned int lookSlot, const LightState* lights) {
    if (!canRenderDistantLand() || isRenderCached) {
        return false;
    }

    // The game draws a mesh with a second texture (a decal, a detail or a dark map) two times,
    // the second time with that texture. The surface is water one time: the second draw gives
    // its texture to the first and is not held.
    if (!pendingWaterVolumes.empty()) {
        auto& last = pendingWaterVolumes.back();
        if (last.rs.vb == rs->vb && last.rs.ib == rs->ib && last.rs.vbOffset == rs->vbOffset && last.rs.baseIndex == rs->baseIndex
            && last.rs.startIndex == rs->startIndex && last.rs.primCount == rs->primCount && last.lookSlot == lookSlot
            && memcmp(&last.rs.worldTransforms[0], &rs->worldTransforms[0], sizeof(D3DXMATRIX)) == 0) {
            if (last.secondTexture == nullptr && rs->texture && rs->texture != last.rs.texture) {
                last.secondTexture = rs->texture;
                last.secondTexture->AddRef();
            }
            return true;
        }
    }

    rs->vb->AddRef();
    rs->ib->AddRef();
    if (rs->texture) {
        rs->texture->AddRef();
    }
    PendingWaterVolume::Tests tests = {};
    DWORD depthEnabled = D3DZB_TRUE, depthFunction = D3DCMP_LESSEQUAL;
    device->GetRenderState(D3DRS_ZENABLE, &depthEnabled);
    device->GetRenderState(D3DRS_ZFUNC, &depthFunction);
    tests.depthTest = depthEnabled != D3DZB_FALSE && depthFunction != D3DCMP_ALWAYS;
    device->GetRenderState(D3DRS_STENCILENABLE, &tests.stencil);
    if (tests.stencil) {
        device->GetRenderState(D3DRS_STENCILFUNC, &tests.func);
        device->GetRenderState(D3DRS_STENCILREF, &tests.ref);
        device->GetRenderState(D3DRS_STENCILMASK, &tests.mask);
        device->GetRenderState(D3DRS_STENCILWRITEMASK, &tests.writeMask);
        device->GetRenderState(D3DRS_STENCILFAIL, &tests.fail);
        device->GetRenderState(D3DRS_STENCILZFAIL, &tests.zFail);
        device->GetRenderState(D3DRS_STENCILPASS, &tests.pass);
    }
    if (!tests.depthTest) {
        // A surface that is drawn whatever the depth is has its place in the order of the
        // draws of its mesh: what the mesh draws after it covers it, as the wall of a well
        // covers the rim of its water. So it is drawn now and not held. It shows nothing of
        // what is behind it, and needs no copy of the frame.
        std::vector<PendingWaterVolume> held;
        held.swap(pendingWaterVolumes);
        const bool distantInView = distantWaterInView;
        pendingWaterVolumes.push_back({ *rs, reflectsScene, { tint.r, tint.g, tint.b }, lookSlot, nullptr, tests });
        if (Configuration.WaterVolume.PointLights) {
            takePointLights(lights, pendingWaterVolumes.back().lights);
        }
        flushWaterVolumes(false);
        pendingWaterVolumes.swap(held);
        distantWaterInView = distantInView;
        waterVolumeDrawn = true;
        return true;
    }

    pendingWaterVolumes.push_back({ *rs, reflectsScene, { tint.r, tint.g, tint.b }, lookSlot, nullptr, tests });
    if (Configuration.WaterVolume.PointLights) {
        takePointLights(lights, pendingWaterVolumes.back().lights);
    }
    waterVolumeDrawn = true;
    return true;
}

// discardWaterVolumes - Drops the surfaces that were taken and not drawn.
void DistantLand::discardWaterVolumes() {
    for (auto& pending : pendingWaterVolumes) {
        pending.rs.vb->Release();
        pending.rs.ib->Release();
        if (pending.rs.texture) {
            pending.rs.texture->Release();
        }
        if (pending.secondTexture) {
            pending.secondTexture->Release();
        }
    }
    pendingWaterVolumes.clear();
}

// flushWaterVolumes - Draws the surfaces taken by renderWaterVolume, all from one copy of the frame.
// withDistant: draw the water among the distant statics as well, from the same copy. It is the
// same water farther away than the game draws, so it is wanted once per frame, with the first flush.
void DistantLand::flushWaterVolumes(bool withDistant) {
    const bool distantWanted = withDistant && distantWaterInView;
    distantWaterInView = false;
    if (pendingWaterVolumes.empty() && !distantWanted) {
        return;
    }

    auto mwBridge = MWBridge::get();
    // From below, the planar reflection is the one of the volume the eye is in.
    const bool underwater = mwBridge->IsUnderwater(eyePos.z);
    const bool drawDistant = distantWanted && !underwater && canRenderDistantLand() && !isRenderCached;
    if (pendingWaterVolumes.empty() && !drawDistant) {
        return;
    }

    IDirect3DStateBlock9* stateSaved;
    UINT passes;

    // Save state block manually since we can change FVF/decl
    device->CreateStateBlock(D3DSBT_ALL, &stateSaved);
    effect->Begin(&passes, D3DXFX_DONOTSAVESTATE);

    IDirect3DTexture9* texRefract = PostShaders::borrowBuffer(0);
    effect->SetTexture(ehTex0, texReflection);
    effect->SetTexture(ehTex1, texWater);
    effect->SetTexture(ehTex2, texRefract);
    effect->SetTexture(ehTex3, texDepthFrame);
    // The mesh is not tessellated for wave displacement, which would only rock it.
    const bool ripples = (Configuration.MGEFlags & DYNAMIC_RIPPLES) != 0;
    float waveHeight = 0.0f;
    if (ripples) {
        effect->SetTexture(ehTex4, texRain);
        effect->SetTexture(ehTex5, texRipples);
        effect->GetFloat(ehWaveHeight, &waveHeight);
        effect->SetFloat(ehWaveHeight, 0.0f);
    }

    // A surface near the player is drawn at every depth.
    effect->SetFloat(ehWaterVolumeHandoff, 0.0f);

    // What a surface reflects in place of the sky in an interior: the light of the room, as
    // clearReflection takes it for the water of the cell. The last value is 1 in an interior.
    // A look with a sky colour of its own has that in its place.
    float indoors[4] = { 0.0f, 0.0f, 0.0f, 0.0f };
    if (!mwBridge->IntLikeExterior(true)) {
        const BYTE* sun = mwBridge->getInteriorSun();
        indoors[0] = std::min(1.0f, sun[0] / 255.0f + ambCol.r);
        indoors[1] = std::min(1.0f, sun[1] / 255.0f + ambCol.g);
        indoors[2] = std::min(1.0f, sun[2] / 255.0f + ambCol.b);
        indoors[3] = 1.0f;
    }
    const auto setSky = [&](const WaterLook& look) {
        if ((look.flags & WATER_LOOK_HAS_SKY) != 0) {
            const float own[4] = { look.sky[0], look.sky[1], look.sky[2], 1.0f };
            effect->SetFloatArray(ehWaterVolumeSky, own, 4);
        } else {
            effect->SetFloatArray(ehWaterVolumeSky, indoors, 4);
        }
        effect->SetFloat(ehWaterVolumeClarity, std::max(0.0f, look.clarity));
    };

    // The options for light on the water. The shadow map is that of the distant land, and is
    // there only in a cell with weather and with shadows on: elsewhere the matrices put every
    // place outside it, which the shading takes as full sun. The surfaces do not use the
    // reflection of the water plane, so the shadow map has its texture place. Caustics are
    // from the sun, so they have a strength only where the sky is.
    const bool outdoors = mwBridge->IntLikeExterior(true);
    if (Configuration.WaterVolume.Shadows && !underwater) {
        if ((Configuration.MGEFlags & USE_SHADOWS) && outdoors) {
            effect->SetMatrixArray(ehShadowViewproj, smViewproj, kShadowCascades);
            effect->SetTexture(ehTex0, texSoftShadow);
        } else {
            static const D3DXMATRIX outsideAtlas(
                0, 0, 0, 0,
                0, 0, 0, 0,
                0, 0, 0, 0,
                2, 0, 0, 1);
            static const D3DXMATRIX noShadow[2] = { outsideAtlas, outsideAtlas };
            effect->SetMatrixArray(ehShadowViewproj, noShadow, 2);
        }
    }
    if (Configuration.WaterVolume.Caustics) {
        effect->SetFloat(ehWaterVolumeCaustics, outdoors ? float(Configuration.DL.WaterCaustics) : 0.0f);
    }
    // A surface is not drawn inside a dry space: see countDrySpaces.
    const bool cutDrySpaces = hasDrySpaces() && !underwater && (!pendingWaterVolumes.empty() || drawDistant);

    // Each surface is drawn with the pass of its look: the water shader of a mod, or the
    // standard one. From below all surfaces have the underwater pass.
    const auto drawHeld = [&](bool depthOnly) {
        int passInUse = -1;
        for (const auto& pending : pendingWaterVolumes) {
            const RenderedState* rs = &pending.rs;
            // A surface with a stencil test of its own is not cut.
            if (depthOnly && pending.tests.stencil) {
                continue;
            }
            const WaterLook& look = WaterLooks::get(pending.lookSlot);
            int pass = PASS_RENDERUNDERWATER;
            if (!underwater) {
                pass = waterShaderPass(look.shader);
                if (pass < 0) {
                    pass = PASS_RENDERWATERVOLUME;
                }
            }
            effect->SetMatrix(ehWorld, &rs->worldTransforms[0]);
            effect->SetFloatArray(ehWaterVolumeTint, pending.tint, 3);
            // The look of the surface. The vertex colour counts only when the look asks for it
            // and the mesh has one: 1 for the tint, 2 for the opacity.
            const float flow[4] = { look.flow[0], look.flow[1], look.speed, std::max(look.scale, 1e-3f) };
            float vertexUse = 0.0f;
            if ((rs->fvf & D3DFVF_DIFFUSE) != 0) {
                vertexUse += (look.flags & WATER_LOOK_TINT_FROM_VERTEX) != 0 ? 1.0f : 0.0f;
                vertexUse += (look.flags & WATER_LOOK_OPACITY_FROM_VERTEX) != 0 ? 2.0f : 0.0f;
            }
            // The last value: 1 the surface reflects what is on screen, 2 it is from the distant
            // land, 4 the mesh has second texture coordinates, 8 the surface is drawn without
            // the depth test, so the depth of the scene says nothing about what is under it.
            const bool secondCoordinates = (rs->fvf & D3DFVF_TEXCOUNT_MASK) >= D3DFVF_TEX2;
            const bool noDepthTest = !pending.tests.depthTest;
            const float mix[4] = { look.glow, look.opacity, vertexUse,
                                   (pending.reflectsScene ? 1.0f : 0.0f) + (secondCoordinates ? 4.0f : 0.0f) + (noDepthTest ? 8.0f : 0.0f) };
            effect->SetFloatArray(ehWaterVolumeFlow, flow, 4);
            effect->SetFloatArray(ehWaterVolumeMix, mix, 4);
            setSky(look);
            if (Configuration.WaterVolume.PointLights) {
                effect->SetFloatArray(ehWaterVolumeLightPos, &pending.lights.place[0][0], 4 * kWaterVolumeLights);
                effect->SetFloatArray(ehWaterVolumeLightCol, &pending.lights.colour[0][0], 4 * kWaterVolumeLights);
                effect->SetFloatArray(ehWaterVolumeLightFalloff, &pending.lights.falloff[0][0], 4 * kWaterVolumeLights);
            }
            if (pass >= PASS_WATERSHADER_FIRST) {
                effect->SetFloatArray(ehWaterVolumeParams, &look.params[0][0], 16);
                effect->SetTexture(ehMeshTex0, rs->texture);
                effect->SetTexture(ehMeshTex1, pending.secondTexture);
            }
            if (pass != passInUse) {
                if (passInUse >= 0) {
                    effect->EndPass();
                }
                effect->BeginPass(pass);
                passInUse = pass;
            } else {
                effect->CommitChanges();
            }
            // The tests of the mesh, after the states of the pass
            const auto& tests = pending.tests;
            // A surface that is drawn whatever the depth is does not write its own depth: it
            // can lie under the ground.
            device->SetRenderState(D3DRS_ZFUNC, tests.depthTest ? D3DCMP_LESSEQUAL : D3DCMP_ALWAYS);
            device->SetRenderState(D3DRS_ZWRITEENABLE, tests.depthTest ? TRUE : FALSE);
            device->SetRenderState(D3DRS_STENCILENABLE, tests.stencil);
            if (tests.stencil) {
                device->SetRenderState(D3DRS_STENCILFUNC, tests.func);
                device->SetRenderState(D3DRS_STENCILREF, tests.ref);
                // The bit of the dry space count is not a part of the test of the mesh.
                device->SetRenderState(D3DRS_STENCILMASK, tests.mask & ~kDrySpaceBit);
                device->SetRenderState(D3DRS_STENCILWRITEMASK, tests.writeMask);
                device->SetRenderState(D3DRS_STENCILFAIL, tests.fail);
                device->SetRenderState(D3DRS_STENCILZFAIL, tests.zFail);
                device->SetRenderState(D3DRS_STENCILPASS, tests.pass);
            } else if (cutDrySpaces && !depthOnly) {
                testOutsideDrySpaces(true);
            }
            device->SetRenderState(D3DRS_COLORWRITEENABLE, depthOnly ? 0 : 0x0f);
            device->SetStreamSource(0, rs->vb, rs->vbOffset, rs->vbStride);
            device->SetIndices(rs->ib);
            device->SetFVF(rs->fvf);
            device->DrawIndexedPrimitive(rs->primType, rs->baseIndex, rs->minIndex, rs->vertCount, rs->startIndex, rs->primCount);
        }
        if (passInUse >= 0) {
            effect->EndPass();
        }
        device->SetRenderState(D3DRS_ZFUNC, D3DCMP_LESSEQUAL);
        device->SetRenderState(D3DRS_ZWRITEENABLE, TRUE);
        device->SetRenderState(D3DRS_STENCILENABLE, FALSE);
        device->SetRenderState(D3DRS_COLORWRITEENABLE, 0x0f);
        effect->SetTexture(ehMeshTex0, NULL);
        effect->SetTexture(ehMeshTex1, NULL);
    };

    // The distant water comes after the surfaces near the player. The game draws a mesh that
    // reaches past its view distance in full, and the distant mesh lies a little behind it in
    // depth, so the depth test leaves the distant mesh out there and it is not shaded twice.
    const auto drawDistantWater = [&](bool depthOnly) {
        // Depth as the distant land has it, so that the land hides the water behind it.
        D3DXMATRIX distProj = mwProj;
        editProjectionZ(&distProj, kDistantNearPlane - 1e-2, Configuration.DL.DrawDist * kCellSize);
        effect->SetMatrix(ehProj, &distProj);
        device->SetVertexDeclaration(StaticDecl);
        // The game has the cell of the player and the eight cells around it.
        const LoadedCells loaded = loadedCells();
        // Each mesh is drawn with the pass and the values of its look. The pass leaves a mesh
        // out nearer than the handoff depth, where the game draws the mesh itself. The game
        // does that only for a reference in a loaded cell, so any other mesh gets zero.
        if (Configuration.WaterVolume.PointLights) {
            // Distant water has no lights of the game: the rows of the last near surface go.
            static const PendingWaterVolume::Lights none = {};
            effect->SetFloatArray(ehWaterVolumeLightPos, &none.place[0][0], 4 * kWaterVolumeLights);
        }
        int distantPassInUse = -1;
        visWaterShared.RenderWater(device, SIZEOFSTATICVERT, [&](const RenderMesh& mesh) {
            // A dry space is in the set too. It is no water: only the count draws it.
            if (mesh.water == WaterLooks::drySpace) {
                return false;
            }
            const WaterLook& look = WaterLooks::distant(mesh.water);
            const bool reflectsScene = mesh.water >= WaterLooks::firstDistantLook
                ? (look.flags & WATER_LOOK_REFLECTS_SCENE) != 0
                : mesh.water == 1;
            int pass = waterShaderPass(look.shader);
            pass = pass < 0 ? PASS_RENDERWATERVOLUME_DISTANT : pass + 1;

            const float flow[4] = { look.flow[0], look.flow[1], look.speed, std::max(look.scale, 1e-3f) };
            // The vertex colour of a distant subset is the colour of the water. Its alpha is
            // the opacity of the mesh's vertex when the look asks for it.
            const float vertexUse = (look.flags & WATER_LOOK_OPACITY_FROM_VERTEX) != 0 ? 2.0f : 0.0f;
            const float mix[4] = { look.glow, look.opacity, vertexUse, (reflectsScene ? 1.0f : 0.0f) + 2.0f };
            effect->SetFloat(ehWaterVolumeHandoff, loaded.hold(mesh.transform) ? nearViewRange : 0.0f);
            effect->SetMatrix(ehWorld, &mesh.transform);
            effect->SetFloatArray(ehWaterVolumeFlow, flow, 4);
            effect->SetFloatArray(ehWaterVolumeMix, mix, 4);
            setSky(look);
            if (pass >= PASS_WATERSHADER_FIRST) {
                effect->SetFloatArray(ehWaterVolumeParams, &look.params[0][0], 16);
                effect->SetTexture(ehMeshTex0, mesh.tex);
            }
            if (pass != distantPassInUse) {
                if (distantPassInUse >= 0) {
                    effect->EndPass();
                }
                effect->BeginPass(pass);
                distantPassInUse = pass;
            } else {
                effect->CommitChanges();
            }
            // The cut by the dry spaces, after the states of the pass
            if (cutDrySpaces && !depthOnly) {
                testOutsideDrySpaces(true);
            }
            device->SetRenderState(D3DRS_COLORWRITEENABLE, depthOnly ? 0 : 0x0f);
            return true;
        });
        if (distantPassInUse >= 0) {
            effect->EndPass();
        }
        device->SetRenderState(D3DRS_STENCILENABLE, FALSE);
        device->SetRenderState(D3DRS_COLORWRITEENABLE, 0x0f);
        effect->SetMatrix(ehProj, &mwProj);
    };

    if (cutDrySpaces) {
        // All the surfaces write their depth, the faces of the dry spaces behind it are
        // counted, and then the surfaces are drawn where the count says outside.
        drawHeld(true);
        if (drawDistant) {
            drawDistantWater(true);
        }
        countDrySpaces(nullptr, true);
        drawHeld(false);
        if (drawDistant) {
            drawDistantWater(false);
        }
        endDrySpaceCount();
        device->SetRenderState(D3DRS_ZFUNC, D3DCMP_LESSEQUAL);
        device->SetRenderState(D3DRS_ZWRITEENABLE, TRUE);
    } else {
        drawHeld(false);
        if (drawDistant) {
            drawDistantWater(false);
        }
    }

    if (ripples) {
        effect->SetFloat(ehWaveHeight, waveHeight);
    }
    effect->End();
    stateSaved->Apply();
    stateSaved->Release();

    discardWaterVolumes();
}

void DistantLand::renderWaterPlane() {
    D3DXMATRIX m;
    IDirect3DTexture9* texRefract = PostShaders::borrowBuffer(0);

    D3DXMatrixTranslation(&m, eyePos.x, eyePos.y, MWBridge::get()->WaterLevel());
    effect->SetMatrix(ehWorld, &m);
    effect->SetTexture(ehTex0, texReflection);
    effect->SetTexture(ehTex1, texWater);
    effect->SetTexture(ehTex2, texRefract);
    effect->SetTexture(ehTex3, texDepthFrame);
    if (Configuration.MGEFlags & DYNAMIC_RIPPLES) {
        effect->SetTexture(ehTex4, texRain);
        effect->SetTexture(ehTex5, texRipples);
    }
    effect->CommitChanges();

    device->SetVertexDeclaration(WaterDecl);
    device->SetStreamSource(0, vbWater, 0, 12);
    device->SetIndices(ibWater);
    device->DrawIndexedPrimitive(D3DPT_TRIANGLELIST, 0, 0, numWaterVerts, 0, numWaterTris);
}
