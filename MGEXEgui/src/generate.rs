//! In-process distant-land generation for the configuration viewport.
//!
//! Runs `distantland::ensure_generated` on a detached worker thread and
//! streams progress back to the UI via an `mpsc` channel. The workspace
//! release profile is `panic = "abort"` so a worker panic takes the whole
//! process, so a `catch_unwind` guard would be dead code.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use distantland::{
    DistantLandGpuMemoryEstimate, GenerationJob, GenerationOutcome, GenerationStage, ProgressReporter, ensure_generated,
    load_generation_report_data, output_has_live_session,
};

/// Number of `GenerationStage` variants, the denominator for the progress bar.
pub const NUM_STAGES: u32 = 14;

/// Worker → UI messages. Progress events arrive in pipeline order; exactly one
/// `Done` is sent, last, after which the sender is dropped.
pub enum Message {
    /// A generation stage began. The UI maps this value to localized text.
    Progress(GenerationStage),
    /// Generation finished, the terminal message.
    Done(Outcome),
}

/// Terminal result of a generation run, handed to the UI on the channel.
pub enum Outcome {
    /// Generation left a valid committed tree. `warnings` are `code: message`
    /// lines from the generation report.
    ///
    /// `gpu_memory` is `None` when the tree was already valid and its advisory report is
    /// missing, unreadable, or written by an older schema. The report is advisory, so that
    /// only costs the finished page its estimate. The output itself is still good.
    Success {
        warnings: Vec<String>,
        gpu_memory: Option<DistantLandGpuMemoryEstimate>,
    },
    /// The live output is locked, normally because Morrowind is running.
    OutputInUse,
    /// Generation failed or was refused; `message` is the human-readable reason.
    Failure { message: String },
}

/// Spawn the detached generation worker, returning the receiver the viewport
/// polls each frame. Dropping the receiver (closing the viewport) makes the
/// worker's sends fail silently; the run itself cannot be cancelled once
/// entered.
pub fn spawn(job: GenerationJob) -> Receiver<Message> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let outcome = run_worker(&sender, job);
        let _ = sender.send(Message::Done(outcome));
    });
    receiver
}

fn run_worker(sender: &Sender<Message>, job: GenerationJob) -> Outcome {
    // Advisory pre-check: the real exclusive lock inside `ensure_generated` is
    // authoritative, so a probe error is not fatal. A held lock, though, is the
    // everyday "Morrowind is open" case and deserves the clear message.
    if let Some(output_root) = job.output_root.as_deref() {
        match output_has_live_session(output_root) {
            Ok(true) => {
                return Outcome::OutputInUse;
            }
            Ok(false) => {}
            Err(_error) => {}
        }
    }

    let mut reporter = ChannelReporter { sender: sender.clone() };
    match ensure_generated(&job, &mut reporter) {
        Ok(GenerationOutcome::Generated { report, .. }) => {
            let warnings = report
                .warnings
                .iter()
                .map(|warning| format!("{}: {}", warning.code, warning.message))
                .collect();
            Outcome::Success {
                warnings,
                gpu_memory: Some(report.report.metrics.gpu_memory),
            }
        }
        Ok(GenerationOutcome::AlreadyValid { status }) => Outcome::Success {
            warnings: Vec::new(),
            // Nothing was generated, so the estimate has to come off disk. A decode failure
            // here is not an error worth surfacing: it means an older or hand-edited report,
            // and the finished page says so.
            gpu_memory: load_generation_report_data(&status.details().generation_report_path)
                .ok()
                .map(|report| report.metrics.gpu_memory),
        },
        Err(error) => Outcome::Failure {
            message: format!("{error:#}"),
        },
    }
}

struct ChannelReporter {
    sender: Sender<Message>,
}

impl ProgressReporter for ChannelReporter {
    fn begin_stage(&mut self, stage: GenerationStage) {
        let _ = self.sender.send(Message::Progress(stage));
    }
}

