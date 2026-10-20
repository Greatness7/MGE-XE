
// XE Mod Water Volume.fx
// Surfaces of water volumes: meshes the game submits, shaded like the water plane.
// Uses the vertex shader, samplers and helpers of XE Mod Water.fx.

// A volume can sit at any height and its surface can face any way, so the planar reflection
// of the main water does not apply to it. The sky is reflected from its analytic colour, and
// what is on screen is reflected by marching the reflected ray through the depth frame.
// The shading follows the normal of the mesh: the ripples are tilted to it, and the reflection
// and the Fresnel term are taken from it.

// A mod can replace the pixel shader of a surface with one of its own: see WaterShade below
// and docs/water-shaders.md.

// The base texture of the mesh, for the water shader of a mod
sampler sampMesh0 = sampler_state { texture = <texMesh0>; minfilter = linear; magfilter = linear; mipfilter = linear; addressu = wrap; addressv = wrap; };
// The second texture of the mesh (a decal, a detail or a dark map), for the same. A surface
// from the distant land has none.
sampler sampMesh1 = sampler_state { texture = <texMesh1>; minfilter = linear; magfilter = linear; mipfilter = linear; addressu = wrap; addressv = wrap; };

// The look of a surface beyond its colour, from waterVolumeFlow, waterVolumeMix and the vertex.
struct WaterSurfaceLook
{
    // Drift of the ripples in world units per second, speed and size of the ripples
    float2 drift;
    float speed;
    float scale;
    float glow;
    // 1 is water; lower shows what is behind the surface
    float opacity;
    // Over how many units of water what is under the surface fades into the colour of deep
    // water; 800 is the standard, and 0 shows nothing of what is under it
    float clarity;
    // The vertex colour, or 1
    float3 tint;
    // The surface reflects what is on screen; otherwise the sky only
    bool reflectsScene;
    // The surface is drawn from the distant land, farther away than the game draws
    bool distant;
    // The surface is drawn without the depth test (a mesh that shows its water through a
    // stencil mask, as in a well). The depth of the scene is then not that of what lies
    // under the water, and the water is taken as deep.
    bool noDepthTest;
    // Free values p0 to p3 of the look, for the water shader of a mod
    float4 p0, p1, p2, p3;
};

// The look of the surface that is drawn. The drift is given in the axes of the mesh and
// turned into the world.
WaterSurfaceLook surfaceLook(float4 vertexColour)
{
    WaterSurfaceLook look;
    look.drift = mul(float4(waterVolumeFlow.xy, 0, 0), world).xy;
    look.speed = waterVolumeFlow.z;
    look.scale = waterVolumeFlow.w;
    look.glow = waterVolumeMix.x;
    look.clarity = waterVolumeClarity;
    float tintFromVertex = fmod(waterVolumeMix.z, 2);
    float opacityFromVertex = floor(waterVolumeMix.z / 2);
    look.opacity = waterVolumeMix.y * lerp(1, vertexColour.a, opacityFromVertex);
    look.tint = lerp(1, vertexColour.rgb, tintFromVertex);
    look.noDepthTest = fmod(waterVolumeMix.w, 16) > 7.5;
    look.reflectsScene = fmod(waterVolumeMix.w, 2) > 0.5 && !look.noDepthTest;
    look.distant = fmod(waterVolumeMix.w, 4) > 1.5;
    look.p0 = waterVolumeParams[0];
    look.p1 = waterVolumeParams[1];
    look.p2 = waterVolumeParams[2];
    look.p3 = waterVolumeParams[3];
    return look;
}

// The ripples of the water plane, with a drift, a speed and a size of their own. The drift
// moves the ripple coordinates; it is wrapped so that it keeps its precision over hours.
float3 surfaceRipples(float2 texcoord1, float2 texcoord2, float dist, float2 vertXY, WaterSurfaceLook look)
{
    float t = 0.4 * time * look.speed;
    float2 moved = look.drift * time;
    float3 w1 = float3(texcoord1 / look.scale + frac(moved / 3900), t);
    float3 w2 = float3(texcoord2 / look.scale + frac(moved / 527), t);

    float2 far_normal = tex3D(sampWater3d, w1).rg;
    float2 close_normal = tex3D(sampWater3d, w2).rg;

#ifdef DYNAMIC_RIPPLES
    close_normal.rg += tex2Dlod(sampRain, float4(texcoord2, 0, 0)).ba;
    close_normal.rg += tex2Dlod(sampWave, float4((vertXY - rippleOrigin) / waveTexWorldSize, 0, 0)).ba;
#endif

    float2 normal_R = 2 * lerp(close_normal, far_normal, saturate(dist / 8000)) - 1;
    return normalize(float3(normal_R, 1));
}

