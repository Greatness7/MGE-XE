// This file contains code from fps optimizer.
// Memory layout originally worked out by Alexander Stasenko.
//
// Struct and field names follow MWSE; see docs/architecture/mwbridge.md.

#include "mwbridge.h"
#include "mwpatches.h"
#include "tes3/nitypes.h"
#include "tes3/tes3addresses.h"
#include "tes3/tes3types.h"
#include "assert.h"

#include <algorithm>
#include <cmath>
#include <cstring>


static MWBridge m_instance;

namespace {

// Reading a fixed global. Morrowind.exe is a fixed target, so these are stable.
template <typename T>
inline T& globalAt(uintptr_t address) {
    return *reinterpret_cast<T*>(address);
}

inline TES3::WorldController* worldController() {
    return TES3::WorldController::get();
}

inline TES3::DataHandler* dataHandler() {
    return TES3::DataHandler::get();
}

inline TES3::WeatherController* weatherController() {
    return worldController()->weatherController;
}

// The four frustum planes MGE rewrites when the FOV changes. Left and right are
// mirrored, as are top and bottom, so the aspect ratio is right/top.
void writeFrustumFOV(NI::Frustum& frustum, float fovtan, float fovtanaspect) {
    frustum.left = -fovtan;
    frustum.right = fovtan;
    frustum.top = fovtanaspect;
    frustum.bottom = -fovtanaspect;
}

}  // namespace

//-----------------------------------------------------------------------------

MWBridge::MWBridge() : m_loaded(false), eShadowFOV(0) {
}

//-----------------------------------------------------------------------------

MWBridge::~MWBridge() {
}

//-----------------------------------------------------------------------------

MWBridge* MWBridge::get() {
    return &m_instance;
}

//-----------------------------------------------------------------------------

// Load - Latches the bridge as connected. Everything is now resolved at the
// point of use through the singletons, so there is no pointer cache to fill;
// the latch remains because Present() drives one-time engine patching off the
// !IsLoaded() && CanLoad() edge.
void MWBridge::Load() {
    m_loaded = true;
}

//-----------------------------------------------------------------------------

bool MWBridge::CanLoad() {
    // Reads a static address, so the game does not need to be loaded.
    return dataHandler() != nullptr;
}

//-----------------------------------------------------------------------------

DWORD MWBridge::GetShadowToggleAddr() {
    assert(m_loaded);
    auto shadowManager = worldController()->shadowManager;
    return shadowManager ? reinterpret_cast<DWORD>(&shadowManager->field_C) : 0;
}

//-----------------------------------------------------------------------------

DWORD MWBridge::GetShadowRealAddr() {
    assert(m_loaded);
    auto shadowManager = worldController()->shadowManager;
    return shadowManager ? reinterpret_cast<DWORD>(&shadowManager->maxShadows) : 0;
}

//-----------------------------------------------------------------------------

DWORD MWBridge::GetShadowFovAddr() {
    assert(m_loaded);

    eShadowFOV = 0;
    auto shadowManager = worldController()->shadowManager;
    if (shadowManager && shadowManager->sgShadowCamera) {
        eShadowFOV = reinterpret_cast<DWORD>(&shadowManager->sgShadowCamera->viewFrustum);
    }
    return eShadowFOV;
}

//-----------------------------------------------------------------------------

DWORD MWBridge::GetCrosshair2() {
    assert(m_loaded);
    // Bit 0 of the cursor node's flags is the app-cull flag.
    return reinterpret_cast<DWORD>(&worldController()->nodeCursor->flags);
}

//-----------------------------------------------------------------------------

void MWBridge::SetCrosshairEnabled(bool enabled) {
    assert(m_loaded);
    auto* nodeCull = reinterpret_cast<BYTE*>(GetCrosshair2());
    auto* cursorOff = reinterpret_cast<BYTE*>(&worldController()->cursorOff);

    if (enabled) {
        *nodeCull &= 0xfe;
        *cursorOff &= 0xfe;
    } else {
        *nodeCull |= 0x01;
        *cursorOff |= 0x01;
    }
}

//-----------------------------------------------------------------------------

void MWBridge::ToggleCrosshair() {
    assert(m_loaded);
    auto* nodeCull = reinterpret_cast<BYTE*>(GetCrosshair2());
    auto* cursorOff = reinterpret_cast<BYTE*>(&worldController()->cursorOff);

    *nodeCull ^= 0x01;
    *cursorOff ^= 0x01;
}

//-----------------------------------------------------------------------------

bool MWBridge::IsExterior() {
    assert(m_loaded);
    auto dh = dataHandler();
    return dh ? (dh->currentInteriorCell == nullptr) : false;
}

//-----------------------------------------------------------------------------

bool MWBridge::IsMenu() {
    assert(m_loaded);
    return worldController()->flagMenuMode;
}

//-----------------------------------------------------------------------------

