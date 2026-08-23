//! Fixed Morrowind world-space constants shared by every crate that maps cells to engine units.
//!
//! These are properties of the game engine and its LAND record layout, not tunables. They live
//! here because `formats` is the workspace's dependency root, so terrain, usage, and statics can
//! all reach them.

/// The length of one exterior cell side, in engine units.
pub const LAND_CELL_SIZE: f32 = 8192.0;