// Turns a direction by the turn that takes straight up to the given direction.
float3 tiltTo(float3 up, float3 v)
{
    float3 axis = float3(-up.y, up.x, 0);
    return v * up.z + cross(axis, v) + axis * (dot(axis, v) / max(1 + up.z, 1e-3));
}

// The reflection march: each step is 1.25 times as long as the one before, starting at 12
// units.

// Halvings of the step the ray went behind something in, to find the place where it did.
static const int volumeReflectionRefinements = 5;

// All looks at the depth of the scene that one ray may take: its steps, and the halvings of
// up to three places where it went behind something.
static const int volumeReflectionSamples = 39;

// How far along a ray it leaves the screen. The ray is given in clip space.
float reflectionReach(float4 origin, float4 dir)
{
    // The ray is on screen while each of these is positive
    float4 room = float4(origin.w - origin.x, origin.w + origin.x, origin.w - origin.y, origin.w + origin.y);
    float4 closing = float4(dir.x - dir.w, -dir.x - dir.w, dir.y - dir.w, -dir.y - dir.w);
    float4 reach = closing > 0 ? room / closing : 1e9;
    return min(min(reach.x, reach.y), min(reach.z, reach.w));
}

// The ray at one distance along it: where it is on screen, and in z how far it is behind what
// the depth frame has there. The depth frame holds view depth; the sky is cleared to a huge
// value, so the ray is never behind it. The depth is read as it is, one pixel, and not
// smoothed: between a thing and what is behind it, a smoothed depth is that of a surface
// that is not there, and the ray would meet it at one pixel and not at the next.
float3 reflectionSample(float4 origin, float4 dir, float travelled)
{
    float4 clip = origin + dir * travelled;
    float2 uv = 0.5 * (1 + rcpRes) + float2(0.5, -0.5) * clip.xy / clip.w;
    return float3(uv, clip.w - tex2Dlod(sampDepthPoint, float4(uv, 0, 0)).r);
}

// Looks for the first thing on screen that the ray from origin along dir meets.
// Returns its colour from the frame behind the water, and in alpha how much to trust it.
//
// At each place along the ray there are three cases. The ray is in front of what is on screen
// there: it is in the open. It is a little behind it: it met that surface in the last step.
// It is far behind it: a thing nearer to the eye hides the ray, which says nothing about the
// ray, so the march goes on to where the ray is in the open again. A ray that stopped at the
// first thing it went behind would leave the water beside a post, an arch or the end of a
// boat without the reflection of what is behind them, and the two answers would change from
// pixel to pixel along the edge of the thing.
float4 reflectScene(float3 origin, float3 dir)
{
    float4 clipOrigin = mul(mul(float4(origin, 1), view), proj);
    float4 clipDir = mul(mul(float4(dir, 0), view), proj);

    // The march stops where the ray leaves the screen
    float reach = 0.999 * reflectionReach(clipOrigin, clipDir);

    float stepLength = 12;
    // How far the march is
    float marched = 0;
    // The last place looked at was behind a thing that hides the ray
    bool hidden = false;
    // While a place where the ray went behind something is looked for: the two ends between
    // which it is, the ray at the far end, how many halvings are left, and how far behind a
    // surface the ray may be to have met it.
    float open = 0;
    float behind = 0;
    float3 atBehind = 0;
    int halvings = 0;
    float limit = 0;

    float4 result = 0;
    for(int i = 0; i < volumeReflectionSamples; i++)
    {
        if(halvings > 0)
        {
            // Find the place, so that the reflection does not jump from one step to the next
            float middle = 0.5 * (open + behind);
            float3 atMiddle = reflectionSample(clipOrigin, clipDir, middle);
            if(atMiddle.z > 0)
            {
                behind = middle;
                atBehind = atMiddle;
            }
            else
            {
                open = middle;
            }
            halvings--;
            if(halvings == 0)
            {
                // The halvings leave a thirty-second of the step, so a surface that the ray
                // met is no farther in front of it than that.
                if(atBehind.z < limit)
                {
                    // Fade out towards the screen edge, where the ray would leave the frame
                    float2 edge = min(atBehind.xy, 1 - atBehind.xy);
                    result.rgb = tex2Dlod(sampRefract, float4(atBehind.xy, 0, 0)).rgb;
                    result.a = saturate(12 * min(edge.x, edge.y));
                    break;
                }
                // The place is the edge of a thing nearer to the eye: the ray went behind it
                hidden = true;
            }
        }
        else
        {
            if(marched >= reach)
                break;

            float next = min(marched + stepLength, reach);
            float3 atNext = reflectionSample(clipOrigin, clipDir, next);
            if(atNext.z > 0)
            {
                // From the open to a little behind a surface: the ray met it in this step
                if(!hidden && atNext.z < 2 * stepLength + 16)
                {
                    open = marched;
                    behind = next;
                    atBehind = atNext;
                    halvings = volumeReflectionRefinements;
                    limit = stepLength / 16 + 16;
                }
                else
                {
                    hidden = true;
                }
            }
            else
            {
                hidden = false;
            }
            marched = next;
            stepLength *= 1.25;
        }
    }

    return result;
}

