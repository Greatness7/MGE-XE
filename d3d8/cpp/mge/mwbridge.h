#pragma once

#include "bc7format.h"
#include <string>

// Forward declared so the engine layout headers stay out of every translation
// unit that only needs the bridge's public interface.
namespace TES3 {
    struct Cell;
    struct MobilePlayer;
}


//-----------------------------------------------------------------------------

class MWBridge {
public:
    ~MWBridge();

    // Singleton access
    static MWBridge* get();

    // Connect to Morrowind memory
    void Load();

    // Used to determine whether we have connected to Morrowind's dynamic memory yet
    inline bool IsLoaded();
    bool CanLoad();

    DWORD GetShadowToggleAddr();
    DWORD GetShadowRealAddr();
    DWORD GetShadowFovAddr();
    DWORD GetCrosshair2();
    void SetCrosshairEnabled(bool enabled);
    void ToggleCrosshair();
    bool IsExterior();
    bool IsMenu();
    bool IsCombat();
    bool IsCrosshair();

    DWORD GetNextTrack();
    DWORD GetMusicVol();
    void SkipToNextTrack();
    void DisableMusic();

    DWORD GetCurrentWeather();
    DWORD GetNextWeather();
    float GetWeatherRatio();
    const RGBVECTOR* getCurrentWeatherSkyCol();
    const RGBVECTOR* getCurrentWeatherFogCol();
    DWORD getScenegraphFogCol();
    void setScenegraphFogCol(DWORD c);
    float getScenegraphFogDensity();
    float* GetWindVector();
    DWORD GetWthrStruct(int wthr);
    int GetWthrString(int wthr, int offset, char str[]);
    void SetWthrString(int wthr, int offset, char str[]);
    bool CellHasWater();
    bool IsUnderwater(float eyeZ);
    bool WaterReflects(float eyeZ);
    float simulationTime();
    float frameTime();

    float* getMouseSensitivityYX();
    float GetViewDistance();
    void SetViewDistance(float dist);
    float GetAIDistance();
    void SetAIDistance(float dist);

    void SetFOV(float screenFOV);

    void GetSunDir(float& x, float& y, float& z);
    BYTE GetSunVis();
    void setSunriseSunset(float rise_time, float rise_dur, float set_time, float set_dur);

    DWORD IntCurCellAddr();
    // The editor's "Behaves like exterior" flag, which exteriors also carry. It is what decides
    // whether the current cell has weather, sky and outdoor lighting. `whenCellUnknown` is
    // returned when no cell resolves - between cells, or before the environment exists - so
    // render paths can keep their exterior setup on such a frame while interior queries do not.
    bool IntLikeExterior(bool whenCellUnknown = false);
    bool IntIllegSleep();
    bool IntHasWater();
    float WaterLevel();

    const char* getInteriorName();
    const BYTE* getInteriorAmb();
    const BYTE* getInteriorSun();
    const BYTE* getInteriorFog();
    float getInteriorFogDens();

    DWORD PlayerPositionPointer();
    float PlayerPositionX();
    float PlayerPositionY();
    float PlayerPositionZ();
    // Explicitly optional player position for callers that run outside a rendered frame
    // (menu / load-screen Present ticks). Validates the environment, cell and position
    // pointers, unlike PlayerPositionX/Y/Z which return a valid-looking zero on failure.
    bool tryGetPlayerPosition(float outPosition[3]);
    float PlayerHeight();
    D3DXVECTOR3* PCam3Offset();
    DWORD getPlayerMACP();
    bool is3rdPerson();
    DWORD getPlayerTarget();
    bool isPlayerCasting();
    bool isPlayerAimingWeapon();
    void* getPlayerCell();

    void HaggleMore(DWORD num);
    void HaggleLess(DWORD num);

    void toggleRipples(BOOL enabled);
    void markWaterNode(float k);
    void markMoonNodes(float k);
    bool isIntroDone();
    bool isLoadingBar();
    void showLoadingBar(const char* text, float amount);
    bool getLoadingBarLabel(std::string& out);
    void setLoadingBarLabel(const char* text);
    void updateLoadingScreen(float percent);
    void renderLoadingFrame();

    HWND getWindowHandle();
    void* getGameOptionsStruct();
    void destroyLoadingBar();
    float getUIScale();
    void setUIScale(float scale);

    void* getGMSTPointer(DWORD id);
    const char* getPlayerName();
    float getGameHour();
    int getDaysPassed();
    int getFrameBeginMillis();
    void* getGlobalVar(const char *id);
    float getGlobalVarValue(const void* globalVar);
    void* getDialogue(const char *id);
    int getJournalIndex(const void* dialogue);
    void* findFirstReferenceById(const char *id);
    unsigned int getRecordFlags(const void* record);

    MWBridge();

protected:
    /// The interior cell the player is in, or null in exteriors and before the
    /// environment exists. Backs every interior-only query below.
    TES3::Cell* getInteriorCell();

    /// The player's mobile object. Not null-safe at each hop; see plan section 6.1.
    TES3::MobilePlayer* getPlayerMobile();

    // Latched by Load() once Morrowind's environment first becomes reachable.
    // This is a one-shot latch, not a live probe: Present() keys the one-time
    // engine patching and DistantLand::init() off `!IsLoaded() && CanLoad()`,
    // which only ever fires while the two disagree.
    bool m_loaded;

    // Cached shadow-camera frustum. Resolved lazily because the shadow manager
    // does not exist until shadows are first enabled, and MGE writes FOV into it
    // every time the player FOV changes.
    DWORD eShadowFOV;
};

//-----------------------------------------------------------------------------
// Inline Functions
//-----------------------------------------------------------------------------

inline bool MWBridge::IsLoaded() {
    return m_loaded;
}

//-----------------------------------------------------------------------------
