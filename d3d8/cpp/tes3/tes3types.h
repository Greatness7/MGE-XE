#pragma once

// Morrowind engine types as laid out by Morrowind.exe v1.6.1820.
//
// Naming authority is MWSE's `MWSE/TES3*.h`; see docs/architecture/mwbridge.md.
// Only the members MGE actually touches are named. Everything else is explicit
// padding, sized so the `static_assert(sizeof(...))` below stays honest.
//
// Not to be confused with `d3d8/cpp/mwse/tes3types.h`, which is the older and
// unrelated MWSE-interop header. Quoted includes resolve relative to the
// including file first, so the two never collide in practice.

#include "tes3/nitypes.h"

#include <cstddef>
#include <cstdint>

namespace TES3 {

struct Cell;
struct Reference;

//-----------------------------------------------------------------------------
// Records
//-----------------------------------------------------------------------------

namespace ObjectFlag {
    enum Flag : unsigned int {
        Modified = 0x2,
        LinksResolved = 0x8,
        NoCollision = 0x10,
        Delete = 0x20,
        Persistent = 0x400,
        Disabled = 0x800,
        SelectedByConsole = 0x1000,
        Blocked = 0x2000,
    };
}

struct BaseObject {
    void* vTable;              // 0x00
    unsigned int objectType;   // 0x04  four-character record tag
    unsigned int objectFlags;  // 0x08
    void* sourceMod;           // 0x0C
};
static_assert(sizeof(BaseObject) == 0x10, "TES3::BaseObject failed size validation");
static_assert(offsetof(BaseObject, objectFlags) == 0x8, "TES3::BaseObject::objectFlags failed offset validation");

struct Object : BaseObject {
    NI::Pointer<NI::Node> sceneNode;            // 0x10
    void* owningCollection;                     // 0x14
    void* referenceToThis;                      // 0x18
    Object* previousInCollection;               // 0x1C
    Object* nextInCollection;                   // 0x20
    NI::Pointer<NI::Node> sceneCollisionRoot;   // 0x24
};
static_assert(sizeof(Object) == 0x28, "TES3::Object failed size validation");

struct Reference : Object {
    BaseObject* baseObject;    // 0x28
    NI::Point3 orientation;    // 0x2C
    NI::Point3 position;       // 0x38
    void* attachments;         // 0x44
    unsigned int sourceID;     // 0x48
    unsigned int targetID;     // 0x4C
};
static_assert(sizeof(Reference) == 0x50, "TES3::Reference failed size validation");
static_assert(offsetof(Reference, position) == 0x38, "TES3::Reference::position failed offset validation");

struct GameSetting : BaseObject {
    union {
        long asLong;
        float asFloat;
        char* asString;
    } value;        // 0x10
    long index;     // 0x14
};
static_assert(sizeof(GameSetting) == 0x18, "TES3::GameSetting failed size validation");
static_assert(offsetof(GameSetting, value) == 0x10, "TES3::GameSetting::value failed offset validation");

struct GlobalVariable : BaseObject {
    char pad_10[0x24];  // 0x10
    float value;        // 0x34
};
static_assert(sizeof(GlobalVariable) == 0x38, "TES3::GlobalVariable failed size validation");
static_assert(offsetof(GlobalVariable, value) == 0x34, "TES3::GlobalVariable::value failed offset validation");

struct Dialogue : BaseObject {
    char pad_10[0x1C];   // 0x10
    int journalIndex;    // 0x2C
};
static_assert(sizeof(Dialogue) == 0x30, "TES3::Dialogue failed size validation");
static_assert(offsetof(Dialogue, journalIndex) == 0x2C, "TES3::Dialogue::journalIndex failed offset validation");

namespace WeaponType {
    enum Type : unsigned char {
        ShortBlade1H = 0x0,
        LongBlade1H = 0x1,
        LongBlade2H = 0x2,
        Blunt1H = 0x3,
        Blunt2Close = 0x4,
        Blunt2Wide = 0x5,
        Spear2H = 0x6,
        Axe1H = 0x7,
        Axe2H = 0x8,
        Bow = 0x9,
        Crossbow = 0xA,
        Thrown = 0xB,
        Arrow = 0xC,
        Bolt = 0xD,
    };
}

struct Weapon {
    char pad_00[0x5C];                 // 0x00
    WeaponType::Type weaponType;       // 0x5C
    char pad_5D[0x1B];                 // 0x5D
};
static_assert(sizeof(Weapon) == 0x78, "TES3::Weapon failed size validation");
static_assert(offsetof(Weapon, weaponType) == 0x5C, "TES3::Weapon::weaponType failed offset validation");

struct NPC {
    char pad_00[0x70];   // 0x00
    char* name;          // 0x70
    char pad_74[0x7C];   // 0x74
};
static_assert(sizeof(NPC) == 0xF0, "TES3::NPC failed size validation");
static_assert(offsetof(NPC, name) == 0x70, "TES3::NPC::name failed offset validation");

struct NPCInstance {
    char pad_00[0x6C];   // 0x00
    NPC* baseNPC;        // 0x6C
    char pad_70[0x8];    // 0x70
};
static_assert(sizeof(NPCInstance) == 0x78, "TES3::NPCInstance failed size validation");
static_assert(offsetof(NPCInstance, baseNPC) == 0x6C, "TES3::NPCInstance::baseNPC failed offset validation");

struct EquipmentStack {
    BaseObject* object;  // 0x00
    void* itemData;      // 0x04
};
static_assert(sizeof(EquipmentStack) == 0x8, "TES3::EquipmentStack failed size validation");

//-----------------------------------------------------------------------------
// Cell
//-----------------------------------------------------------------------------

namespace CellFlag {
    enum Flag : unsigned int {
        Interior = 0x1,
        HasWater = 0x2,
        SleepIsIllegal = 0x4,
        WasLoaded = 0x8,
        TempRefsLoaded = 0x10,
        MarkerDrawn = 0x20,
        // 0x40 is unnamed in both MWSE and the IDB, and the TESCS never writes it.
        BehavesAsExterior = 0x80,
    };
}

struct Cell {
    char pad_00[0x10];         // 0x00
    char* name;                // 0x10
    char pad_14[0x4];          // 0x14
    unsigned int cellFlags;    // 0x18
    union {
        struct {
            NI::PackedColor regionMapColor;  // 0x00
            void* landscape;                 // 0x04
            int gridX;                       // 0x08
            int gridY;                       // 0x0C
        } exterior;
        struct {
            NI::PackedColor ambientColor;    // 0x00
            NI::PackedColor sunColor;        // 0x04
            NI::PackedColor fogColor;        // 0x08
            float fogDensity;                // 0x0C
        } interior;
    } variantData;             // 0x1C
    char pad_2C[0x64];         // 0x2C
    union {
        float waterLevel;
        void* region;
    } waterLevelOrRegion;      // 0x90