// The shading of a surface point before the look finishes it. The water shader of a mod
// gets it from shadeWaterVolume, changes the colour, and gives it to finishWaterVolume.
struct WaterShade
{
    // The colour of the water at this point
    float3 colour;
    // From the eye to the point, and how far that is
    float3 eyeVec;
    float dist;
    // Fog at the point: rgb what the fog adds, a how much of the surface is left
    float4 fog;
    // The ripple normal as on a level surface, and the way the surface faces
    float3 ripple;
    float3 face;
    // How far the view ray goes through the water to what is behind the surface, and how
    // deep that is under the surface. Both are 4000 or more where the water is deep.
    float rayDepth;
    float waterDepth;
    // How much of the sun reaches the point, 0 to 1: less in the shadow of a thing. It is 1
    // unless the option for shadows on water volumes is on.
    float sun;
};

//------------------------------------------------------------
// Light on the water. Each part is an option of MGE XE, and is compiled only when it is on.

#ifdef WATER_VOLUME_SHADOWS
// The shadow map of the distant land. The surfaces of water volumes do not use the reflection
// of the water plane, so it has that texture place.
sampler sampVolumeShadow = sampler_state { texture = <tex0>; minfilter = linear; magfilter = linear; mipfilter = none; addressu = clamp; addressv = clamp; };

// How much of the sun reaches a place in the world. A place outside the shadow map has it all.
float volumeSunlight(float3 place)
{
    float4 inner = mul(float4(place, 1), shadowViewProj[0]);
    float4 outer = mul(float4(place, 1), shadowViewProj[1]);
    inner.z /= inner.w;
    outer.z /= outer.w;

    float dz = 1e-6;
    [branch] if(all(saturate(atlasMargin - abs(inner.xyz))))
    {
        float2 uv = (0.5 + 0.5 * shadowRcpRes) + float2(0.5, -0.5) * inner.xy;
        dz = tex2Dlod(sampVolumeShadow, mapShadowToAtlas(uv, 0)).r / ESM_scale - inner.z;
    }
    else if(all(saturate(atlasMargin - abs(outer.xyz))))
    {
        float2 uv = (0.5 + 0.5 * shadowRcpRes) + float2(0.5, -0.5) * outer.xy;
        dz = tex2Dlod(sampVolumeShadow, mapShadowToAtlas(uv, 1)).r / ESM_scale - outer.z;
    }
    return 1 - shadowESM(dz);
}
#endif

