//! Asset directory maps that track normalized path keys to loose-file or BSA sources.

use std::fs::{self, DirEntry, Metadata};
use std::hash::{BuildHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, ensure};
use indexmap::map::raw_entry_v1::RawEntryApiV1;
use rayon::prelude::*;

use distantland_foundation::IndexMap;
use tracing::warn;

mod key;
mod map;
mod scan;

#[cfg(test)]
pub(crate) use crate::normalize::trim_normalized_prefix;
pub(crate) use crate::normalize::{NormalizedStr, NormalizedString, is_normalized, normalize};
pub(crate) use key::*;
pub use map::*;
pub use scan::build_directory_map;
pub(crate) use scan::*;

#[cfg(test)]
mod tests;
