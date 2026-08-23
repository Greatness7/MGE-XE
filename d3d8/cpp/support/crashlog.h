#pragma once

namespace CrashLog {
#ifdef MGE_ENABLE_CRASH_LOG
    // Installs a first-chance vectored handler for hardware faults. Call early
    // (in DllMain) so it sits ahead of anything MWSE installs.
    void install();

    // Installs our top-level unhandled-exception filter, chained ahead of any
    // existing one (e.g. MWSE's). Call AFTER MWSE has loaded so we win, since
    // SetUnhandledExceptionFilter keeps only the most recently installed filter.
    void installUnhandledFilter();
#else
    inline void install() {}
    inline void installUnhandledFilter() {}
#endif
}
