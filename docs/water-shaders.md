# Water shaders of mods

A mod can give the surface of a water volume a pixel shader of its own: foam, lava, ice,
anything. MGE XE does not ship such looks. It gives the place to plug them in.

## What a mod ships

One file for each shader, `Data Files\shaders\water\<name>.fx`. The file has a pixel shader
named `WaterShaderPS`:

```hlsl
float4 WaterShaderPS(in WaterVolumeVertOut IN) : COLOR0
{
    WaterSurfaceLook look = surfaceLook(IN.color);
    WaterShade shade = shadeWaterVolume(IN.water, IN.facing, IN.tint, look);

    // Change shade.colour here.

    return finishWaterVolume(IN.water, shade, IN.tint, look);
}
```

A surface uses the shader when its look names it (`WaterLook::shader`, see
`MGE_WaterLookSet`). The mod that owns the water volumes sets the look; with True Water it is
the key `shader=<name>` in the look line of the mesh.

The standard look is such a shader too: `shaders\core\XE Mod Water Standard.fx` is the
example above and nothing more. It uses only what is listed here. To change the look of every
water volume that names no shader, put a file of that name into `shaders\core-mods`.

## What the shader gets

| Name | What it is |
| --- | --- |
| `IN.water` | The vertex of the water plane shader: `pos` (world), `texcoords`, `screenpos` |
| `IN.facing` | The normal of the mesh, in the world |
| `IN.tint` | The colour of the water: the emissive colour of the material |
| `IN.color` | The vertex colour of the mesh. From the distant land: white, with the alpha of the mesh when the look has `opacity=vertex`, else 1 |
| `IN.uv` | The texture coordinates of the mesh: the first set in `xy`, the second in `zw` (the first again when the mesh has one set) |
| `sampMesh0` | The base texture of the mesh, with mip levels, wrapped |
| `sampMesh1` | The second texture of the mesh (a decal, a detail or a dark map), the same way. Near the player only |
| `surfaceLook(IN.color)` | The look of the surface: `drift`, `speed`, `scale`, `glow`, `opacity`, `clarity`, `tint`, `reflectsScene`, `distant`, `noDepthTest`, and the free values `p0` to `p3` |
| `shadeWaterVolume(...)` | The standard shading up to the look |
| `shadeWaterVolumeRippled(..., ripple)` | The same with ripples of your own: `ripple` is the normal of the ripples as on a level surface, z up, of length 1. For a normal map, or ripples that move along a flow map |
| `WaterShade` | Its result: `colour`, `eyeVec`, `dist`, `fog`, `ripple`, `face`, `rayDepth` (how far the view ray goes through the water), `waterDepth` (how deep the water is under the point) and `sun` (how much of the sun reaches the point, 0 to 1; it is 1 unless the user has shadows on water volumes on) |
| `finishWaterVolume(...)` | The last steps: glow and opacity |

Everything else of `XE Common.fx`, `XE Mod Water.fx` and `XE Mod Water Volume.fx` can be
used too: `time`, `eyePos`, `sunColAdjusted`, `skyCol`, `fogApply`, `sampDepth`,
`sampRefract`, `reflectScene`.

## Rules

- All water shaders are part of one effect. Give every function and constant of your file a
  name of its own, for example with the name of the shader in front. `WaterShaderPS` is the
  one name that every file has; MGE XE renames it for each file.
- A shader has no pass, no vertex shader, no sampler and no render target of its own.
- Call the standard shading one time. `shadeWaterVolume` and `shadeWaterVolumeRippled` are
  most of a water shader, and each call is a copy of it in your pixel shader. With two calls,
  say one for near and one for distant water, the shader can run out of registers when the
  user turns the light options on (error X4505), and then MGE XE turns the water shaders of
  all mods off. Work out what differs first, the ripple normal for example, and call once.
- The user can turn on shadows, point lights and caustics for water volumes
  (`distant_land.water.volume_*`). They are part of the standard shading, so a shader that
  calls it has them. They make it longer by up to 150 instruction slots: leave room.
- The file name, without `.fx`, is the name of the shader. It is not case sensitive and has
  at most 31 characters.

## When a shader does not compile

MGE XE compiles the main effect again without the water shaders of mods, names each file
that fails with the compiler's message in `mgeXE.log`, and shows a line on the status
overlay. The surfaces then have the standard look. The same happens to a surface whose look
names a shader that is not there.

## Near and far

The same pixel shader draws a surface near the player and the same surface from the distant
land; MGE XE gives both the same vertex. Far away:

- `look.distant` is true, and the colour part of `IN.color` is 1. A tint from the vertex colour is not there.
- The look is the one in the mesh: the generator copies the look line of a water mesh into
  the distant land. A look that a script sets while the game runs does not reach it.
- The base texture and the texture coordinates are those of the baked mesh. There is no
  second texture: do not read `sampMesh1` when `look.distant` is true.

## Limits

- From under the surface every surface has the underwater pass.
- The cost of a water shader is the cost of its author. The standard pass has about 360
  instruction slots.