bool MWBridge::IsCombat() {
    assert(m_loaded);
    // Kept as the original bit test rather than a comparison against
    // MusicSituation::Combat, so no unlisted enum value can change behaviour.
    return (static_cast<unsigned int>(worldController()->musicSituation) & 1) != 0;
}

//-----------------------------------------------------------------------------

bool MWBridge::IsCrosshair() {
    assert(m_loaded);
    // Read as a byte rather than through the bool: the writers above preserve
    // bits 1-7, so only bit 0 is the crosshair state.
    return (*reinterpret_cast<const BYTE*>(&worldController()->cursorOff) & 1) == 0;
}

//-----------------------------------------------------------------------------

DWORD MWBridge::GetNextTrack() {
    assert(m_loaded);
    return reinterpret_cast<DWORD>(&worldController()->audioController->musicFlags);
}

//-----------------------------------------------------------------------------

DWORD MWBridge::GetMusicVol() {
    assert(m_loaded);
    return reinterpret_cast<DWORD>(&worldController()->audioController->volumeMusic);
}

//-----------------------------------------------------------------------------

void MWBridge::SkipToNextTrack() {
    assert(m_loaded);
    // Clearing the Playing bit makes the engine's music update advance a track.
    auto* musicFlags = reinterpret_cast<BYTE*>(GetNextTrack());
    *musicFlags &= ~BYTE(TES3::MusicFlag::Playing);
}

//-----------------------------------------------------------------------------

void MWBridge::DisableMusic() {
    assert(m_loaded);
    auto audioController = worldController()->audioController;

    audioController->volumeMusic = 0.01f;
    audioController->volumeNextTrack = 0.01f;

    const auto setMusicVolume = reinterpret_cast<void(__thiscall*)(TES3::AudioController*, float)>(
        TES3::Address::AudioController_setMusicVolume);
    setMusicVolume(audioController, 0.01f);
}

//-----------------------------------------------------------------------------

DWORD MWBridge::GetCurrentWeather() {
    assert(m_loaded);
    auto weather = weatherController()->currentWeather;
    if (weather == nullptr) {
        return 0;
    }
    return static_cast<DWORD>(weather->index);
}

//-----------------------------------------------------------------------------

DWORD MWBridge::GetNextWeather() {
    assert(m_loaded);
    auto weather = weatherController()->nextWeather;
    if (weather == nullptr) {
        return GetCurrentWeather();
    }
    return static_cast<DWORD>(weather->index);
}

//-----------------------------------------------------------------------------

float MWBridge::GetWeatherRatio() {
    assert(m_loaded);
    return weatherController()->transitionScalar;
}

//-----------------------------------------------------------------------------

const RGBVECTOR* MWBridge::getCurrentWeatherSkyCol() {
    assert(m_loaded);
    return reinterpret_cast<const RGBVECTOR*>(&weatherController()->currentSkyColor);
}

//-----------------------------------------------------------------------------

const RGBVECTOR* MWBridge::getCurrentWeatherFogCol() {
    assert(m_loaded);
    return reinterpret_cast<const RGBVECTOR*>(&weatherController()->currentFogColor);
}

//-----------------------------------------------------------------------------

DWORD MWBridge::getScenegraphFogCol() {
    return *reinterpret_cast<const DWORD*>(dataHandler()->sgFogProperty->color);
}

//-----------------------------------------------------------------------------

void MWBridge::setScenegraphFogCol(DWORD c) {
    *reinterpret_cast<DWORD*>(dataHandler()->sgFogProperty->color) = c;
}

//-----------------------------------------------------------------------------

float MWBridge::getScenegraphFogDensity() {
    return dataHandler()->sgFogProperty->density;
}

//-----------------------------------------------------------------------------

float* MWBridge::GetWindVector() {
    assert(m_loaded);
    return reinterpret_cast<float*>(&weatherController()->windVelocityCurrWeather);
}

//-----------------------------------------------------------------------------

DWORD MWBridge::GetWthrStruct(int wthr) {
    assert(m_loaded);
    if (wthr >= 0 && wthr < TES3::WeatherController::MAX_WEATHER_COUNT) {
        return reinterpret_cast<DWORD>(weatherController()->arrayWeathers[wthr]);
    }
    return 0;
}

//-----------------------------------------------------------------------------

int MWBridge::GetWthrString(int wthr, int offset, char str[]) {
    assert(m_loaded);
    DWORD addr = GetWthrStruct(wthr);
    int i = 0;

    if (addr != 0) {
        const char* src = reinterpret_cast<const char*>(addr + offset);
        while ((str[i] = *src) != 0) {
            ++src;
            ++i;
        }
    }
    str[i++] = 0;
    return i;
}

//-----------------------------------------------------------------------------

void MWBridge::SetWthrString(int wthr, int offset, char str[]) {
    assert(m_loaded);
    DWORD addr = GetWthrStruct(wthr);
    int i = 0;

    if (addr != 0) {
        char c;
        char* dst = reinterpret_cast<char*>(addr + offset);
        do {
            c = str[i++];
            *dst++ = c;
        } while (c != 0);
    }
}

