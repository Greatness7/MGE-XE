#pragma once

#include "support/winheader.h"
#include "support/log.h"

#include <cstring>

class membuf_reader {
    char* ptr;

public:
    membuf_reader(char* buf) : ptr(buf) {}

    template <typename T>
    inline void read(T* dest, size_t size) {
        memcpy((char*)dest, ptr, size);
        ptr += size;
    }

    inline char* get() {
        return ptr;
    }

    inline void advance(size_t size) {
        ptr += size;
    }
};

namespace DistantLoadInstrumentation {
    inline LARGE_INTEGER counter_now() {
        LARGE_INTEGER counter;
        QueryPerformanceCounter(&counter);
        return counter;
    }

    inline double elapsed_ms(const LARGE_INTEGER& start, const LARGE_INTEGER& end) {
        static const double milliseconds_per_tick = []() {
            LARGE_INTEGER frequency;
            QueryPerformanceFrequency(&frequency);
            return 1000.0 / static_cast<double>(frequency.QuadPart);
        }();
        return static_cast<double>(end.QuadPart - start.QuadPart) * milliseconds_per_tick;
    }

    inline double elapsed_ms(const LARGE_INTEGER& start) {
        return elapsed_ms(start, counter_now());
    }

    inline void log_timing(const char* label, double milliseconds) {
        LOG::logline("-- Distant load timing: %s %.2f ms", label, milliseconds);
    }

    class ScopedLoadTimer {
        const char* label;
        LARGE_INTEGER start;

    public:
        explicit ScopedLoadTimer(const char* label)
            : label(label), start(counter_now()) {
        }

        ~ScopedLoadTimer() {
            log_timing(label, elapsed_ms(start));
        }
    };

    inline bool ReadExact(HANDLE file, void* buffer, size_t bytes, const char* label) {
        if (bytes == 0) {
            return true;
        }

        const DWORD requested = static_cast<DWORD>(bytes);
        if (static_cast<size_t>(requested) != bytes) {
            LOG::logline("!! Distant land read too large for Win32 API (%s): %zu bytes", label, bytes);
            LOG::flush();
            return false;
        }

        DWORD read = 0;
        if (!ReadFile(file, buffer, requested, &read, 0)) {
            LOG::winerror("Failed to read distant land data (%s)", label);
            LOG::flush();
            return false;
        }
        if (read != requested) {
            LOG::logline("!! Short distant land read (%s): expected %lu bytes, got %lu", label, requested, read);
            LOG::flush();
            return false;
        }
        return true;
    }

}

class DistantLandShare {
public:
    static bool hasCurrentWorldSpace;

    static HANDLE beginReadStatics();
};
