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

    // Silences first-chance hardware-fault logging on the calling thread for the
    // lifetime of the scope. Only for a fault MGE XE provokes on purpose and
    // catches itself, which would otherwise read as a crash in the log and spend
    // one of the sixteen log slots reserved for real ones. A fault that escapes
    // the scope is still logged.
    class ExpectedFaultScope {
    public:
        ExpectedFaultScope();
        ~ExpectedFaultScope();
        ExpectedFaultScope(const ExpectedFaultScope&) = delete;
        ExpectedFaultScope& operator=(const ExpectedFaultScope&) = delete;
    };
#else
    inline void install() {}
    inline void installUnhandledFilter() {}

    class ExpectedFaultScope {};
#endif
}