#ifdef WATER_VOLUME_LIGHTS
// The point lights of the game on the surface: in glint, the mirror image of each light in
// the ripples; in glow, the light that the body of the water takes from them.
void volumePointLights(float3 place, float3 eyeVec, float3 normal, float3 face, out float3 glint, out float3 glow)
{
    glint = 0;
    glow = 0;
    for(int i = 0; i < 4; i++)
    {
        float3 toLight = waterVolumeLightPos[i].xyz - place;
        float distance = length(toLight);
        toLight /= max(distance, 1e-3);
        float3 falloff = waterVolumeLightFalloff[i].xyz;
        float reach = waterVolumeLightPos[i].w / max(falloff.x + falloff.y * distance + falloff.z * distance * distance, 1e-4);
        float3 light = waterVolumeLightCol[i].rgb * reach;

        float mirrored = saturate(dot(-eyeVec, reflect(-toLight, normal)));
        glint += light * (pow(mirrored, 90) + 0.06 * pow(mirrored, 6));
        glow += light * saturate(dot(toLight, face));
    }
}
#endif

#ifdef WATER_VOLUME_CAUSTICS
// The light pattern of the ripples on what is under the surface, as the water of the cell has
// it: a number to multiply the colour of the bed with. bed is the place under the water in
// the world, level the height of the surface over it, depth how far the bed is from the eye.
float volumeCaustics(float3 bed, float level, float depth)
{
    float under = bed.z - level;
    float2 sunray = bed.xy - sunVec.xy * (under / min(sunVec.z, -0.05));
    float sunlight = 1 - pow(1 - sunVis, 2);
    float strength = 0.05 * waterVolumeCaustics * saturate(0.75 * sunlight + 0.35 * length(fogColFar));
    float caust = strength * tex3D(sampWater3d, float3(sunray / 1104, 0.4 * time)).b;
    caust *= saturate(125 / depth * min(fwidth(sunray.x), fwidth(sunray.y)));
    return 1 + (caust - 0.3 * saturate(strength)) * saturate(exp(under / 400)) * saturate(under / -30);
}
#endif