    bool getIsInterior() const { return (cellFlags & CellFlag::Interior) != 0; }
    bool getHasWater() const { return (cellFlags & CellFlag::HasWater) != 0; }
    bool getSleepIsIllegal() const { return (cellFlags & CellFlag::SleepIsIllegal) != 0; }
    bool getBehavesAsExterior() const { return (cellFlags & CellFlag::BehavesAsExterior) != 0; }
};
static_assert(sizeof(Cell) == 0x94, "TES3::Cell failed size validation");
static_assert(offsetof(Cell, name) == 0x10, "TES3::Cell::name failed offset validation");
static_assert(offsetof(Cell, cellFlags) == 0x18, "TES3::Cell::cellFlags failed offset validation");
static_assert(offsetof(Cell, variantData) == 0x1C, "TES3::Cell::variantData failed offset validation");
static_assert(offsetof(Cell, waterLevelOrRegion) == 0x90, "TES3::Cell::waterLevelOrRegion failed offset validation");

//-----------------------------------------------------------------------------
// Weather
//-----------------------------------------------------------------------------

struct Weather {
    char pad_00[0x4];    // 0x00
    int index;           // 0x04
    char pad_08[0x310];  // 0x08
};
static_assert(sizeof(Weather) == 0x318, "TES3::Weather failed size validation");
static_assert(offsetof(Weather, index) == 0x4, "TES3::Weather::index failed offset validation");

struct Moon {
    char pad_00[0x10];               // 0x00
    NI::TriShape* sgTriMoonShadow;   // 0x10
    char pad_14[0x148];              // 0x14
};
static_assert(sizeof(Moon) == 0x15C, "TES3::Moon failed size validation");
static_assert(offsetof(Moon, sgTriMoonShadow) == 0x10, "TES3::Moon::sgTriMoonShadow failed offset validation");

struct WeatherController {
    static constexpr int MAX_WEATHER_COUNT = 10;

