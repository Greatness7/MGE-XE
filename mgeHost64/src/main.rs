#![windows_subsystem = "windows"]

use std::path::Path;
use std::process;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use tracing::{error, info};

mod abi;
mod config;
mod error;
mod ipc;
mod logging;
mod startup_generation;
mod state;
#[cfg(test)]
mod test_support;
mod win;

use distantland::output_index::{OutputSnapshot, OutputValidation, open_output_snapshot};
use ipc::server::{OutputState, Server};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Only the IPC host mode launched by `d3d8.dll` is supported.
fn main() {
    let guard = match logging::init_logger("mgeHost64.log") {
        Ok(guard) => guard,
        Err(error) => {
            process::exit(error.exit_code());
        }
    };

    if let Err(error) = run() {
        error!("{error}");
        drop(guard);
        process::exit(error.exit_code());
    }
}

fn run() -> Result<(), error::HostError> {
    info!(command_line = %win::raw_command_line(), "Host process started");

    let handles = win::parse_startup_handles()?;
    let configuration = config::Configuration::load();
    let output_state = Arc::new(Mutex::new(OutputState::default()));

    let worker_output_state = Arc::clone(&output_state);
    thread::spawn(move || {
        // Debug unwinds become OutputFailed; release builds abort and report ServerLost.
        let worker_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut generation_configuration = configuration;
            let morrowind_root = std::env::current_dir().map_err(|error| {
                error!(%error, "Unable to determine Morrowind root for startup generation");
            })?;

            if generation_configuration.automatic_distant_land_rebuild {
                let status = startup_generation::ensure_distant_land_ready(&mut generation_configuration, &morrowind_root);
                info!(?status, "Distant-land startup generation policy completed");
            } else {
                info!("Automatic distant-land rebuild is disabled; using existing distant-land output");
            }

            let output_root = morrowind_root.join("Data Files");
            let snapshot = open_session_output_snapshot(&output_root, Duration::from_secs(30));
            if let Ok(mut output) = worker_output_state.lock() {
                output.failed = snapshot.is_none();
                output.snapshot = snapshot;
                output.configuration = Some(generation_configuration);
            }
            Ok::<(), ()>(())
        }));

        if !matches!(worker_result, Ok(Ok(()))) {
            error!("Distant-land startup generation worker failed");
            if let Ok(mut output) = worker_output_state.lock() {
                output.failed = true;
                output.configuration = Some(configuration);
            }
        }
    });

    let mut server = Server::new(handles, configuration, output_state);
    server.init()?;
    server.listen()?;
    Ok(())
}

fn open_session_output_snapshot(output_root: &Path, timeout: Duration) -> Option<OutputSnapshot> {
    match open_output_snapshot(output_root, timeout, OutputValidation::Routine) {
        Ok(snapshot) => Some(snapshot),
        Err(error) => {
            error!(output_root = %output_root.display(), %error, "Distant-land output is unavailable for this session");
            None
        }
    }
}