//-----------------------------------------------------------------------------

bool MWBridge::CellHasWater() {
    assert(m_loaded);
    auto cell = getInteriorCell();
    if (cell != nullptr) {
        return cell->getIsInterior() && cell->getHasWater();
    }
    return true;
}

//-----------------------------------------------------------------------------

bool MWBridge::IsUnderwater(float eyeZ) {
    assert(m_loaded);
    return (CellHasWater() && (eyeZ < WaterLevel() - 1.0f));
}

//-----------------------------------------------------------------------------

bool MWBridge::WaterReflects(float eyeZ) {
    assert(m_loaded);
    return (CellHasWater() && (eyeZ > WaterLevel() - 1.0f));
}

//-----------------------------------------------------------------------------

// simulationTime - Total real time elapsed this session, does not advance in menus
float MWBridge::simulationTime() {
    assert(m_loaded);
    return globalAt<float>(TES3::Address::simulationTimestamp);
}

//-----------------------------------------------------------------------------

// frameTime - Duration of last frame in seconds
float MWBridge::frameTime() {
    assert(m_loaded);
    return worldController()->deltaTime;
}

//-----------------------------------------------------------------------------

// getMouseSensitivityYX() - Returns address of mouse sensitivity struct
// data is float[2], Y sensitivity component is first
float* MWBridge::getMouseSensitivityYX() {
    return &worldController()->mouseSensitivity;
}

//-----------------------------------------------------------------------------

float MWBridge::GetViewDistance() {
    assert(m_loaded);
    return worldController()->worldCamera.cameraData.farPlaneDistance;
}

//-----------------------------------------------------------------------------

void MWBridge::SetViewDistance(float dist) {
    assert(m_loaded);
    auto wc = worldController();

    wc->worldCamera.cameraData.farPlaneDistance = dist;
    wc->shadowCamera.cameraData.farPlaneDistance = dist;
    wc->worldCamera.cameraData.camera->viewFrustum.farPlane = dist;
    wc->shadowCamera.cameraData.camera->viewFrustum.farPlane = dist;
    TES3::Game::get()->renderDistance = dist;
}

//-----------------------------------------------------------------------------

float MWBridge::GetAIDistance() {
    assert(m_loaded);
    return worldController()->aiDistanceScale;
}

//-----------------------------------------------------------------------------

void MWBridge::SetAIDistance(float dist) {
    assert(m_loaded);
    worldController()->aiDistanceScale = dist;
}

//-----------------------------------------------------------------------------

void MWBridge::SetFOV(float screenFOV) {
    assert(m_loaded);

    auto wc = worldController();
    NI::Frustum& worldFrustum = wc->worldCamera.cameraData.camera->viewFrustum;

    // Recalculate FOV values
    float fovtan = std::tan(screenFOV * D3DX_PI / 360.0f);

    if (std::fabs(worldFrustum.left + fovtan) > 0.001f) {
        float aspect = worldFrustum.right / worldFrustum.top;
        float fovtanaspect = fovtan / aspect;

        writeFrustumFOV(worldFrustum, fovtan, fovtanaspect);
        // The first person arms camera, which MGE historically called eSkyFOV.
        writeFrustumFOV(wc->armCamera.cameraData.camera->viewFrustum, fovtan, fovtanaspect);

        if (!eShadowFOV) {
            GetShadowFovAddr();
        }
        if (eShadowFOV) {
            writeFrustumFOV(*reinterpret_cast<NI::Frustum*>(eShadowFOV), fovtan, fovtanaspect);
        }
    }
}

//-----------------------------------------------------------------------------

void MWBridge::GetSunDir(float& x, float& y, float& z) {
    // Note: Previous method caused significant jitter with moving view
    // This now returns the exact offset which was in the same scenegraph node
    assert(m_loaded);
    const NI::Point3& sunDir = weatherController()->sgSunVis->localTranslate;
    x = sunDir.x;
    y = sunDir.y;
    z = sunDir.z;
}

//-----------------------------------------------------------------------------

BYTE MWBridge::GetSunVis() {
    assert(m_loaded);
    return weatherController()->shTriSunBase->modelData->color[0].a;
}

//-----------------------------------------------------------------------------

// Both of the engine's derivations start from an integer radius, so the right
// inversion comes back as one, to within the rounding of a float that went
// through a division and a square root. The allowance is capped: left to grow
// with the radius it would pass any wrong reading that came out large enough.
static bool isIntegerRadius(float radius) {
    const float allowance = std::min(0.01f + 1e-5f * radius, 0.1f);
    return std::fabs(radius - std::round(radius)) <= allowance;
}

