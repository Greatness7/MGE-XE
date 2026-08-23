//! Generation stage enum, progress-reporter trait, and the null reporter.

use serde::Serialize;

/// High-level phases emitted by the generation pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationStage {
    /// Resolve the Morrowind installation, data directories, and archives.
    InitializeVfs,
    /// Parse static override files and classifier rules.
    ParseOverrides,
    /// Read plugins and build usage information.
    ParsePlugins,
    /// Build the intermediate distant-static set.
    GenerateStatics,
    /// Measure texel density and plan per-texture atlas dimensions.
    AnalyzeTextureDensity,
    /// Pack static textures into atlas pages.
    CreateTextureAtlas,
    /// Simplify and merge mesh data before conversion.
    OptimizeMeshes,
    /// Convert intermediate statics into the final binary format.
    ConvertStatics,
    /// Write the `distantland\version` marker file.
    WriteVersionFile,
    /// Serialize `distantland\statics\usage.data`.
    WriteUsageData,
    /// Serialize dirty static-mesh shards for this build's fixed shard count.
    WriteStaticMeshes,
    /// Generate and serialize the terrain runtime package.
    WriteTerrainPackage,
    /// Compare the immutable current unit state with the previously committed one.
    ComputeUnitDiff,
    /// Write `distantland\generation_report.toml`.
    WriteGenerationReport,
}

/// Receives coarse generation lifecycle events.
pub trait ProgressReporter {
    fn begin_stage(&mut self, _stage: GenerationStage) {}
    fn finish_stage(&mut self, _stage: GenerationStage, _elapsed_ms: f64) {}
}

/// Progress reporter that discards all events.
#[derive(Default)]
pub struct NullProgressReporter;

impl ProgressReporter for NullProgressReporter {}