    NI::Node* sgSunVis;                              // 0x00
    char pad_04[0x10];                               // 0x04
    Weather* arrayWeathers[MAX_WEATHER_COUNT];       // 0x14
    Weather* currentWeather;                         // 0x3C
    Weather* nextWeather;                            // 0x40
    Moon* moonSecunda;                               // 0x44
    Moon* moonMasser;                                // 0x48
    char pad_4C[0x3C];                               // 0x4C
    NI::Pointer<NI::TriShape> shTriSunBase;          // 0x88
    char pad_8C[0x4];                                // 0x8C
    NI::Point3 currentSkyColor;                      // 0x90
    NI::Point3 currentFogColor;                      // 0x9C
    char pad_A8[0x10];                               // 0xA8
    NI::Point3 windVelocityCurrWeather;              // 0xB8
    char pad_C4[0x18];                               // 0xC4
    float sunriseHour;                               // 0xDC
    float sunsetHour;                                // 0xE0
    float sunriseDuration;                           // 0xE4
    float sunsetDuration;                            // 0xE8
    char pad_EC[0x84];                               // 0xEC
    float transitionScalar;                          // 0x170
    char pad_174[0x7C];                              // 0x174
};
static_assert(sizeof(WeatherController) == 0x1F0, "TES3::WeatherController failed size validation");
static_assert(offsetof(WeatherController, arrayWeathers) == 0x14, "TES3::WeatherController::arrayWeathers failed offset validation");
static_assert(offsetof(WeatherController, currentWeather) == 0x3C, "TES3::WeatherController::currentWeather failed offset validation");
static_assert(offsetof(WeatherController, moonMasser) == 0x48, "TES3::WeatherController::moonMasser failed offset validation");
static_assert(offsetof(WeatherController, shTriSunBase) == 0x88, "TES3::WeatherController::shTriSunBase failed offset validation");
static_assert(offsetof(WeatherController, currentSkyColor) == 0x90, "TES3::WeatherController::currentSkyColor failed offset validation");
static_assert(offsetof(WeatherController, currentFogColor) == 0x9C, "TES3::WeatherController::currentFogColor failed offset validation");
static_assert(offsetof(WeatherController, windVelocityCurrWeather) == 0xB8, "TES3::WeatherController::windVelocityCurrWeather failed offset validation");
static_assert(offsetof(WeatherController, sunriseHour) == 0xDC, "TES3::WeatherController::sunriseHour failed offset validation");
static_assert(offsetof(WeatherController, transitionScalar) == 0x170, "TES3::WeatherController::transitionScalar failed offset validation");

//-----------------------------------------------------------------------------
// Mobiles
//-----------------------------------------------------------------------------

enum class AttackAnimationState : signed char {
    Idle = 0x0,
    Ready = 0x1,
    SwingUp = 0x2,
    SwingDown = 0x3,
    SwingHit = 0x4,
    SwingFollowLight = 0x5,
    SwingFollowMed = 0x6,
    SwingFollowHeavy = 0x7,
    ReadyingWeap = 0x8,
    UnreadyWeap = 0x9,
    Casting = 0xA,
    CastingFollow = 0xB,
    ReadyingMagic = 0xC,
    UnreadyMagic = 0xD,
    Knockdown = 0xE,
    KnockedOut = 0xF,
    PickingProbing = 0x10,
    Wait = 0x11,
    Dying = 0x12,
    Dead = 0x13,
};

struct ActionData {
    char pad_00[0x11];                       // 0x00
    AttackAnimationState animStateAttack;    // 0x11
    char pad_12[0x5E];                       // 0x12
};
static_assert(sizeof(ActionData) == 0x70, "TES3::ActionData failed size validation");
static_assert(offsetof(ActionData, animStateAttack) == 0x11, "TES3::ActionData::animStateAttack failed offset validation");

struct PlayerAnimationController {
    char pad_00[0xD8];            // 0x00
    NI::Point3 cameraOffset;      // 0xD8
    char pad_E4[0x4];             // 0xE4
    bool is3rdPerson;             // 0xE8
    char pad_E9[0x3B];            // 0xE9
};
static_assert(sizeof(PlayerAnimationController) == 0x124, "TES3::PlayerAnimationController failed size validation");
static_assert(offsetof(PlayerAnimationController, cameraOffset) == 0xD8, "TES3::PlayerAnimationController::cameraOffset failed offset validation");
static_assert(offsetof(PlayerAnimationController, is3rdPerson) == 0xE8, "TES3::PlayerAnimationController::is3rdPerson failed offset validation");

struct MobileObject {
    void* vTable;         // 0x00
    char pad_04[0x10];    // 0x04
    Reference* reference; // 0x14
    char pad_18[0x68];    // 0x18
};
static_assert(sizeof(MobileObject) == 0x80, "TES3::MobileObject failed size validation");
static_assert(offsetof(MobileObject, reference) == 0x14, "TES3::MobileObject::reference failed offset validation");

struct MobileActor : MobileObject {
    char pad_80[0x4C];                                  // 0x80
    ActionData actionData;                              // 0xCC
    char pad_13C[0x108];                                // 0x13C
    PlayerAnimationController* animationController;     // 0x244
    char pad_248[0x140];                                // 0x248
    EquipmentStack* readiedWeapon;                      // 0x388
    char pad_38C[0x24];                                 // 0x38C
};
static_assert(sizeof(MobileActor) == 0x3B0, "TES3::MobileActor failed size validation");
static_assert(offsetof(MobileActor, actionData) == 0xCC, "TES3::MobileActor::actionData failed offset validation");
static_assert(offsetof(MobileActor, animationController) == 0x244, "TES3::MobileActor::animationController failed offset validation");
static_assert(offsetof(MobileActor, readiedWeapon) == 0x388, "TES3::MobileActor::readiedWeapon failed offset validation");

struct MobileNPC : MobileActor {
    char pad_3B0[0x1B0];       // 0x3B0
    NPCInstance* npcInstance;  // 0x560
    char pad_564[0x8];         // 0x564
};
static_assert(sizeof(MobileNPC) == 0x56C, "TES3::MobileNPC failed size validation");
static_assert(offsetof(MobileNPC, npcInstance) == 0x560, "TES3::MobileNPC::npcInstance failed offset validation");

struct MobilePlayer : MobileNPC {
    char pad_56C[0x48];        // 0x56C
    bool vanityDisabled;       // 0x5B4
    char pad_5B5[0x2];         // 0x5B5
    bool alwaysRun;            // 0x5B7
    bool autoRun;              // 0x5B8
    bool sleeping;             // 0x5B9
    char pad_5BA[0x1];         // 0x5BA
    bool waiting;              // 0x5BB
    char pad_5BC[0xD8];        // 0x5BC
};
static_assert(sizeof(MobilePlayer) == 0x694, "TES3::MobilePlayer failed size validation");
static_assert(offsetof(MobilePlayer, vanityDisabled) == 0x5B4, "TES3::MobilePlayer::vanityDisabled failed offset validation");
static_assert(offsetof(MobilePlayer, alwaysRun) == 0x5B7, "TES3::MobilePlayer::alwaysRun failed offset validation");
static_assert(offsetof(MobilePlayer, autoRun) == 0x5B8, "TES3::MobilePlayer::autoRun failed offset validation");
static_assert(offsetof(MobilePlayer, sleeping) == 0x5B9, "TES3::MobilePlayer::sleeping failed offset validation");
static_assert(offsetof(MobilePlayer, waiting) == 0x5BB, "TES3::MobilePlayer::waiting failed offset validation");

struct ProcessManager {
    MobilePlayer* mobilePlayer;  // 0x00
    char pad_04[0x82C];          // 0x04
};
static_assert(sizeof(ProcessManager) == 0x830, "TES3::ProcessManager failed size validation");

struct MobManager {
    char pad_00[0x24];                // 0x00
    ProcessManager* processManager;   // 0x24
    char pad_28[0x64];                // 0x28
};
static_assert(sizeof(MobManager) == 0x8C, "TES3::MobManager failed size validation");
static_assert(offsetof(MobManager, processManager) == 0x24, "TES3::MobManager::processManager failed offset validation");

//-----------------------------------------------------------------------------
// Controllers
//-----------------------------------------------------------------------------

namespace MusicFlag {
    enum Flag : unsigned int {
        FilterGraphValid = 0x1,
        Playing = 0x2,
        Paused = 0x4,
    };
}

struct AudioController {
    char pad_00[0x8];                 // 0x00
    unsigned int musicFlags;          // 0x08
    char pad_0C[0x280];               // 0x0C
    float volumeNextTrack;            // 0x28C
    char pad_290[0x4];                // 0x290
    float volumeMusic;                // 0x294
    char pad_298[0x14];               // 0x298
    NI::Point3 listenerPosition;      // 0x2AC
    char pad_2B8[0x20];               // 0x2B8
};
static_assert(sizeof(AudioController) == 0x2D8, "TES3::AudioController failed size validation");
static_assert(offsetof(AudioController, musicFlags) == 0x8, "TES3::AudioController::musicFlags failed offset validation");
static_assert(offsetof(AudioController, volumeNextTrack) == 0x28C, "TES3::AudioController::volumeNextTrack failed offset validation");
static_assert(offsetof(AudioController, volumeMusic) == 0x294, "TES3::AudioController::volumeMusic failed offset validation");
static_assert(offsetof(AudioController, listenerPosition) == 0x2AC, "TES3::AudioController::listenerPosition failed offset validation");

// IDA names this class `CursorController`; MWSE names it `MouseController`.
struct MouseController {
    char pad_00[0x24];              // 0x00
    NI::Point3 minimumPosition;     // 0x24
    NI::Point3 maximumPosition;     // 0x30
    char pad_3C[0x14];              // 0x3C
};
static_assert(sizeof(MouseController) == 0x50, "TES3::MouseController failed size validation");
static_assert(offsetof(MouseController, minimumPosition) == 0x24, "TES3::MouseController::minimumPosition failed offset validation");

struct WaterController {
    char pad_00[0xB4];                      // 0x00
    NI::Pointer<NI::Node> waterPlane;       // 0xB4
    char pad_B8[0x10];                      // 0xB8
};
static_assert(sizeof(WaterController) == 0xC8, "TES3::WaterController failed size validation");
static_assert(offsetof(WaterController, waterPlane) == 0xB4, "TES3::WaterController::waterPlane failed offset validation");

struct Fader {
    bool isActive;       // 0x00
    char pad_01[0x3F];   // 0x01
};
static_assert(sizeof(Fader) == 0x40, "TES3::Fader failed size validation");

// MWSE leaves `WorldController::shadowManager` as `void*`; this layout is the
// IDB's `ShadowManager` UDT.
struct ShadowManager {
    char bHighDetailShadows;      // 0x00
    char pad_01[0x3];             // 0x01
    void* list_4;                 // 0x04
    NI::Camera* sgShadowCamera;   // 0x08
    char field_C;                 // 0x0C
    char pad_0D[0x3];             // 0x0D
    NI::Node* sgWorldRoot;        // 0x10
    short maxShadows;             // 0x14
    short maxShadowsPerObject;    // 0x16
    char pad_18[0x58];            // 0x18
};
static_assert(sizeof(ShadowManager) == 0x70, "TES3::ShadowManager failed size validation");
static_assert(offsetof(ShadowManager, sgShadowCamera) == 0x8, "TES3::ShadowManager::sgShadowCamera failed offset validation");
static_assert(offsetof(ShadowManager, field_C) == 0xC, "TES3::ShadowManager::field_C failed offset validation");
static_assert(offsetof(ShadowManager, maxShadows) == 0x14, "TES3::ShadowManager::maxShadows failed offset validation");

//-----------------------------------------------------------------------------
// Singletons
//-----------------------------------------------------------------------------

struct WorldControllerRenderCamera {
    struct CameraData {
        NI::Pointer<NI::Camera> camera;  // 0x00
        NI::Pointer<NI::Node> unknown_0x4;  // 0x04
        float fovDegrees;                // 0x08
        float nearPlaneDistance;         // 0x0C
        float farPlaneDistance;          // 0x10
        unsigned int viewportWidth;      // 0x14
        unsigned int viewportHeight;     // 0x18
    };