// Inverts the two ways Morrowind derives point-light attenuation from a radius.
// The coefficients reach D3D unchanged, so each inversion is exact up to
// rounding, but the coefficients do not always say which one applies; see the
// first branch. A radius that cannot be recovered is returned as 0 and does
// not fade.
float MWBridge::pointLightRadius(float constant, float linear, float quadratic) {
    using namespace TES3::Address;
    float radius = 0.0f;

    // EntityLight::createLightOnReference copies the INI constant as-is, which is
    // what identifies its lights. OutQuadInLin enables the linear term indoors
    // and the quadratic term outdoors, whatever their own flags say. Which of
    // the two the light was created in is not tested here, and need not be: the
    // term the engine skipped was left at zero, so the coefficient guards below
    // reject that branch on their own.
    const auto flags = globalAt<unsigned int>(LightAttenuation_Flags);
    const bool quadraticInLinear = globalAt<BYTE>(LightAttenuation_QuadraticInLinear) != 0;
    const float iniConstant = (flags & 1) ? globalAt<float>(LightAttenuation_ConstantValue) : 0.0f;

    const int quadraticMethod = globalAt<int>(LightAttenuation_QuadraticMethod);
    const float quadraticValue = globalAt<float>(LightAttenuation_QuadraticValue);
    const int linearMethod = globalAt<int>(LightAttenuation_LinearMethod);
    const float linearValue = globalAt<float>(LightAttenuation_LinearValue);

    if (constant == iniConstant
     && quadratic > 0 && quadraticValue > 0 && quadraticMethod != 0
     && ((flags & 4) || quadraticInLinear)) {
        // Method 1 divides by the radius before the multiplier is applied.
        radius = quadraticMethod == 1
            ? quadraticValue / quadratic
            : std::sqrt(quadraticValue / quadratic)
                / globalAt<float>(LightAttenuation_QuadraticRadiusMultiplier);

        // With no INI constant, a body light from setLightEffectFalloff has this
        // same shape: constant 0, linear 0, quadratic positive. That is every
        // spell and projectile light and every light an actor carries, torches
        // included. Read as a record light, its radius comes out
        // sqrt(quadraticValue / 10) times too long under method 2, and
        // quadraticValue * r / 10 times under method 1, which puts a torch out
        // at the one-cell cap: attached to every node in reach, and first in
        // each node's list, which the engine sorts by distance less radius.
        // Usually only one reading comes back as an integer. When both do, as
        // they always will for a quadraticValue of 10, the record reading
        // stands: the longer one costs slots but changes no pixel, while a
        // record light read short would visibly shrink. A record light is
        // never taken for a body light, since its own reading is an integer.
        if (constant == 0 && linear == 0) {
            const float bodyRadius = std::sqrt(10.0f / quadratic);
            if (isIntegerRadius(bodyRadius) && !isIntegerRadius(radius)) {
                radius = bodyRadius;
            }
        }
    } else if (constant == iniConstant
     && linear > 0 && linearValue > 0 && linearMethod != 0
     && ((flags & 2) || quadraticInLinear)) {
        radius = (linearMethod == 2 ? std::sqrt(linearValue / linear) : linearValue / linear)
            / globalAt<float>(LightAttenuation_LinearRadiusMultiplier);
    } else if (constant == 0 && linear == 0 && quadratic > 0) {
        // MobileObject::setLightEffectFalloff: spell and projectile lights, and
        // through EntityLight::setupInternalLight every light an actor carries.
        // Reached directly whenever the INI has a constant or no quadratic term,
        // which covers the stock INI and the one MGE recommends.
        radius = std::sqrt(10.0f / quadratic);
    }

    return radius > 0 ? radius : 0.0f;
}

//-----------------------------------------------------------------------------

// setSunriseSunset - Sets sunrise and sunset time and duration
void MWBridge::setSunriseSunset(float rise_time, float rise_dur, float set_time, float set_dur) {
    auto wthr = weatherController();
    wthr->sunriseHour = rise_time;
    wthr->sunriseDuration = rise_dur;
    wthr->sunsetHour = set_time;
    wthr->sunsetDuration = set_dur;
}

//-----------------------------------------------------------------------------

// getInteriorCell - The interior cell the player is in, or null in exteriors
TES3::Cell* MWBridge::getInteriorCell() {
    auto dh = dataHandler();
    return dh ? dh->currentInteriorCell : nullptr;
}

//-----------------------------------------------------------------------------

DWORD MWBridge::IntCurCellAddr() {
    assert(m_loaded);
    return reinterpret_cast<DWORD>(getInteriorCell());
}

//-----------------------------------------------------------------------------

const char* MWBridge::getInteriorName() {
    assert(m_loaded);
    auto cell = getInteriorCell();
    return cell ? cell->name : nullptr;
}

//-----------------------------------------------------------------------------

bool MWBridge::IntLikeExterior(bool whenCellUnknown) {
    assert(m_loaded);
    auto cell = getInteriorCell();
    if (cell == nullptr) {
        return whenCellUnknown;
    }
    return cell->getBehavesAsExterior();
}

//-----------------------------------------------------------------------------

