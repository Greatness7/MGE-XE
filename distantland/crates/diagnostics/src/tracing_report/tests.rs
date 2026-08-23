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
