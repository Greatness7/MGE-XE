
#include "winheader.h"
#include "log.h"
#include "crashlog.h"

#include <cstdio>

#ifdef MGE_ENABLE_CRASH_LOG

namespace {

    LPTOP_LEVEL_EXCEPTION_FILTER previousUnhandledFilter = nullptr;

    // Resolve an address to "module+0xRVA (0xADDR)". Module+offset is what lets us
    // map the fault back to source against the PDB, and needs no dbghelp.
    void describeAddress(void* addr, char* out, size_t outSize) {
        HMODULE mod = nullptr;
        if (GetModuleHandleExA(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                reinterpret_cast<LPCSTR>(addr), &mod) && mod) {
            char path[MAX_PATH] = {0};
            GetModuleFileNameA(mod, path, sizeof(path));

            // Strip to base name.
            const char* name = path;
            for (const char* p = path; *p; ++p) {
                if (*p == '\\' || *p == '/') {
                    name = p + 1;
                }
            }

            DWORD_PTR rva = reinterpret_cast<DWORD_PTR>(addr) - reinterpret_cast<DWORD_PTR>(mod);
            std::snprintf(out, outSize, "%s+0x%IX (0x%p)", name, rva, addr);
        } else {
            std::snprintf(out, outSize, "0x%p", addr);
        }
    }

    bool isHardwareFault(DWORD code) {
        switch (code) {
        case EXCEPTION_ACCESS_VIOLATION:
        case EXCEPTION_ARRAY_BOUNDS_EXCEEDED:
        case EXCEPTION_DATATYPE_MISALIGNMENT:
        case EXCEPTION_FLT_DENORMAL_OPERAND:
        case EXCEPTION_FLT_DIVIDE_BY_ZERO:
        case EXCEPTION_FLT_INEXACT_RESULT:
        case EXCEPTION_FLT_INVALID_OPERATION:
        case EXCEPTION_FLT_OVERFLOW:
        case EXCEPTION_FLT_STACK_CHECK:
        case EXCEPTION_FLT_UNDERFLOW:
        case EXCEPTION_ILLEGAL_INSTRUCTION:
        case EXCEPTION_IN_PAGE_ERROR:
        case EXCEPTION_INT_DIVIDE_BY_ZERO:
        case EXCEPTION_INT_OVERFLOW:
        case EXCEPTION_PRIV_INSTRUCTION:
        case EXCEPTION_STACK_OVERFLOW:
            return true;
        default:
            return false;
        }
    }

    bool reserveLogSlot(volatile LONG* logged) {
        if (InterlockedIncrement(logged) > 16) {
            return false;
        }

        return true;
    }

    void writeExceptionLog(EXCEPTION_POINTERS* info, const char* source) {
        const DWORD code = info->ExceptionRecord->ExceptionCode;
        char buf[512];

        LOG::logline("");
        LOG::logline("!! ==== Crash handler: caught %s exception 0x%08X ====", source, code);

        describeAddress(info->ExceptionRecord->ExceptionAddress, buf, sizeof(buf));
        LOG::logline("!! Faulting instruction: %s", buf);

        if ((code == EXCEPTION_ACCESS_VIOLATION || code == EXCEPTION_IN_PAGE_ERROR) &&
            info->ExceptionRecord->NumberParameters >= 2) {
            const ULONG_PTR op = info->ExceptionRecord->ExceptionInformation[0];
            void* target = reinterpret_cast<void*>(info->ExceptionRecord->ExceptionInformation[1]);
            const char* kind = (op == 0) ? "read" : (op == 1) ? "write" : (op == 8) ? "execute" : "?";
            LOG::logline("!! Access violation: %s at 0x%p", kind, target);
        }

        if (info->ExceptionRecord->NumberParameters > 0) {
            LOG::logline("!! Exception parameters (%lu):", info->ExceptionRecord->NumberParameters);
            for (DWORD i = 0; i < info->ExceptionRecord->NumberParameters; ++i) {
                LOG::logline("!!   [%lu] 0x%IX", i, info->ExceptionRecord->ExceptionInformation[i]);
            }
        }

        // Plain return-address walk: robust and dependency-free.
        void* frames[32] = {0};
        USHORT n = CaptureStackBackTrace(0, 32, frames, nullptr);
        LOG::logline("!! Backtrace (%u frames):", static_cast<unsigned>(n));
        for (USHORT i = 0; i < n; ++i) {
            describeAddress(frames[i], buf, sizeof(buf));
            LOG::logline("!!   [%02u] %s", static_cast<unsigned>(i), buf);
        }

        LOG::logline("!! ==== end crash handler ====");
        LOG::flush();
    }

    LONG CALLBACK vehHandler(EXCEPTION_POINTERS* info) {
        // Only act on hard faults. Benign first-chance exceptions (C++ EH, guard
        // page, debugger breakpoints, etc.) pass straight through untouched.
        if (!isHardwareFault(info->ExceptionRecord->ExceptionCode)) {
            return EXCEPTION_CONTINUE_SEARCH;
        }

        // Cap logging so a fault storm can't fill the disk. The last block written
        // before the process dies is the fatal one.
        static volatile LONG logged = 0;
        if (!reserveLogSlot(&logged)) {
            return EXCEPTION_CONTINUE_SEARCH;
        }

        writeExceptionLog(info, "first-chance");

        // We only observe the fault; let the OS / MWSE handler run as before.
        return EXCEPTION_CONTINUE_SEARCH;
    }

    LONG WINAPI unhandledFilter(EXCEPTION_POINTERS* info) {
        static volatile LONG logged = 0;
        if (reserveLogSlot(&logged)) {
            writeExceptionLog(info, "unhandled");
        }

        if (previousUnhandledFilter && previousUnhandledFilter != unhandledFilter) {
            return previousUnhandledFilter(info);
        }

        return EXCEPTION_CONTINUE_SEARCH;
    }

}


namespace CrashLog {
    void install() {
        // First (1) in the VEH chain, so MWSE's later SetUnhandledExceptionFilter
        // cannot hide the fault from us.
        AddVectoredExceptionHandler(1, vehHandler);
    }

    void installUnhandledFilter() {
        LPTOP_LEVEL_EXCEPTION_FILTER previous = SetUnhandledExceptionFilter(unhandledFilter);
        if (previous != unhandledFilter) {
            previousUnhandledFilter = previous;
        }
    }
}

#endif