bool MWBridge::IntIllegSleep() {
    assert(m_loaded);
    auto cell = getInteriorCell();
    if (cell != nullptr) {
        return cell->getIsInterior() && cell->getSleepIsIllegal();
    }
    return false;
}

//-----------------------------------------------------------------------------

bool MWBridge::IntHasWater() {
    assert(m_loaded);
    auto cell = getInteriorCell();
    if (cell != nullptr) {
        return cell->getIsInterior() && cell->getHasWater() && !cell->getBehavesAsExterior();
    }
    return false;
}

//-----------------------------------------------------------------------------

float MWBridge::WaterLevel() {
    assert(m_loaded);
    auto cell = getInteriorCell();
    if (cell != nullptr && cell->getIsInterior() && cell->getHasWater() && !cell->getBehavesAsExterior()) {
        return cell->waterLevelOrRegion.waterLevel;
    }
    return 0.0f;
}

//-----------------------------------------------------------------------------

void MWBridge::HaggleMore(DWORD num) {
    assert(m_loaded);
    const auto updateHaggle = reinterpret_cast<bool(__cdecl*)()>(TES3::Address::ui_MenuBarter_updateHaggle);
    auto& haggleAmount = globalAt<int>(TES3::Address::ui_MenuBarter_haggleAmount);

    if (num != 0) {
        int d = haggleAmount;
        if (d <= 0) {
            d -= num;
        } else {
            d += num;
        }

        haggleAmount = d;
    }
    updateHaggle();
}

//-----------------------------------------------------------------------------

void MWBridge::HaggleLess(DWORD num) {
    assert(m_loaded);
    const auto updateHaggle = reinterpret_cast<bool(__cdecl*)()>(TES3::Address::ui_MenuBarter_updateHaggle);
    auto& haggleAmount = globalAt<int>(TES3::Address::ui_MenuBarter_haggleAmount);

    if (num != 0) {
        int d = haggleAmount;
        if (d <= 0) {
            d += num;
        } else {
            d -= num;
        }

        haggleAmount = d;
    }
    updateHaggle();
}

//-----------------------------------------------------------------------------

const BYTE* MWBridge::getInteriorAmb() {
    assert(m_loaded);
    auto cell = getInteriorCell();
    if (cell != nullptr) {
        return reinterpret_cast<const BYTE*>(&cell->variantData.interior.ambientColor);
    }
    return nullptr;
}

//-----------------------------------------------------------------------------

const BYTE* MWBridge::getInteriorSun() {
    assert(m_loaded);
    auto cell = getInteriorCell();
    if (cell != nullptr) {
        return reinterpret_cast<const BYTE*>(&cell->variantData.interior.sunColor);
    }
    return nullptr;
}

//-----------------------------------------------------------------------------

const BYTE* MWBridge::getInteriorFog() {
    assert(m_loaded);
    auto cell = getInteriorCell();
    if (cell != nullptr) {
        return reinterpret_cast<const BYTE*>(&cell->variantData.interior.fogColor);
    }
    return nullptr;
}

//-----------------------------------------------------------------------------

float MWBridge::getInteriorFogDens() {
    assert(m_loaded);
    auto cell = getInteriorCell();
    if (cell != nullptr) {
        return cell->variantData.interior.fogDensity;
    }
    return 0;
}

//-----------------------------------------------------------------------------

DWORD MWBridge::PlayerPositionPointer() {
    auto player = getPlayerMobile();
    if (player == nullptr || player->reference == nullptr) {
        return 0;
    }
    return reinterpret_cast<DWORD>(&player->reference->position);
}

//-----------------------------------------------------------------------------

float MWBridge::PlayerPositionX() {
    DWORD addr = PlayerPositionPointer();
    if (addr != 0) {
        return reinterpret_cast<const NI::Point3*>(addr)->x;
    }
    return 0;
}

//-----------------------------------------------------------------------------

float MWBridge::PlayerPositionY() {
    DWORD addr = PlayerPositionPointer();
    if (addr != 0) {
        return reinterpret_cast<const NI::Point3*>(addr)->y;
    }
    return 0;
}

//-----------------------------------------------------------------------------

float MWBridge::PlayerPositionZ() {
    DWORD addr = PlayerPositionPointer();
    if (addr != 0) {
        return reinterpret_cast<const NI::Point3*>(addr)->z;
    }
    return 0;
}

//-----------------------------------------------------------------------------

bool MWBridge::tryGetPlayerPosition(float outPosition[3]) {
    if (!m_loaded) {
        return false;
    }

    // getPlayerCell() dereferences the environment pointer unchecked; validate it here
    // instead, so a menu-frame caller never faults before the world exists.
    auto dh = dataHandler();
    if (dh == nullptr || dh->currentCell == nullptr) {
        return false;
    }

    const DWORD addr = PlayerPositionPointer();
    if (addr == 0) {
        return false;
    }

    const NI::Point3& position = *reinterpret_cast<const NI::Point3*>(addr);
    if (!std::isfinite(position.x) || !std::isfinite(position.y) || !std::isfinite(position.z)) {
        return false;
    }

    outPosition[0] = position.x;
    outPosition[1] = position.y;
    outPosition[2] = position.z;
    return true;
}