// A surface either reflects what is on screen or the sky only; that is a value of the look.
// A distant surface reflects what is on screen out to waterVolumeReflectRange.
// A range of zero turns the reflection of what is on screen off, near and far.
// tint is the colour of the water of this surface: the emissive colour of its material.
// facing is the normal of the mesh. A mesh without normals faces up.
// look is the rest of what the mesh and its mod say about the surface.
// ripple is the normal of the ripples as on a level surface, z up, of length 1. A water
// shader of a mod that has ripples of its own, from a normal map or along a flow map, gives
// them here; shadeWaterVolume gives the standard ones.
WaterShade shadeWaterVolumeRippled(in WaterVertOut IN, float3 facing, float3 tint, WaterSurfaceLook look, float3 ripple)
{
    // A distant surface is left out nearer than the handoff depth, where the game draws the
    // surface itself. The depth is zero for a surface near the player, and for a distant mesh
    // whose reference the game has not loaded.
    clip(IN.screenpos.w - waterVolumeHandoff);

    bool reflectsScene = look.reflectsScene;
    bool distant = look.distant;

    // Calculate eye vector
    float3 EyeVec = IN.pos.xyz - eyePos.xyz;
    float dist = length(EyeVec);
    EyeVec /= dist;

    // Define fog
    float4 fog = fogColourWater(EyeVec, dist);
    float3 depthColor = fogApply(waterDepthBase(depthBaseColor, tint) * look.tint, fog);

    // The way the surface faces, turned to the side of the eye
    float3 face = dot(facing, facing) > 0.25 ? normalize(facing) : float3(0, 0, 1);
    face = dot(face, EyeVec) > 0 ? -face : face;

    // In the shadow of a thing the water has no glint of the sun, and its depth is darker by
    // the share that the sun has in it.
    float sun = 1;
#ifdef WATER_VOLUME_SHADOWS
    sun = volumeSunlight(IN.pos.xyz);
    depthColor *= lerp(0.72, 1, sun);
#endif

    // Calculate water normal: the ripples of a level surface, tilted to the surface
    float3 normal = tiltTo(face, ripple);

    // Refraction pixel distortion factor, wind strength increases distortion
    float2 reffactor = (windFactor * dist + 0.1) * ripple.xy;

    // Distort refraction dependent on depth
    float4 newscrpos = IN.screenpos + float4(reffactor.yx, 0, 0);
    float depth = max(0, tex2Dproj(sampDepth, newscrpos).r - IN.screenpos.w);
    if (look.noDepthTest) {
        depth = 4000;
    }

#ifdef WATER_VOLUME_CAUSTICS
    // Caustics on what is under a surface that faces up, where the sun reaches it. They are
    // worked out here and not where the bed is read, because they need the change of a value
    // from pixel to pixel, which a branch cannot have.
    float viewForward = dot(EyeVec, float3(view[0][2], view[1][2], view[2][2]));
    float causticRay = depth / viewForward;
    float caustics = volumeCaustics(IN.pos.xyz + EyeVec * causticRay, IN.pos.z, dist + causticRay);
    caustics = lerp(1, caustics, sun * saturate(4 * face.z - 3));
#endif

    // Refraction
    float3 refracted = depthColor;
    float shorefactor = 0;
    float rayDepth = depth;

    // Avoid sampling deep water
    if(depth < 4000)
    {
        // Sample refraction texture
        newscrpos = IN.screenpos + saturate(depth / 100) * float4(reffactor.yx, 0, 0);
        refracted = tex2Dproj(sampRefract, newscrpos).rgb;

        // Get distorted depth
        depth = max(0, tex2Dproj(sampDepth, newscrpos).r - IN.screenpos.w);
        depth /= dot(EyeVec, float3(view[0][2], view[1][2], view[2][2]));
        // The clarity of the look sets how fast the water takes what is under it: the colour
        // of the water takes the other colours out as much faster as the fade is shorter.
        float clarity = max(look.clarity, 1);
#ifdef WATER_VOLUME_CAUSTICS
        refracted *= caustics;
#endif
        refracted *= waterTransmission(tint, depth * 800 / clarity) * look.tint;
        rayDepth = depth;

        // Small scale shoreline animation
        depth += 300 * (0.95 - ripple.z);

        float depthscale = saturate(exp(-depth / clarity));
        shorefactor = pow(depthscale, 90);

        // Make transition between actual refraction image and depth color depending on water depth
        refracted = lerp(depthColor, refracted, 0.8 * depthscale + 0.2 * shorefactor);
    }

    // Smooth out high frequencies at a distance
    float calm = (1 + dot(EyeVec, face)) * (1 - saturate(1 / (dist / 1000 + 1)));
    float3 adjustnormal = lerp(0.1 * face, normal, pow(saturate(1.05 * fog.a), 2));
    adjustnormal = lerp(adjustnormal, face, calm);

    // Reflect the sky. The ripples tilt the reflected direction only a little, to keep it calm,
    // and less far away and at a flat angle, where a ripple is smaller than a pixel and the
    // reflection would break up into dots.
    // A reflected direction that points into the surface is turned back out of it.
    float3 reflectdir = reflect(EyeVec, normalize(lerp(face, normal, 0.35 * (1 - calm))));
    reflectdir -= 2 * min(0, dot(reflectdir, face)) * face;
    reflectdir = normalize(reflectdir);
    float3 reflected = fogColourSky(reflectdir).rgb;
    // The look can give a colour in place of the sky. An interior has no sky, and its fog
    // colour is too bright for one: there the surface reflects the light of the room, as the
    // water of the cell does, unless the look says otherwise.
    reflected = lerp(reflected, waterVolumeSky.rgb, waterVolumeSky.a);

    // Reflect what is on screen over the sky
    if(reflectsScene)
    {
        // Far away the march is left out; it fades out over the last quarter of a cell.
        float sceneWeight = distant ? saturate((waterVolumeReflectRange - dist) / 2048) : step(1, waterVolumeReflectRange);
        if(sceneWeight > 0)
        {
            float4 scene = reflectScene(IN.pos.xyz, reflectdir);
            reflected = lerp(reflected, scene.rgb, scene.a * sceneWeight);
        }
    }

    // Fade reflection into an inscatter dominated horizon
    reflected = lerp(fog.rgb, reflected, fog.a);

    // Fresnel equation determines reflection/refraction
    float fresnel = dot(-EyeVec, adjustnormal);
    fresnel = 0.02 + pow(saturate(0.9988 - 0.28 * fresnel), 16);
    float3 result = lerp(refracted, reflected, fresnel);

    // Specular lighting, as for the water plane
    float vdotr = dot(-EyeVec, reflect(-sunPos, normal));
    vdotr = saturate(1.0025 * vdotr);
    float3 spec = sunColAdjusted * (pow(vdotr, 170) + 0.07 * pow(vdotr, 4));
    result += spec * fog.a * sun;

#ifdef WATER_VOLUME_LIGHTS
    // The lamps and fires of the game: their glint in the ripples, and a little of their
    // light in the body of the water.
    float3 glint, glow;
    volumePointLights(IN.pos.xyz, EyeVec, normal, face, glint, glow);
    result += (glint + 0.08 * glow * (dot(tint, 1) > 0 ? tint : float3(0.25, 0.5, 0.6))) * fog.a;
#endif

    // Smooth transition at shore line
    result = lerp(result, refracted, shorefactor * fog.a);

    WaterShade shade;
    shade.colour = result;
    shade.eyeVec = EyeVec;
    shade.dist = dist;
    shade.fog = fog;
    shade.ripple = ripple;
    shade.face = face;
    shade.rayDepth = rayDepth;
    shade.waterDepth = rayDepth * abs(dot(EyeVec, face));
    shade.sun = sun;
    return shade;
}

