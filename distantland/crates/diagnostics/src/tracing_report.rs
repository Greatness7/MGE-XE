//! In-process collector for the bounded timing summary written to the generation report.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use hashbrown::HashMap;
use serde::{Deserialize, Serialize};
use tracing::span::Attributes;
use tracing::{Id, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

/// Maximum number of pipeline-stage timings retained in one report.
const MAX_STAGE_TIMINGS: usize = 128;
/// Top-level span opened by the generation pipeline.
const GENERATION_SPAN_NAME: &str = "generation";

/// Shared handle for collecting and snapshotting pipeline timings.
#[derive(Clone, Debug, Default)]
pub struct TraceReportHandle {
    inner: Arc<Mutex<TraceReportState>>,
}

#[derive(Debug, Default)]
struct TraceReportState {
    next_id: usize,
    active: HashMap<usize, ActiveTiming>,
    completed: Vec<CompletedStageTiming>,
    total_elapsed_ms: u64,
}

#[derive(Debug)]
struct ActiveTiming {
    id: usize,
    name: String,
    started_at: Instant,
    /// Counters sampled when the span opened, paired with a close-time sample to give the
    /// span an interval rather than a lone endpoint. Spans that open past
    /// [`MAX_STAGE_TIMINGS`] completions still sample here and are discarded at close;
    /// the pipeline's stage count makes that waste negligible.
    memory_at_start: Option<ProcessMemorySample>,
}

#[derive(Clone, Debug)]
struct CompletedStageTiming {
    id: usize,
    timing: StageTiming,
}

#[derive(Clone, Copy, Debug)]
struct ReportableSpanId(usize);

/// Bounded timing summary persisted in `generation_report.toml`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct TraceSummary {
    /// Elapsed time of the top-level generation span in milliseconds.
    pub total_elapsed_ms: u64,
    /// Timings for `stage.*` pipeline spans in opening order.
    pub stage_timings: Vec<StageTiming>,
}

/// Elapsed time for one pipeline stage.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct StageTiming {
    /// Tracing span name, restricted to the `stage.*` namespace.
    pub stage: String,
    /// Elapsed wall-clock time in milliseconds.
    pub elapsed_ms: u64,
    /// Process memory sampled when the span closed. Absent for spans still active at
    /// snapshot time and on non-Windows targets.
    ///
    /// Declared last so the rendered TOML table stays valid inside
    /// `[[trace_summary.stage_timings]]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<StageMemory>,
}

/// Process-wide memory counters bracketing one pipeline stage.
///
/// Every value describes the whole process, not the stage in isolation, and `stage.*` spans
/// nest, so a parent's interval contains its children's. Read the pairs as intervals:
/// `private_bytes_at_end - private_bytes_at_start` is how much private commit the span left
/// behind, and a rise from `peak_working_set_bytes_at_start` to `peak_working_set_bytes_at_end`
/// says the process high-water mark grew while the span was open. Neither is an exclusive
/// per-stage peak, and neither endpoint captures a spike that was released before the close.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct StageMemory {
    /// Instantaneous private commit (`PrivateUsage`) at span close.
    pub private_bytes_at_end: u64,
    /// Process-lifetime working-set high-water mark (`PeakWorkingSetSize`) as of span
    /// close. Not a per-stage peak and not a private-byte peak.
    pub peak_working_set_bytes_at_end: u64,
    /// `PrivateUsage` at span open. Defaulted so reports written before the open-time
    /// sample existed still decode.
    #[serde(default)]
    pub private_bytes_at_start: u64,
    /// `PeakWorkingSetSize` at span open. Defaulted so reports written before the
    /// open-time sample existed still decode.
    #[serde(default)]
    pub peak_working_set_bytes_at_start: u64,
}

/// One reading of the process-wide counters, before it is paired into a [`StageMemory`].
#[derive(Clone, Copy, Debug)]
struct ProcessMemorySample {
    private_bytes: u64,
    peak_working_set_bytes: u64,
}

impl StageMemory {
    /// Pairs an open-time and close-time sample, yielding `None` unless both succeeded.
    fn from_endpoints(start: Option<ProcessMemorySample>, end: Option<ProcessMemorySample>) -> Option<Self> {
        let (start, end) = (start?, end?);
        Some(Self {
            private_bytes_at_end: end.private_bytes,
            peak_working_set_bytes_at_end: end.peak_working_set_bytes,
            private_bytes_at_start: start.private_bytes,
            peak_working_set_bytes_at_start: start.peak_working_set_bytes,
        })
    }
}

