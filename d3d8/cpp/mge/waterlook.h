#pragma once

#include <cstddef>
#include <cstdint>

// The look of a water volume surface, beyond its colour: what the mesh and the mod that owns
// it set. A mod reports looks through MGE_WaterLookSet, one per slot, and marks a surface
// with its slot in the specular power of the material (100000 + slot). The struct is a flat
// C layout that is shared with the mod: size comes first, so that a mod built against an
// older or a newer layout can be told apart.
struct WaterLook {
    // sizeof(WaterLook) as the caller has it
    std::uint32_t size;
    // WaterLookFlags
    std::uint32_t flags;
    // Drift of the ripples, in the axes of the mesh, in units per second
    float flow[2];
    // Speed of the ripples; 1 is the standard
    float speed;
    // Size of the ripples; 1 is the standard
    float scale;
    // Light that the surface gives, 0 to 1
    float glow;
    // 1 is water; lower shows what is behind the surface
    float opacity;
    // Free parameters p0 to p3, for a water shader of the mod's own
    float params[4][4];
    // The name of the water shader of a mod, the file Data Files\shaders\water\<name>.fx,
    // NUL terminated; empty for the standard shading. A name that MGE has no shader for
    // gives the standard shading.
    char shader[32];
    // What the surface reflects in place of the sky, red, green and blue from 0 to 1. It
    // counts with WATER_LOOK_HAS_SKY.
    float sky[3];
    // Over how many units of water what is under the surface fades into the colour of deep
    // water; 800 is the standard. Small is murky, and 0 shows nothing of what is under it.
    float clarity;
};

enum WaterLookFlags : std::uint32_t {
    // The surface reflects what is on screen; otherwise the sky only
    WATER_LOOK_REFLECTS_SCENE = 1,
    // The vertex colour of the mesh tints the water
    WATER_LOOK_TINT_FROM_VERTEX = 2,
    // The vertex alpha of the mesh is the opacity of the water
    WATER_LOOK_OPACITY_FROM_VERTEX = 4,
    // The look gives the colour that the surface reflects in place of the sky. Without it
    // that is the sky outdoors, and the light of the room in an interior.
    WATER_LOOK_HAS_SKY = 8,
};

namespace WaterLooks {
    // Slots that a mod can use. Slot 0 is the standard look and cannot be set.
    constexpr unsigned int maxSlots = 4096;

    // The specular power of a material that marks a surface with a look slot
    constexpr float slotMarkerBase = 100000.0f;

    // Keeps a copy of the look for the slot; a null look clears the slot. Fields past the
    // caller's size keep their standard values.
    void set(unsigned int slot, const WaterLook* look);

    // The look of a slot: the standard look for slot 0, for a slot never set, and for a slot
    // out of range.
    const WaterLook& get(unsigned int slot);

    // The standard look
    const WaterLook& standard();

    // Reads a look line, the text of the string data of a mesh after "wv:", for example
    // "flow=0,-140 speed=1.2 shader=foam p0=0.4,0.5". Keys it does not know are left out.
    // The mod that owns the water reads the same line for the surfaces near the player.
    WaterLook parse(const char* text, size_t length);

    // The looks of distant water. The generator copies the look line of a water mesh into the
    // distant land, and the loader gives each different look an index, which a distant subset
    // carries in its water byte: firstDistantLook plus the index. The values under it are a
    // subset without a look: 1 reflects the scene, 2 the sky only. drySpace is no water at
    // all: a closed shape inside which no water is drawn (DistantLand::countDrySpaces). It
    // comes in the same visible set as the water, and no pass draws it.
    constexpr unsigned int drySpace = 3;
    constexpr unsigned int firstDistantLook = 4;
    void clearDistant();
    // The water byte for a look; kind (1 or 2) when the table is full.
    std::uint8_t addDistant(const WaterLook& look, std::uint8_t kind);
    const WaterLook& distant(std::uint8_t water);
}