//-----------------------------------------------------------------------------

// PlayerHeight - The player's collision height above the ground, in world units.
float MWBridge::PlayerHeight() {
    auto player = getPlayerMobile();
    return (player != nullptr) ? player->height : 0.0f;
}

//-----------------------------------------------------------------------------

// getPlayerMobile - Gets main game object holding the player state, or null if
// the world is not up yet. Every hop is checked: the whole chain is unresolvable
// during load screens and the main menu, and callers reach here from Present.
TES3::MobilePlayer* MWBridge::getPlayerMobile() {
    auto wc = worldController();
    if (wc == nullptr) {
        return nullptr;
    }

    auto mobManager = wc->mobManager;
    if (mobManager == nullptr) {
        return nullptr;
    }

    auto processManager = mobManager->processManager;
    if (processManager == nullptr) {
        return nullptr;
    }

    return processManager->mobilePlayer;
}

//-----------------------------------------------------------------------------

DWORD MWBridge::getPlayerMACP() {
    return reinterpret_cast<DWORD>(getPlayerMobile());
}

//-----------------------------------------------------------------------------

D3DXVECTOR3* MWBridge::PCam3Offset() {
    auto player = getPlayerMobile();
    if (player == nullptr || player->animationController == nullptr) {
        return nullptr;
    }

    // Camera control structure
    return reinterpret_cast<D3DXVECTOR3*>(&player->animationController->cameraOffset);
}

//-----------------------------------------------------------------------------

bool MWBridge::is3rdPerson() {
    auto player = getPlayerMobile();
    if (player == nullptr || player->animationController == nullptr) {
        return false;
    }

    // Camera control structure
    return player->animationController->is3rdPerson;
}

//-----------------------------------------------------------------------------

DWORD MWBridge::getPlayerTarget() {
    return reinterpret_cast<DWORD>(TES3::Game::get()->playerTarget);
}

//-----------------------------------------------------------------------------

// isPlayerCasting - Tests is the player is currently casting
bool MWBridge::isPlayerCasting() {
    auto player = getPlayerMobile();
    if (player == nullptr) {
        return false;
    }

    // Check animation state machine for casting
    return player->actionData.animStateAttack == TES3::AttackAnimationState::Casting;
}

//-----------------------------------------------------------------------------

// isPlayerAimingWeapon - Tests if the player is in the drawing stage of attacking with a ranged weapon
bool MWBridge::isPlayerAimingWeapon() {
    auto player = getPlayerMobile();
    if (player == nullptr) {
        return false;
    }

    // Check animation state machine for weapon pullback
    if (player->actionData.animStateAttack != TES3::AttackAnimationState::SwingUp) {
        return false;
    }

    // Check weapon type (bow, crossbow, thrown)
    auto readiedWeapon = player->readiedWeapon;
    if (readiedWeapon == nullptr) {
        return false;
    }

    auto weapon = reinterpret_cast<const TES3::Weapon*>(readiedWeapon->object);
    return weapon->weaponType >= TES3::WeaponType::Bow;
}

//-----------------------------------------------------------------------------

// getPlayerCell - Gets pointer to player cell
void* MWBridge::getPlayerCell() {
    return dataHandler()->currentCell;
}

//-----------------------------------------------------------------------------

// toggleRipples - Turns off ripple generation from all sources
void MWBridge::toggleRipples(BOOL enabled) {
    auto* site = reinterpret_cast<DWORD*>(TES3::Address::patch_ripples);
    DWORD code = *site;
    if ((enabled && code == 0x33504D8B) || (!enabled && code == 0x3390C931)) {
        return;
    }
    code = enabled ? 0x33504D8B : 0x3390C931;

    MWPatches::VirtualMemWriteAccessor vw(site, 4);
    *site = code;
}

//-----------------------------------------------------------------------------

// markWaterNode
// Edits the water material to set (normally unused) specular power to a recognizable value
void MWBridge::markWaterNode(float k) {
    // Get water node
    NI::Node* waterPlane = dataHandler()->waterController->waterPlane;

    // Look for NiMaterialProperty in property list (skipping first property)
    NI::MaterialProperty* material = nullptr;
    for (auto link = waterPlane->propertyNode.next; link; link = link->next) {
        // The list can carry an empty link; the original walk did not check this.
        if (link->data == nullptr) {
            continue;
        }
        if (reinterpret_cast<uintptr_t>(link->data->vTable) == TES3::Address::vtable_NiMaterialProperty) {
            material = reinterpret_cast<NI::MaterialProperty*>(link->data);
            break;
        }
    }

    // Write to specular power member
    if (material) {
        material->shininess = k;
    }
}