    void* vTable;                        // 0x00
    NI::Pointer<NI::Renderer> renderer;  // 0x04
    NI::Pointer<NI::Node> root;          // 0x08
    NI::Pointer<NI::Node> cameraRoot;    // 0x0C
    CameraData cameraData;               // 0x10
};
static_assert(sizeof(WorldControllerRenderCamera::CameraData) == 0x1C, "TES3::WorldControllerRenderCamera::CameraData failed size validation");
static_assert(sizeof(WorldControllerRenderCamera) == 0x2C, "TES3::WorldControllerRenderCamera failed size validation");
static_assert(offsetof(WorldControllerRenderCamera, cameraData) == 0x10, "TES3::WorldControllerRenderCamera::cameraData failed offset validation");

enum class MusicSituation : int {
    Explore = 0,
    Combat = 1,
    Uninterruptible = 2,
};

struct WorldController {
    char pad_00[0x14];                                  // 0x00
    float framesPerSecond;                              // 0x14
    char pad_18[0x8];                                   // 0x18
    unsigned int systemTimeMillis;                      // 0x20
    char pad_24[0x8];                                   // 0x24
    float deltaTime;                                    // 0x2C
    NI::Renderer* renderer;                             // 0x30
    AudioController* audioController;                   // 0x34
    char pad_38[0x18];                                  // 0x38
    MouseController* mouseController;                   // 0x50
    char pad_54[0x4];                                   // 0x54
    WeatherController* weatherController;               // 0x58
    MobManager* mobManager;                             // 0x5C
    char pad_60[0x18];                                  // 0x60
    int viewWidth;                                      // 0x78
    int viewHeight;                                     // 0x7C
    char pad_80[0x4];                                   // 0x80
    int bShadows;                                       // 0x84
    char pad_88[0xC];                                   // 0x88
    bool cursorOff;                                     // 0x94
    char pad_95[0x3];                                   // 0x95
    float aiDistanceScale;                              // 0x98
    char pad_9C[0xC];                                   // 0x9C
    GlobalVariable* gvarGameHour;                       // 0xA8
    char pad_AC[0xC];                                   // 0xAC
    GlobalVariable* gvarDaysPassed;                     // 0xB8
    char pad_BC[0xC];                                   // 0xBC
    void* Win32_hWndParent;                             // 0xC8
    char pad_CC[0xA];                                   // 0xCC
    bool flagMenuMode;                                  // 0xD6
    char pad_D7[0x11];                                  // 0xD7
    float mouseSensitivity;                             // 0xE8
    float horzSensitivity;                              // 0xEC
    char pad_F0[0x4];                                   // 0xF0
    NI::Node* nodeCursor;                               // 0xF4
    WorldControllerRenderCamera splashscreenCamera;     // 0xF8
    WorldControllerRenderCamera worldCamera;            // 0x124
    // The first-person arms camera. MGE writes the player FOV into it, which is
    // correct; it was only ever misnamed `eSkyFOV`.
    WorldControllerRenderCamera armCamera;              // 0x150
    WorldControllerRenderCamera menuCamera;             // 0x17C
    char pad_1A8[0x108];                                // 0x1A8
    WorldControllerRenderCamera shadowCamera;           // 0x2B0
    ShadowManager* shadowManager;                       // 0x2DC
    void* mapController;                                // 0x2E0
    char pad_2E4[0x6C];                                 // 0x2E4
    MusicSituation musicSituation;                      // 0x350
    Fader* transitionFader;                             // 0x354
    char pad_358[0x1C];                                 // 0x358

