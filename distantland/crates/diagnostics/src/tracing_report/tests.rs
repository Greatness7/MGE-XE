use super::*;
use tracing::info_span;
use tracing_subscriber::prelude::*;

#[test]
fn collector_records_only_pipeline_stage_timings() {
    let (layer, handle) = TraceReportLayer::new();
    let subscriber = tracing_subscriber::Registry::default().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        let root = info_span!("generation", report = true);
        let _root = root.enter();
        let child = info_span!("stage.initialize_vfs", report = true, arbitrary = 3_u64);
        let _child = child.enter();
        let _ignored = info_span!("terrain.save_normal_dds", report = true).entered();
    });

    let summary = handle.snapshot();
    assert_eq!(summary.stage_timings.len(), 1);
    assert_eq!(summary.stage_timings[0].stage, "stage.initialize_vfs");
}

#[test]
fn collector_filters_non_pipeline_spans() {
    let (layer, handle) = TraceReportLayer::new();
    let subscriber = tracing_subscriber::Registry::default().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        let _span = info_span!("not_reported", report = true).entered();
    });

    assert!(handle.snapshot().stage_timings.is_empty());
}

#[test]
fn collector_clear_drops_previous_run_data() {
    let (layer, handle) = TraceReportLayer::new();
    let subscriber = tracing_subscriber::Registry::default().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        let _span = info_span!("stage.initialize_vfs").entered();
    });
    assert_eq!(handle.snapshot().stage_timings.len(), 1);

    handle.clear();
    assert!(handle.snapshot().stage_timings.is_empty());
    assert_eq!(handle.snapshot().total_elapsed_ms, 0);
}

#[test]
fn stage_memory_is_omitted_from_serialization_when_absent() {
    let summary = TraceSummary {
        total_elapsed_ms: 7,
        stage_timings: vec![StageTiming {
            stage: "stage.initialize_vfs".to_string(),
            elapsed_ms: 7,
            memory: None,
        }],
    };

    let rendered = toml::to_string_pretty(&summary).unwrap();
    assert!(!rendered.contains("memory"), "unexpected memory table in {rendered}");
    assert_eq!(toml::from_str::<TraceSummary>(&rendered).unwrap(), summary);
}

#[test]
fn stage_memory_round_trips_through_the_report_format() {
    let summary = TraceSummary {
        total_elapsed_ms: 7,
        stage_timings: vec![
            StageTiming {
                stage: "stage.initialize_vfs".to_string(),
                elapsed_ms: 7,
                memory: Some(StageMemory {
                    private_bytes_at_end: 1_234,
                    peak_working_set_bytes_at_end: 5_678,
                    private_bytes_at_start: 1_000,
                    peak_working_set_bytes_at_start: 5_000,
                }),
            },
            StageTiming {
                stage: "stage.write_terrain_package".to_string(),
                elapsed_ms: 9,
                memory: None,
            },
        ],
    };

    let rendered = toml::to_string_pretty(&summary).unwrap();
    assert_eq!(toml::from_str::<TraceSummary>(&rendered).unwrap(), summary);
}

#[test]
fn stage_timings_deserialize_from_reports_written_without_memory() {
    let summary: TraceSummary = toml::from_str(
        r#"
total_elapsed_ms = 7

[[stage_timings]]
stage = "stage.initialize_vfs"
elapsed_ms = 7
"#,
    )
    .unwrap();

    assert_eq!(summary.stage_timings[0].memory, None);
}

#[test]
fn active_stage_snapshots_report_no_memory() {
    let (layer, handle) = TraceReportLayer::new();
    let subscriber = tracing_subscriber::Registry::default().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        let _span = info_span!("stage.initialize_vfs").entered();
        let summary = handle.snapshot();
        assert_eq!(summary.stage_timings.len(), 1);
        assert_eq!(summary.stage_timings[0].memory, None);
    });
}

#[cfg(windows)]
#[test]
fn closed_stage_spans_capture_process_memory() {
    let (layer, handle) = TraceReportLayer::new();
    let subscriber = tracing_subscriber::Registry::default().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        let _span = info_span!("stage.initialize_vfs").entered();
    });

    let memory = handle.snapshot().stage_timings[0]
        .memory
        .expect("closed stage span should sample process memory on Windows");
    assert!(memory.private_bytes_at_end > 0);
    assert!(memory.peak_working_set_bytes_at_end > 0);
}