/// Map a stage to its progress-bar index and localization key. The indices are
/// a curated display order, not the enum's declaration order.
pub fn stage_progress(stage: GenerationStage) -> (u32, &'static str) {
    use GenerationStage::*;
    match stage {
        InitializeVfs => (0, "generator.stages.initialize_vfs"),
        ParseOverrides => (1, "generator.stages.parse_overrides"),
        ParsePlugins => (2, "generator.stages.parse_plugins"),
        GenerateStatics => (3, "generator.stages.generate_statics"),
        AnalyzeTextureDensity => (4, "generator.stages.analyze_texture_density"),
        CreateTextureAtlas => (5, "generator.stages.create_texture_atlas"),
        ComputeUnitDiff => (6, "generator.stages.compute_unit_diff"),
        OptimizeMeshes => (7, "generator.stages.optimize_meshes"),
        ConvertStatics => (8, "generator.stages.convert_statics"),
        WriteVersionFile => (9, "generator.stages.write_version"),
        WriteUsageData => (10, "generator.stages.write_usage"),
        WriteStaticMeshes => (11, "generator.stages.write_statics"),
        WriteTerrainPackage => (12, "generator.stages.write_terrain"),
        WriteGenerationReport => (13, "generator.stages.write_generation_report"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every stage the pipeline can emit. Kept explicit rather than derived so a
    /// new `GenerationStage` variant fails to compile here, forcing a decision
    /// about its label and index instead of silently falling through.
    const ALL_STAGES: [GenerationStage; NUM_STAGES as usize] = [
        GenerationStage::InitializeVfs,
        GenerationStage::ParseOverrides,
        GenerationStage::ParsePlugins,
        GenerationStage::GenerateStatics,
        GenerationStage::AnalyzeTextureDensity,
        GenerationStage::CreateTextureAtlas,
        GenerationStage::ComputeUnitDiff,
        GenerationStage::OptimizeMeshes,
        GenerationStage::ConvertStatics,
        GenerationStage::WriteVersionFile,
        GenerationStage::WriteUsageData,
        GenerationStage::WriteStaticMeshes,
        GenerationStage::WriteTerrainPackage,
        GenerationStage::WriteGenerationReport,
    ];

    #[test]
    fn every_stage_maps_to_a_distinct_index_and_a_label() {
        let mut seen = [false; NUM_STAGES as usize];
        for stage in ALL_STAGES {
            let (index, label) = stage_progress(stage);
            assert!(index < NUM_STAGES, "{stage:?} index {index} out of range");
            assert!(!label.is_empty(), "{stage:?} has an empty label");
            assert!(
                !std::mem::replace(&mut seen[index as usize], true),
                "index {index} is used by more than one stage"
            );
        }
        // Every slot in the progress range is claimed, so the bar has no gaps.
        assert!(seen.iter().all(|&used| used));
    }

    fn drain(receiver: Receiver<Message>) -> Outcome {
        let mut outcome = None;
        for message in receiver {
            if let Message::Done(done) = message {
                outcome = Some(done);
            }
        }
        outcome.expect("the worker always sends exactly one Done")
    }

    /// Covers all three ways the finished page can be reached: a fresh run, a no-op
    /// run reading the report back off disk, and a no-op run whose advisory report is
    /// gone. The last one must still be a success, because the output is valid either way.
    #[test]
    fn a_successful_run_carries_the_estimate_when_a_current_report_exists() {
        let root = std::env::temp_dir().join(format!(
            "mge-gui-gen-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let inputs = distantland_test_support::build_hermetic_fixture(
            distantland_test_support::BASELINE_WORLD_V1,
            &root.join("inputs"),
        )
        .unwrap();
        let output_root = root.join("Data Files");
        let job = distantland_test_support::hermetic_generation_job(&inputs, &output_root);

        let generated = drain(spawn(job.clone()));
        let Outcome::Success { gpu_memory, .. } = generated else {
            panic!("a hermetic fixture must generate successfully");
        };
        let generated_estimate = gpu_memory.expect("a fresh run reports its own metrics");
        assert!(generated_estimate.total_bytes() > 0);

        let already_valid = drain(spawn(job.clone()));
        let Outcome::Success { gpu_memory, .. } = already_valid else {
            panic!("an unchanged tree must stay valid");
        };
        assert_eq!(
            gpu_memory,
            Some(generated_estimate),
            "the report read off disk must match the one just written"
        );

        let report_path = distantland::OutputPaths::new(&output_root).generation_report_path;
        std::fs::write(&report_path, b"not a generation report").unwrap();
        let without_report = drain(spawn(job));
        let Outcome::Success { gpu_memory, .. } = without_report else {
            panic!("an unreadable advisory report must not fail the run");
        };
        assert_eq!(gpu_memory, None);

        std::fs::remove_dir_all(&root).ok();
    }
}
