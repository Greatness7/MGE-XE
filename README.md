# MGE XE G7 fork

This is a fork of [MGE XE](https://github.com/Hrnchamd/MGE%20XE), the graphics
extender for *The Elder Scrolls III: Morrowind*.

It's branched from the prior [MGE XE Unofficial
Fork](https://github.com/NullCascade/MGE%20XE) and includes features from it.

This is NOT an official MGE XE release. It is my personal fork and it will have
rough edges. Please report problems here rather than on the official MGE XE
page.

## What it does

MGE XE extends Morrowind's visuals with distant land, animated grass, per-pixel
lighting, post-process shaders, and much more. It provides a GUI for configuring
various graphics-related settings and additionally enables the scripting
interface that MWSE Lua mods depend on.

It does not work with OpenMW.

## What's different from MGE XE 0.19.1

The primary expected difference is **performance**. Visuals have been largely
untouched and everything should look about the same as prior MGE XE builds.
Adding new visuals or features was not the initial goal, but some snuck in and
are detailed below.

The majority of changes are oriented around one goal: **reducing draw calls**.

We do this by:

1. Optimizing and simplifying objects into appropriate "LOD" representations
   suitable for rendering in a distant scene.
2. Generating a shared texture atlas and having all objects sample from it
   rather than their own individual textures.
3. Merging all objects of each cell into a single instance, which the prior
   steps' shared atlas makes possible.
4. Computing visibility data and doing runtime *occlusion culling*, so cells
   obscured by landscape do not draw at all.

The end result: Cells that used to cost hundreds or thousands of draw calls now
cost one (or zero if occluded). We create the ideal conditions for the GPU:
Fewer, large draw calls, where everything is rendered in one pass with all
objects sharing the same state, same texture, same shader, same lighting, etc.

On my (9-year-old) machine, with a vanilla install and a 16-cell draw distance,
this results in **6x FPS improvement**. The better your GPU, the more detailed
your mods are, and the larger your draw distance, the more you will benefit.
Very large draw distances can see 15x or more FPS improvement. Even very small
draw distances can see 2x improvements around dense modded cells.

## Other changes

* **Faster generation**

  We ask the generator to produce a lot more data than prior MGE XE builds did,
  but we also made it *much faster*. It's been rewritten from scratch, is now
  natively 64-bit (can handle larger installs), and is thoroughly multi-threaded
  and incremental. Despite doing more work it typically runs full rebuilds
  around 25x faster than the original MGE XE generator. On my 12-core 3.7 GHz
  machine, it went from 8m12s to 19s for a full TR+SHOTN+PC install. The more
  cores you have to throw at it, the larger that difference becomes.

  Since generation is now incremental, subsequent modifications after your first
  generation should be far faster. Typically just a few seconds (depending on
  what kinds of changes they contain). Automatic change detection and
  regeneration can now be enabled so you don't have to revisit the GUI after the
  initial run.

* **Better landscape detail**

  The landscape implementation has been rewritten entirely. In prior MGE XE
  builds the texture quality of landscape would get progressively worse the more
  landmass mods you installed. With large worlds it dedicated only a few pixels
  to each entire cell and relied on an overlaid noise texture to fake detail. In
  this fork, landscape texture quality is much higher and entirely independent
  of world size. Geometry quality is also improved. Distant landscapes now look
  basically identical to near ones.

* **Dark mode GUI**

  The GUI was rewritten from scratch and now features a dark mode theme while
  staying faithful to the original layout. Grass mods are auto-detected and
  displayed in their own list independent of plugins to reduce user mistakes.
  Across the GUI various new knobs for fine-tuning distant land performance are
  now available.

* **TOML configuration**

  All of MGE XE's configuration settings have been moved to a single
  `mgeXE.toml` file in the Morrowind root folder, aligning with other ecosystem
  tooling. The generator now also detects and auto-loads plugin `-metadata.toml`
  files, allowing authors to configure distant land overrides and dynamic
  visibility for their mods without requiring users to complete complicated,
  obscure installation steps.

* **Custom DXVK integration**

  We now ship (and require) a custom build of DXVK that has been modified to
  take advantage of Morrowind-specific domain knowledge. This allows us to make
  various improvements to rendering beyond just distant land optimizations.

  Highlights:

  * **Native depth capture** allows us to avoid re-drawing the entire scene a
    second time just for depth information; we now read what's already on the
    GPU.
  * **Native per-pixel lighting** allows us to pass compact lighting data
    directly to the GPU rather than using slow legacy shader paths,
    significantly reducing PPL overhead.
  * **Expanded light limits**, an optional feature that raises the maximum
    number of lights that can influence an object from 8 to 32, removing most
    lighting seams.
  * **Indexed skinning** allows us to remove the 4-bones-per-shape limitation
    and render skinned objects like NPC hands in a single draw rather than
    dozens.
  * **BC7 texture support**, a newer compression format that preserves
    significantly more detail, providing better compatibility with OpenMW mods.

## Requirements (IMPORTANT, READ CAREFULLY)

This release raises the hardware floor a long way over MGE XE 0.19.1. On
unsupported hardware the symptom is a crash at launch or a permanent black
screen.

- 64-bit Windows 10 (1809 or later) or Windows 11. Windows 7, 8 and 8.1 cannot
  run this.
- A graphics card supporting Vulkan 1.3:
  - NVIDIA GeForce GTX 900 series or GTX 750 Ti (Maxwell) and newer. Kepler
    cards, the GTX 600 and 700 series, will not work.
  - AMD Radeon RX 400 series (Polaris) and newer. HD 7000/8000 and R7/R9 200/300
    will not work.
  - Intel Iris Xe (11th generation Core) or Arc. Older integrated graphics will
    not work.
- Graphics drivers from 2023 or newer.
- A CPU with SSE4.2, so Intel Nehalem (2008) and AMD Bulldozer (2011) or newer.
- The DirectX June 2010 redistributable and the Visual C++ runtime.
- 8 GB or more of VRAM for large mod lists. Less may work but will require
  reducing generator settings below the defaults.
- SSD and your game data installed on it. Technically not required, but the
  generation experience will be excruciating without it.
- [Morrowind Code Patch](https://www.nexusmods.com/morrowind/mods/19510/),
  needed if you want MWSE.
- [MWSE 2.1](https://www.nexusmods.com/morrowind/mods/45468), optional, for Lua
  mods and the in-game settings menu.

## Install

There is no installer. Extract the archive into your Morrowind folder, the one
containing `Morrowind.exe`, overwriting when asked.

The archive includes `d3d9.dll`, the custom build of DXVK that this release
requires. Do not replace it with an official DXVK release. Doing so will cause
out-of-memory crashes as well as the removal of several of our features and
performance improvements.

After installing run `MGEXEgui.exe` from the Morrowind folder:

1. Set your resolution and graphics options on the Graphics tab. Prefer
   borderless windowed mode if possible.
2. Generate distant land on the Distant Land tab. This part can take a while.

Re-run the generator whenever you add or remove mods that change the world, or
enable automatic regeneration.

## Upgrading from MGE XE

Extract over your existing install, run `MGEXEgui.exe`, and regenerate distant
land. That last step is not optional.

Your old settings will not carry over. We do not migrate `MGE.ini` into
`mgeXE.toml`. You will need to set your options up again from the GUI.

Custom "core-mods" shader overrides are most likely not compatible. Uninstalling
them first is recommended. Check your `mgeXE.log` after running the game for
errors. Post-process shaders should be fine.

## Uninstall

Delete `d3d8.dll`, `d3d9.dll`, `dinput8.dll`, `MGEXEgui.exe`, `mgeHost64.exe`,
`mgeXE.toml`, `mge3\MGE XE Default Statics Classifiers.toml`, and `Data
Files\distantland`.

## Credits

A lot of people have worked on MGE and MGE XE over the years.

MGE was written by Timeslip, LizTail, Krzymar and Phal. Hrnchamd has maintained
MGE XE since. The "MGE XE Unofficial Fork" featured contributions from descawed,
Greatness7, NullCascade and vtastek. There's probably more names that belong in
this list but it's hard to keep track.

Additional thanks goes to the play-testers on the MMC discord who've been
helping with bug hunting and feature ideas for months. Thanks to Robin Hjelti,
Storm, vtastek, romavictrix3858, Kynesifnar, MS, Sharmat, Melchior Dahrk and any
others I can't remember at the moment.

## License

GPL v2. Copyright remains with the original authors. This fork adds no further
restrictions.

### Third-party: DXVK

`d3d9.dll` is [DXVK](https://github.com/doitsujin/dxvk), a Direct3D 9 to Vulkan
translation layer. It is not part of MGE XE and is not covered by MGE XE's
license. The build shipped here is modified, not an official DXVK release, so do
not report problems with it to the DXVK project.

DXVK is distributed under the zlib/libpng license:

> Copyright (c) 2017 Philip Rebohle Copyright (c) 2019 Joshua Ashton Copyright
> (c) 2019 Robin Kertels Copyright (c) 2023 Jeffrey Ellison
>
> This software is provided 'as-is', without any express or implied warranty. In
> no event will the authors be held liable for any damages arising from the use
> of this software.
>
> Permission is granted to anyone to use this software for any purpose,
> including commercial applications, and to alter it and redistribute it freely,
> subject to the following restrictions:
>
> - The origin of this software must not be misrepresented; you must not claim
>   that you wrote the original software. If you use this software in a product,
>   an acknowledgment in the product documentation would be appreciated but is
>   not required.
> - Altered source versions must be plainly marked as such, and must not be
>   misrepresented as being the original software.
> - This notice may not be removed or altered from any source distribution.