    static WorldController* get() {
        return *reinterpret_cast<WorldController**>(0x7C67DC);
    }
};
static_assert(sizeof(WorldController) == 0x374, "TES3::WorldController failed size validation");
static_assert(offsetof(WorldController, framesPerSecond) == 0x14, "TES3::WorldController::framesPerSecond failed offset validation");
static_assert(offsetof(WorldController, systemTimeMillis) == 0x20, "TES3::WorldController::systemTimeMillis failed offset validation");
static_assert(offsetof(WorldController, deltaTime) == 0x2C, "TES3::WorldController::deltaTime failed offset validation");
static_assert(offsetof(WorldController, renderer) == 0x30, "TES3::WorldController::renderer failed offset validation");
static_assert(offsetof(WorldController, audioController) == 0x34, "TES3::WorldController::audioController failed offset validation");
static_assert(offsetof(WorldController, mouseController) == 0x50, "TES3::WorldController::mouseController failed offset validation");
static_assert(offsetof(WorldController, weatherController) == 0x58, "TES3::WorldController::weatherController failed offset validation");
static_assert(offsetof(WorldController, mobManager) == 0x5C, "TES3::WorldController::mobManager failed offset validation");
static_assert(offsetof(WorldController, viewWidth) == 0x78, "TES3::WorldController::viewWidth failed offset validation");
static_assert(offsetof(WorldController, viewHeight) == 0x7C, "TES3::WorldController::viewHeight failed offset validation");
static_assert(offsetof(WorldController, bShadows) == 0x84, "TES3::WorldController::bShadows failed offset validation");
static_assert(offsetof(WorldController, cursorOff) == 0x94, "TES3::WorldController::cursorOff failed offset validation");
static_assert(offsetof(WorldController, aiDistanceScale) == 0x98, "TES3::WorldController::aiDistanceScale failed offset validation");
static_assert(offsetof(WorldController, gvarGameHour) == 0xA8, "TES3::WorldController::gvarGameHour failed offset validation");
static_assert(offsetof(WorldController, gvarDaysPassed) == 0xB8, "TES3::WorldController::gvarDaysPassed failed offset validation");
static_assert(offsetof(WorldController, Win32_hWndParent) == 0xC8, "TES3::WorldController::Win32_hWndParent failed offset validation");
static_assert(offsetof(WorldController, flagMenuMode) == 0xD6, "TES3::WorldController::flagMenuMode failed offset validation");
static_assert(offsetof(WorldController, mouseSensitivity) == 0xE8, "TES3::WorldController::mouseSensitivity failed offset validation");
static_assert(offsetof(WorldController, horzSensitivity) == 0xEC, "TES3::WorldController::horzSensitivity failed offset validation");
static_assert(offsetof(WorldController, nodeCursor) == 0xF4, "TES3::WorldController::nodeCursor failed offset validation");
static_assert(offsetof(WorldController, splashscreenCamera) == 0xF8, "TES3::WorldController::splashscreenCamera failed offset validation");
static_assert(offsetof(WorldController, worldCamera) == 0x124, "TES3::WorldController::worldCamera failed offset validation");
static_assert(offsetof(WorldController, armCamera) == 0x150, "TES3::WorldController::armCamera failed offset validation");
static_assert(offsetof(WorldController, menuCamera) == 0x17C, "TES3::WorldController::menuCamera failed offset validation");
static_assert(offsetof(WorldController, shadowCamera) == 0x2B0, "TES3::WorldController::shadowCamera failed offset validation");
static_assert(offsetof(WorldController, shadowManager) == 0x2DC, "TES3::WorldController::shadowManager failed offset validation");
static_assert(offsetof(WorldController, mapController) == 0x2E0, "TES3::WorldController::mapController failed offset validation");
static_assert(offsetof(WorldController, musicSituation) == 0x350, "TES3::WorldController::musicSituation failed offset validation");
static_assert(offsetof(WorldController, transitionFader) == 0x354, "TES3::WorldController::transitionFader failed offset validation");

struct NonDynamicData {
    char pad_00[0x18];        // 0x00
    GameSetting** GMSTs;      // 0x18
    char pad_1C[0xB390];      // 0x1C
};
static_assert(sizeof(NonDynamicData) == 0xB3AC, "TES3::NonDynamicData failed size validation");
static_assert(offsetof(NonDynamicData, GMSTs) == 0x18, "TES3::NonDynamicData::GMSTs failed offset validation");

struct DataHandler {
    NonDynamicData* nonDynamicData;    // 0x0000
    char pad_0004[0x98];               // 0x0004
    NI::FogProperty* sgFogProperty;    // 0x009C
    char pad_00A0[0xC];                // 0x00A0
    Cell* currentInteriorCell;         // 0x00AC
    char pad_00B0[0xB43C];             // 0x00B0
    WaterController* waterController;  // 0xB4EC
    char pad_B4F0[0x50];               // 0xB4F0
    Cell* currentCell;                 // 0xB540
    char pad_B544[0x14];               // 0xB544

