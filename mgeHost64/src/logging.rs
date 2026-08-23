use std::fs::File;
use std::io::{self, BufWriter, IsTerminal};
use std::path::Path;
use std::sync::Once;

use tracing::Level;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::prelude::*;

use crate::error::HostError;

static PANIC_HOOK: Once = Once::new();

/// The guard must outlive logging so queued records can flush.
pub fn init_logger(path: impl AsRef<Path>) -> Result<WorkerGuard, HostError> {
    let file = File::create(path).map_err(HostError::io)?;

    let (writer, guard) = tracing_appender::non_blocking(BufWriter::new(file));

    let filter = EnvFilter::builder()
        .with_default_directive(Level::INFO.into())
        .with_regex(false)
        .from_env_lossy();

    let file_layer = tracing_subscriber::fmt::layer()
        .pretty()
        .with_writer(writer)
        .with_ansi(false)
        .with_target(false)
        .with_file(false)
        .with_line_number(false)
        .with_span_events(FmtSpan::CLOSE);

    let stderr_layer = io::stderr().is_terminal().then(|| {
        // The IPC host normally launches headlessly from the 32-bit runtime.
        tracing_subscriber::fmt::layer()
            .pretty()
            .with_writer(io::stderr)
            .with_target(false)
            .with_file(false)
            .with_line_number(false)
            .with_span_events(FmtSpan::CLOSE)
    });

    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer);

    let registry = registry.with(distantland::TraceReportLayer::with_handle(distantland::trace_report_handle()));

    if let Err(e) = registry.try_init() {
        return Err(HostError::init(format!("Failed to initialize logger: {e:?}")));
    }

    install_panic_hook();

    Ok(guard)
}

fn install_panic_hook() {
    PANIC_HOOK.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            let payload = info
                .payload()
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
                .unwrap_or("<non-string panic payload>");
            let location = info
                .location()
                .map(|loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()))
                .unwrap_or_else(|| "<unknown>".to_string());

            tracing::error!(panic_message = payload, panic_location = location, "mgeHost64 panicked");
        }));
    });
}