// The standard shading, with the standard ripples.
WaterShade shadeWaterVolume(in WaterVertOut IN, float3 facing, float3 tint, WaterSurfaceLook look)
{
    float dist = length(IN.pos.xyz - eyePos.xyz);
    float3 ripple = surfaceRipples(IN.texcoords.xy, IN.texcoords.zw, dist, IN.pos.xy, look);
    return shadeWaterVolumeRippled(IN, facing, tint, look, ripple);
}

// The last steps of the shading, which belong to the look: glow and opacity.
float4 finishWaterVolume(in WaterVertOut IN, WaterShade shade, float3 tint, WaterSurfaceLook look)
{
    float3 result = shade.colour;

    // Light of the surface's own, in the colour of the water. It does not follow the daylight,
    // so a surface of the usual colour glows in a water blue of its own.
    float3 glowColour = dot(tint, 1) > 0 ? tint : float3(0.25, 0.5, 0.6);
    result += look.glow * glowColour * shade.fog.a;

    // A surface that is not fully water shows what is behind it
    float3 behind = tex2Dproj(sampRefract, IN.screenpos).rgb;
    result = lerp(behind, result, look.opacity);

    return float4(result, 1);
}

// A vertex of a surface, with the normal of the mesh in world space.
struct WaterVolumeVertOut
{
    WaterVertOut water;
    // The colour of the water: the emissive colour of the material
    float3 tint : TEXCOORD4;
    float3 facing : TEXCOORD5;
    // The vertex colour of the mesh; 1 for a surface from the distant land
    float4 color : COLOR0;
    // The texture coordinates of the mesh: the first in xy and the second in zw. A mesh with
    // one set, and a surface from the distant land, has the first in both.
    float4 uv : TEXCOORD6;
};

WaterVolumeVertOut WaterVolumeVS(in float4 pos : POSITION, in float3 normal : NORMAL, in float4 color : COLOR0, in float2 uv : TEXCOORD0, in float2 uv1 : TEXCOORD1)
{
    WaterVolumeVertOut OUT;
    OUT.water = WaterVS(pos);
    OUT.tint = waterVolumeTint;
    OUT.facing = mul(float4(normal, 0), world).xyz;
    OUT.color = color;
    OUT.uv = float4(uv, fmod(waterVolumeMix.w, 8) >= 4 ? uv1 : uv);
    return OUT;
}

// The pixel shader of a surface near the player is a water shader: the standard one in
// XE Mod Water Standard.fx, or the one of a mod.

//------------------------------------------------------------
// The same water among the distant statics, farther away than the game draws.

// It gives the pixel shader the same vertex as a surface near the player, so one water
// shader draws both.
// A distant static keeps a palette index in pos.w. The generator writes the colour of the
// water into the vertex colour of a water subset, and the opacity of the mesh's vertex
// into its alpha when the look asks for it.
WaterVolumeVertOut WaterVolumeDistantVS(in StatVertIn IN)
{
    WaterVolumeVertOut OUT;
    OUT.water = WaterVS(float4(IN.pos.xyz, 1));
    OUT.tint = IN.color.rgb;
    OUT.facing = mul(float4(2 * IN.normal.xyz - 1, 0), world).xyz;
    OUT.color = float4(1, 1, 1, IN.color.a);
    OUT.uv = IN.texcoords.xyxy;
    return OUT;
}
