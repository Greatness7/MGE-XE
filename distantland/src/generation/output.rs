//! Public output-contract and report types for generated distant-land artifacts.
//!
//! Re-exports the path layout ([`OutputPaths`]), contract validation types,
//! and report types so callers do not need to reach into the internal submodules.

mod manifest;

use distantland_foundation::output as contract;

pub use contract::{
    GENERATION_REPORT_NAME, GeneratedFileInfo, MGE_DL_VERSION, OPAQUE_ATLAS_PREFIX, OutputContractValidation,
    OutputFileStatus, OutputPaths, OutputValidationIssue, STATIC_MESH_SHARD_COUNT, static_mesh_shard_file_name,
    static_mesh_shard_relative_path,
};
pub use manifest::{
    GENERATION_REPORT_VERSION, GenerationReport, GenerationReportData, GenerationWarning, load_generation_report_data,
};

pub(crate) use manifest::{build_generation_report_data, write_generation_report_data};
