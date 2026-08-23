#include "dlshare.h"

#include "mge/mgeversion.h"
#include "support/log.h"

bool DistantLandShare::hasCurrentWorldSpace = false;

HANDLE DistantLandShare::beginReadStatics() {
    HANDLE h;

    {
        DistantLoadInstrumentation::ScopedLoadTimer timer("statics.open_version");
        h = CreateFile("Data Files\\distantland\\version", GENERIC_READ, FILE_SHARE_READ, 0, OPEN_EXISTING, 0, 0);
        if (h == INVALID_HANDLE_VALUE) {
            LOG::logline("!! Required distant statics files are missing, regeneration required - distantland/version");
            LOG::flush();
            return INVALID_HANDLE_VALUE;
        }
        BYTE version = 0;
        if (!DistantLoadInstrumentation::ReadExact(h, &version, sizeof(version), "distantland.version")) {
            CloseHandle(h);
            return INVALID_HANDLE_VALUE;
        }
	if (version != MGE_DL_VERSION) {
            LOG::logline("!! Distant statics data is from an old version and needs to be regenerated");
            LOG::flush();
            CloseHandle(h);
            return INVALID_HANDLE_VALUE;
        }
        CloseHandle(h);
    }

    {
        DistantLoadInstrumentation::ScopedLoadTimer timer("statics.open_usage_data");
        h = CreateFile("Data Files\\distantland\\statics\\usage.data", GENERIC_READ, FILE_SHARE_READ, 0, OPEN_EXISTING, 0, 0);
        if (h == INVALID_HANDLE_VALUE) {
            LOG::logline("!! Required distant statics files are missing, regeneration required - distantland/statics/usage.data");
            LOG::flush();
            return INVALID_HANDLE_VALUE;
        }
    }

    return h;
}
