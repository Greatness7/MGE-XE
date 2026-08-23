//! Minimal `--job` CLI for one-shot distant-land generation (profiling / dev runs).
//! The GUI/host drive generation through the library `generate()` API directly.

use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Arg, ArgAction, command};

use distantland::{NullProgressReporter, OutputWriteDecision, generate, info, init_logger, load_generation_job_file, warn};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> Result<()> {
    let matches = command!()
        .about("One-shot MGE-XE distant-land generation (profiling / dev). Hosts should call generate() directly.")
        .args(&[
            Arg::new("JOB")
                .long("job")
                .help("Versioned generation job to run, as TOML.")
                .value_name("FILE")
                .value_parser(clap::value_parser!(PathBuf))
                .required(true),
            Arg::new("FORCE-REBUILD")
                .long("force-rebuild")
                .help("Force a full rebuild instead of reusing existing generated artifacts.")
                .action(ArgAction::SetTrue),
        ])
        .get_matches();

    // Hold the logging guard for the rest of `main` so the non-blocking writer flushes
    // the final output-path, generation-report, and warning records before the process exits.
    let _logging = match init_logger() {
        Ok(guard) => guard,
        Err(err) => bail!("failed to initialize logger: {err}"),
    };

    let job_path: &PathBuf = matches.get_one("JOB").unwrap();
    let force_rebuild = matches.get_flag("FORCE-REBUILD");

    info!(job = %job_path.display(), force_rebuild, "CLI starting generation");

    let mut job = load_generation_job_file(job_path)?;

    if force_rebuild {
        job.settings.force_rebuild = true;
    }

    let mut reporter = NullProgressReporter;
    let report = generate(&job, &mut reporter)?;

    info!(output_root = %report.output_root.display(), "Generated distant land");
    match report.report_written {
        OutputWriteDecision::Written => {
            info!(generation_report = %report.report_path.display(), "Wrote generation report");
        }
        OutputWriteDecision::SkippedUnchanged => {
            info!(generation_report = %report.report_path.display(), "Generation report unchanged, not rewritten");
        }
    }

    for warning in &report.warnings {
        warn!(code = %warning.code, "{}", warning.message);
    }

    Ok(())
}