    static DataHandler* get() {
        return *reinterpret_cast<DataHandler**>(0x7C67E0);
    }
};
static_assert(sizeof(DataHandler) == 0xB558, "TES3::DataHandler failed size validation");
static_assert(offsetof(DataHandler, sgFogProperty) == 0x9C, "TES3::DataHandler::sgFogProperty failed offset validation");
static_assert(offsetof(DataHandler, currentInteriorCell) == 0xAC, "TES3::DataHandler::currentInteriorCell failed offset validation");
static_assert(offsetof(DataHandler, waterController) == 0xB4EC, "TES3::DataHandler::waterController failed offset validation");
static_assert(offsetof(DataHandler, currentCell) == 0xB540, "TES3::DataHandler::currentCell failed offset validation");

struct Game;

struct Game_vTable {
    char pad_00[0x50];                            // 0x00
    void(__thiscall* setGamma)(Game*, float);     // 0x50
};
static_assert(offsetof(Game_vTable, setGamma) == 0x50, "TES3::Game_vTable::setGamma failed offset validation");

struct Game {
    Game_vTable* vTable;         // 0x00
    const char* registryKey;     // 0x04
    int windowWidth;             // 0x08
    int windowHeight;            // 0x0C
    int screenDepth;             // 0x10
    int backBuffers;             // 0x14
    int multiSamples;            // 0x18
    bool fullscreen;             // 0x1C
    char pad_1D[0x1F];           // 0x1D
    float gamma;                 // 0x3C
    char pad_40[0xC];            // 0x40
    float renderDistance;        // 0x4C
    char pad_50[0x98];           // 0x50
    Reference* playerTarget;     // 0xE8
    char pad_EC[0x24];           // 0xEC

    static Game* get() {
        return *reinterpret_cast<Game**>(0x7C6CDC);
    }
};
static_assert(sizeof(Game) == 0x110, "TES3::Game failed size validation");
static_assert(offsetof(Game, fullscreen) == 0x1C, "TES3::Game::fullscreen failed offset validation");
static_assert(offsetof(Game, gamma) == 0x3C, "TES3::Game::gamma failed offset validation");
static_assert(offsetof(Game, renderDistance) == 0x4C, "TES3::Game::renderDistance failed offset validation");
static_assert(offsetof(Game, playerTarget) == 0xE8, "TES3::Game::playerTarget failed offset validation");

}  // namespace TES3