//-----------------------------------------------------------------------------

// markMoonNodes
// Edits the material for both moons to set (normally unused) specular power to a recognizable value
void MWBridge::markMoonNodes(float k) {
    auto wthr = weatherController();

    for (TES3::Moon* moon : { wthr->moonMasser, wthr->moonSecunda }) {
        NI::TriShape* shadow = moon->sgTriMoonShadow;

        // Look for NiMaterialProperty in first property slot
        NI::Property* property = shadow->propertyNode.data;

        // Write to specular power member
        if (property && reinterpret_cast<uintptr_t>(property->vTable) == TES3::Address::vtable_NiMaterialProperty) {
            reinterpret_cast<NI::MaterialProperty*>(property)->shininess = k;
        }
    }
}

//-----------------------------------------------------------------------------

// isIntroDone - Tests if both intro movies are finished, and main menu is about to display
bool MWBridge::isIntroDone() {
    return globalAt<BYTE>(TES3::Address::isIntroDone) != 0;
}

//-----------------------------------------------------------------------------

// isLoadingBar - Tests if a loading bar is shown
bool MWBridge::isLoadingBar() {
    return globalAt<BYTE>(TES3::Address::ui_MenuLoading_active) != 0;
}

//-----------------------------------------------------------------------------

// showLoadingBar - Displays the loading progress bar with text and fill amount
void MWBridge::showLoadingBar(const char* text, float amount) {
    const auto showLoadingMenu = reinterpret_cast<void(__cdecl*)(const char*, float)>(
        TES3::Address::ui_showLoadingMenu);
    showLoadingMenu(text, amount);

    renderLoadingFrame();
}

//-----------------------------------------------------------------------------

// getLoadingBarLabel - Copies the native loading-bar label when the menu exists
bool MWBridge::getLoadingBarLabel(std::string& out) {
    const auto findMenu = reinterpret_cast<void*(__cdecl*)(short)>(TES3::Address::ui_findMenu);
    void* loadingMenu = findMenu(globalAt<const short>(TES3::Address::ui_id_MenuLoading));
    if (!loadingMenu) {
        return false;
    }

    const auto findChildElement = reinterpret_cast<void*(__thiscall*)(void*, short)>(
        TES3::Address::ui_findChildElement);
    void* loadingLabel = findChildElement(loadingMenu, globalAt<const short>(TES3::Address::ui_id_MenuLoading_label));
    if (!loadingLabel) {
        return false;
    }

    const auto getText = reinterpret_cast<const char*(__thiscall*)(void*)>(TES3::Address::ui_getText);
    const char* text = getText(loadingLabel);
    if (!text) {
        return false;
    }
    out = text;
    return true;
}

//-----------------------------------------------------------------------------

// setLoadingBarLabel - Updates only the native loading-bar label
void MWBridge::setLoadingBarLabel(const char* text) {
    const auto updateLoadingLabel = reinterpret_cast<void(__cdecl*)(const char*)>(
        TES3::Address::ui_updateLoadingLabel);
    updateLoadingLabel(text);
}

//-----------------------------------------------------------------------------

// updateLoadingScreen - Updates the native loading screen at its own cadence
void MWBridge::updateLoadingScreen(float percent) {
    const auto loadingScreenUpdate = reinterpret_cast<void(__cdecl*)(float)>(
        TES3::Address::ui_updateLoadingMenu);
    loadingScreenUpdate(percent);
}

//-----------------------------------------------------------------------------

// renderLoadingFrame - Renders and presents one native loading frame
void MWBridge::renderLoadingFrame() {
    const auto renderNextFrame = reinterpret_cast<void(__thiscall*)(TES3::Game*, int)>(
        TES3::Address::Game_renderNextFrame);
    renderNextFrame(TES3::Game::get(), 0);
}

//-----------------------------------------------------------------------------

// destroyLoadingBar - Destroys loading bar menu element
void MWBridge::destroyLoadingBar() {
    const auto destroyLoadingMenu = reinterpret_cast<void(__cdecl*)()>(
        TES3::Address::ui_destroyLoadingMenu);
    destroyLoadingMenu();
}

//-----------------------------------------------------------------------------

// getWindowHandle - Gets window handle of windowed mode frame
HWND MWBridge::getWindowHandle() {
    return static_cast<HWND>(worldController()->Win32_hWndParent);
}

//-----------------------------------------------------------------------------

// getGameOptionsStruct - Gets TES3Game struct
void* MWBridge::getGameOptionsStruct() {
    return TES3::Game::get();
}

//-----------------------------------------------------------------------------

// getUIScale - Get the scaling of Morrowind's UI system.
//              MWBridge is not required to be loaded for this function.
float MWBridge::getUIScale() {
    auto wc = worldController();

    // Read renderer and viewport sizes
    int w = wc->renderer->currentRenderTarget->width;
    int vw = wc->viewWidth;

    // Calculate scale factor
    return float(w) / float(vw);
}