/// Samples process memory counters, returning `None` when unsupported or unavailable.
#[cfg(windows)]
fn sample_process_memory() -> Option<ProcessMemorySample> {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    let cb = counters.cb;
    let filled = unsafe {
        // The EX layout begins with the base counters and only extends it, so the API
        // fills this buffer in place; `cb` declares the extended length that makes
        // `PrivateUsage` valid on return. The pseudo-handle needs no release.
        K32GetProcessMemoryInfo(GetCurrentProcess(), (&raw mut counters).cast::<PROCESS_MEMORY_COUNTERS>(), cb)
    };
    (filled != 0).then_some(ProcessMemorySample {
        private_bytes: counters.PrivateUsage as u64,
        peak_working_set_bytes: counters.PeakWorkingSetSize as u64,
    })
}

/// Samples process memory counters, returning `None` when unsupported or unavailable.
#[cfg(not(windows))]
fn sample_process_memory() -> Option<ProcessMemorySample> {
    None
}

impl ActiveTiming {
    fn elapsed_ms(&self, now: Instant) -> u64 {
        u64::try_from(now.duration_since(self.started_at).as_millis()).unwrap_or(u64::MAX)
    }
}

impl TraceReportHandle {
    fn state(&self) -> MutexGuard<'_, TraceReportState> {
        self.inner.lock().expect("trace report mutex poisoned")
    }

    /// Clears collected timing state so the handle can be reused for a fresh run.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex has been poisoned.
    pub fn clear(&self) {
        let mut state = self.state();
        state.next_id = 0;
        state.active.clear();
        state.completed.clear();
        state.total_elapsed_ms = 0;
    }

    /// Returns a point-in-time bounded summary of pipeline timings.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex has been poisoned.
    pub fn snapshot(&self) -> TraceSummary {
        let now = Instant::now();
        let state = self.state();
        let total_elapsed_ms = state
            .active
            .values()
            .find(|timing| timing.name == GENERATION_SPAN_NAME)
            .map(|timing| timing.elapsed_ms(now))
            .unwrap_or(state.total_elapsed_ms);
        let mut stage_timings = state.completed.clone();
        stage_timings.extend(
            state
                .active
                .values()
                .filter(|timing| timing.name.starts_with("stage."))
                .map(|timing| CompletedStageTiming {
                    id: timing.id,
                    timing: StageTiming {
                        stage: timing.name.clone(),
                        elapsed_ms: timing.elapsed_ms(now),
                        // Still open, so there is no close-time endpoint to pair the
                        // open-time sample with; a half-interval would misreport.
                        memory: None,
                    },
                }),
        );
        stage_timings.sort_by_key(|timing| timing.id);
        stage_timings.truncate(MAX_STAGE_TIMINGS);
        TraceSummary {
            total_elapsed_ms,
            stage_timings: stage_timings.into_iter().map(|timing| timing.timing).collect(),
        }
    }
}

/// Tracing layer that records only the top-level generation span and `stage.*` spans.
#[derive(Clone, Debug)]
pub struct TraceReportLayer {
    handle: TraceReportHandle,
}

impl TraceReportLayer {
    /// Creates a layer and a fresh query handle.
    pub fn new() -> (Self, TraceReportHandle) {
        let handle = TraceReportHandle::default();
        (Self { handle: handle.clone() }, handle)
    }

    /// Creates a layer that writes into an existing shared handle.
    pub fn with_handle(handle: TraceReportHandle) -> Self {
        Self { handle }
    }
}

impl<S> Layer<S> for TraceReportLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let name = attrs.metadata().name();
        if name != GENERATION_SPAN_NAME && !name.starts_with("stage.") {
            return;
        }

        let mut state = self.handle.state();
        let report_id = state.next_id;
        state.next_id += 1;
        state.active.insert(
            report_id,
            ActiveTiming {
                id: report_id,
                name: name.to_owned(),
                started_at: Instant::now(),
                memory_at_start: sample_process_memory(),
            },
        );
        drop(state);

        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(ReportableSpanId(report_id));
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else {
            return;
        };
        let Some(report_id) = span.extensions().get::<ReportableSpanId>().copied().map(|id| id.0) else {
            return;
        };
        let mut state = self.handle.state();
        let Some(active) = state.active.remove(&report_id) else {
            return;
        };
        let elapsed_ms = active.elapsed_ms(Instant::now());
        if active.name == GENERATION_SPAN_NAME {
            state.total_elapsed_ms = elapsed_ms;
        } else if state.completed.len() < MAX_STAGE_TIMINGS {
            let memory = StageMemory::from_endpoints(active.memory_at_start, sample_process_memory());
            state.completed.push(CompletedStageTiming {
                id: active.id,
                timing: StageTiming {
                    stage: active.name,
                    elapsed_ms,
                    memory,
                },
            });
        }
    }
}

#[cfg(test)]
mod tests;
