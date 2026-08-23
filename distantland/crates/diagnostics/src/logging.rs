//! Logging initialization and trace-summary plumbing.

use std::error::Error;
use std::fmt;
use std::sync::{Mutex, OnceLock};

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::prelude::*;

use crate::tracing_report::{TraceReportHandle, TraceReportLayer};

pub use tracing::{debug, error, info, trace, warn};

type BoxedError = Box<dyn Error + Send + Sync + 'static>;

/// Shares the in-memory trace summary collector across all callers.
static TRACE_REPORT: OnceLock<TraceReportHandle> = OnceLock::new();
/// Serializes the one process-global subscriber installation attempt.
static LOGGING_STATE: Mutex<LoggingState> = Mutex::new(LoggingState::Uninitialized);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LoggingState {
    Uninitialized,
    Installed,
}

#[derive(Debug)]
enum LoggingInitializationError {
    AlreadyInitialized,
    StatePoisoned,
}

impl fmt::Display for LoggingInitializationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInitialized => f.write_str(
                "logging has already been initialized; retain the original LoggingGuard for the full logging lifetime",
            ),
            Self::StatePoisoned => f.write_str("logging initialization state is poisoned"),
        }
    }
}

impl Error for LoggingInitializationError {}

/// Owns the non-blocking stderr worker guard for the logging lifetime.
///
/// Dropping the guard flushes queued records. Keep it alive until normal process shutdown;
/// aborting shutdowns remain best-effort because they bypass `Drop`.
///
/// Do not hide the guard in a process-global static: statics are not dropped on an ordinary
/// return, so final queued records can be lost. Bind the returned guard for the executable's
/// full logging lifetime.
#[must_use = "dropping the guard immediately flushes and stops logging; hold it for the executable's lifetime"]
pub struct LoggingGuard {
    _worker: WorkerGuard,
}

impl fmt::Debug for LoggingGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoggingGuard").finish_non_exhaustive()
    }
}

/// Runs one initialization closure while holding the process-global state lock.
///
/// The state changes to [`LoggingState::Installed`] only after success, leaving failed attempts
/// retryable and serializing concurrent callers.
fn serialize_initialization<T>(
    state: &Mutex<LoggingState>,
    initialize: impl FnOnce() -> Result<T, BoxedError>,
) -> Result<T, BoxedError> {
    let mut state = state
        .lock()
        .map_err(|_| Box::new(LoggingInitializationError::StatePoisoned) as BoxedError)?;

    if *state == LoggingState::Installed {
        return Err(Box::new(LoggingInitializationError::AlreadyInitialized));
    }

    let initialized = initialize()?;
    *state = LoggingState::Installed;
    Ok(initialized)
}

/// Initializes the global tracing subscriber and returns the guard that flushes its writer.
///
/// Initialization is serialized and succeeds once. Creating the shared
/// [`TraceReportHandle`] does not install logging, and failed setup remains retryable.
///
/// # Errors
///
/// Returns an error if logging is already installed, another subscriber owns the process-global
/// dispatcher, the state lock is poisoned, or subscriber setup fails.
pub fn init_logger() -> Result<LoggingGuard, BoxedError> {
    serialize_initialization(&LOGGING_STATE, || {
        let trace_report = trace_report_handle();
        let (writer, worker) = tracing_appender::non_blocking(std::io::stderr());

        let filter = tracing_subscriber::EnvFilter::builder()
            .with_default_directive("distantland=info".parse()?)
            .with_regex(false)
            .from_env_lossy();

        let fmt_layer = tracing_subscriber::fmt::layer()
            .pretty()
            .with_writer(writer)
            .with_target(false)
            .with_file(false)
            .with_line_number(false);

        let report_layer = TraceReportLayer::with_handle(trace_report.clone());

        let subscriber = tracing_subscriber::Registry::default()
            .with(filter)
            .with(fmt_layer)
            .with(report_layer);

        #[cfg(feature = "trace_tracy")]
        let subscriber = subscriber.with(tracing_tracy::TracyLayer::default());

        subscriber.try_init()?;

        Ok(LoggingGuard { _worker: worker })
    })
}

/// Returns the lazily-created process-global trace-report handle without initializing logging.
pub fn trace_report_handle() -> TraceReportHandle {
    TRACE_REPORT.get_or_init(TraceReportHandle::default).clone()
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex};
    use std::thread;
    use std::time::Duration;

    use super::{BoxedError, LoggingState, serialize_initialization, trace_report_handle};

    #[test]
    fn trace_report_handle_does_not_mark_logging_installed() {
        let handle = trace_report_handle();
        let summary = handle.snapshot();
        assert_eq!(summary.total_elapsed_ms, 0);
        assert!(summary.stage_timings.is_empty());

        let state = Mutex::new(LoggingState::Uninitialized);
        let initialized = serialize_initialization(&state, || Ok(42)).unwrap();

        assert_eq!(initialized, 42);
        assert_eq!(*state.lock().unwrap(), LoggingState::Installed);
    }

    #[test]
    fn failed_initialization_can_be_retried() {
        let state = Mutex::new(LoggingState::Uninitialized);

        let first = serialize_initialization(&state, || {
            Err::<(), BoxedError>(Box::new(io::Error::other("installation failed")))
        });
        assert!(first.is_err());
        assert_eq!(*state.lock().unwrap(), LoggingState::Uninitialized);

        serialize_initialization(&state, || Ok(())).unwrap();
        assert_eq!(*state.lock().unwrap(), LoggingState::Installed);
    }

    #[test]
    fn concurrent_initialization_has_one_winner() {
        let state = Arc::new(Mutex::new(LoggingState::Uninitialized));
        let barrier = Arc::new(Barrier::new(2));
        let attempts = Arc::new(AtomicUsize::new(0));

        let workers: Vec<_> = (0..2)
            .map(|_| {
                let state = Arc::clone(&state);
                let barrier = Arc::clone(&barrier);
                let attempts = Arc::clone(&attempts);
                thread::spawn(move || {
                    barrier.wait();
                    serialize_initialization(&state, || {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(10));
                        Ok(())
                    })
                    .is_ok()
                })
            })
            .collect();

        let successes = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .filter(|succeeded| *succeeded)
            .count();

        assert_eq!(successes, 1);
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(*state.lock().unwrap(), LoggingState::Installed);
    }
}
