//! Terrain source-texture sampling, DDS writers, and per-cell texture cache helpers.

#[cfg(test)]
use std::fs::File;
#[cfg(test)]
use std::io::BufWriter;
use std::io::Write;
#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::sync::Arc;

use glam::{Vec2, Vec3, Vec4};
use image::RgbaImage;
use smallvec::SmallVec;
use tracing::info_span;

pub use crate::usage::{DEFAULT_LAND_TEXTURE, TerrainCell, TerrainCells, TerrainTextureTable, default_land_texture_key};
use crate::vfs::Vfs;
use crate::{IndexMap, IndexSet};
use tracing::*;

mod cell;
mod color;
mod dds;
mod sampler;
mod texture_cache;

pub use cell::*;
pub use color::*;
pub use dds::*;
pub use sampler::*;
pub use texture_cache::*;

#[cfg(test)]
use dds::rgba8_dds_capacity_hint;
#[cfg(test)]
mod tests;
