#include "startupgen.h"

#include "configuration.h"
#include "distantland.h"
#include "support/log.h"
#include "support/winheader.h"

#include <cstdio>
#include <cstring>

namespace {
    enum class LaunchState {
        NotStarted,
        Started,
        Skipped,
        FailedToStart,
    };

    LaunchState launchState = LaunchState::NotStarted;

    bool fileExists(const char* path) {
        DWORD attrs = GetFileAttributesA(path);
        return attrs != INVALID_FILE_ATTRIBUTES && !(attrs & FILE_ATTRIBUTE_DIRECTORY);
    }

    bool directoryFromProcess(char* root, size_t rootSize) {
        DWORD length = GetModuleFileNameA(NULL, root, static_cast<DWORD>(rootSize));
        if (length == 0 || length >= rootSize) {
            return false;
        }

        for (char* p = root + std::strlen(root); p != root; --p) {
            if (*p == '\\' || *p == '/') {
                *p = 0;
                return true;
            }
        }
        return false;
    }

    bool pathJoin(char* output, size_t outputSize, const char* left, const char* right) {
        int written = std::snprintf(output, outputSize, "%s\\%s", left, right);
        return written > 0 && static_cast<size_t>(written) < outputSize;
    }

}

namespace StartupGeneration {
    void launchEarlyHost(bool isMW) {
        if (launchState != LaunchState::NotStarted) {
            return;
        }

        if (!isMW) {
            launchState = LaunchState::Skipped;
            return;
        }
        if (Configuration.MGEFlags & MGE_DISABLED) {
            LOG::logline("Startup generation skipped: MGE is disabled");
            launchState = LaunchState::Skipped;
            return;
        }
        if (Configuration.OnlyProxyD3D8To9) {
            LOG::logline("Startup generation skipped: D3D8-to-D3D9 proxy-only mode is enabled");
            launchState = LaunchState::Skipped;
            return;
        }
        if (!(Configuration.MGEFlags & USE_DISTANT_LAND)) {
            LOG::logline("Startup generation skipped: distant land is disabled");
            launchState = LaunchState::Skipped;
            return;
        }

        char root[MAX_PATH];
        if (!directoryFromProcess(root, sizeof(root))) {
            LOG::logline("Startup generation skipped: failed to resolve Morrowind root");
            launchState = LaunchState::FailedToStart;
            return;
        }

        char exePath[MAX_PATH];
        if (!pathJoin(exePath, sizeof(exePath), root, "mgeHost64.exe") || !fileExists(exePath)) {
            LOG::logline("Startup generation skipped: mgeHost64.exe is missing");
            launchState = LaunchState::Skipped;
            return;
        }

        if (!DistantLand::ipcClient.launchServer(exePath, root)) {
            launchState = LaunchState::FailedToStart;
            return;
        }

        launchState = LaunchState::Started;
        LOG::logline("Startup generation launched persistent 64-bit IPC host");
    }
}
