//! Shared DDS, texture decoding, resizing, and exact-deduplication primitives.

/// DDS encoding and mip-planning primitives.
pub mod dds;
/// Texture aliasing and exact-content deduplication.
pub mod texture_dedupe;
/// Texture decoding and resize rules.
pub mod texture_io;