//-----------------------------------------------------------------------------

// setUIScale - Configures the scaling of Morrowind's UI system.
//              MWBridge is not required to be loaded for this function.
void MWBridge::setUIScale(float scale) {
    auto wc = worldController();

    // Read renderer width and height
    const NI::DX8RenderTarget* backBuffer = wc->renderer->currentRenderTarget;
    // Calculate a smaller viewport that will be scaled up by Morrowind
    int w = (int)(backBuffer->width / scale);
    int h = (int)(backBuffer->height / scale);
    // Write new viewport size
    wc->viewWidth = w;
    wc->viewHeight = h;

    // Call UI configuration method to update scaling
    const auto ui_configureUIScale = reinterpret_cast<void(__thiscall*)(TES3::WorldController*, DWORD)>(
        TES3::Address::WorldController_configUIScaling);

    ui_configureUIScale(wc, w);

    // Call UI configuration method to update mouse bounds
    const auto ui_configureUIMouseArea = reinterpret_cast<void(__thiscall*)(TES3::MouseController*, int, int, int, int)>(
        TES3::Address::CursorController_setCursorBounds);

    int w_half = (w + 1) / 2, h_half = (h + 1) / 2;
    ui_configureUIMouseArea(wc->mouseController, -w_half, -h_half, w_half, h_half);

    // Patch raycast system to use UI viewport size instead of D3D viewport size
    void* addr = reinterpret_cast<void*>(TES3::Address::patch_uiScaleRaycast);
    const BYTE patch[] = {
        0xa1, 0xdc, 0x67, 0x7c, 0x00,       // mov eax, WorldController::get()
        0x8b, 0x78, 0x78,                   // mov edi, [eax+0x78]
        0x8b, 0x40, 0x7c,                   // mov eax, [eax+0x7c]
        0x90, 0x90, 0x90                    // nops
    };

    MWPatches::VirtualMemWriteAccessor vw(addr, sizeof(patch));
    memcpy(addr, patch, sizeof(patch));
}

//-----------------------------------------------------------------------------

// getGMSTPointer - Gets a pointer directly to the data of a GMST (of any type)
void* MWBridge::getGMSTPointer(DWORD id) {
    TES3::GameSetting* setting = dataHandler()->nonDynamicData->GMSTs[id];
    return &setting->value;
}

//-----------------------------------------------------------------------------

// getPlayerName - Returns the player's name, or null if not loaded
const char* MWBridge::getPlayerName() {
    auto player = getPlayerMobile();
    if (player == nullptr) {
        return nullptr;
    }

    // Get name from base NPC
    return player->npcInstance->baseNPC->name;
}

//-----------------------------------------------------------------------------

// getGameHour - Returns the value of the script global GameHour
float MWBridge::getGameHour() {
    return worldController()->gvarGameHour->value;
}

//-----------------------------------------------------------------------------

// getDaysPassed - Returns the value of the script global DaysPassed
int MWBridge::getDaysPassed() {
    return int(worldController()->gvarDaysPassed->value);
}

//-----------------------------------------------------------------------------

// getFrameBeginMillis - Returns timer millis measured at start of frame
int MWBridge::getFrameBeginMillis() {
    return worldController()->systemTimeMillis;
}

//-----------------------------------------------------------------------------

// getGlobalVar - Get global variable record
void* MWBridge::getGlobalVar(const char* id) {
    const auto findGlobalVariable = reinterpret_cast<TES3::GlobalVariable*(__thiscall*)(TES3::NonDynamicData*, const char*)>(
        TES3::Address::NonDynamicData_findGlobalVariable);

    return findGlobalVariable(dataHandler()->nonDynamicData, id);
}

float MWBridge::getGlobalVarValue(const void* globalVar) {
    return static_cast<const TES3::GlobalVariable*>(globalVar)->value;
}

// getDialogue - Get dialogue record
void* MWBridge::getDialogue(const char* id) {
    const auto findDialogue = reinterpret_cast<TES3::Dialogue*(__thiscall*)(TES3::NonDynamicData*, const char*)>(
        TES3::Address::NonDynamicData_findDialogue);

    return findDialogue(dataHandler()->nonDynamicData, id);
}

int MWBridge::getJournalIndex(const void* dialogue) {
    return static_cast<const TES3::Dialogue*>(dialogue)->journalIndex;
}

// findFirstReferenceById - Find first reference to object
void* MWBridge::findFirstReferenceById(const char* id) {
    const auto findFirstReferenceByObjectId = reinterpret_cast<TES3::Reference*(__thiscall*)(TES3::NonDynamicData*, const char*)>(
        TES3::Address::NonDynamicData_findFirstReferenceByObjectId);

    return findFirstReferenceByObjectId(dataHandler()->nonDynamicData, id);
}

unsigned int MWBridge::getRecordFlags(const void* record) {
    return static_cast<const TES3::BaseObject*>(record)->objectFlags;
}
