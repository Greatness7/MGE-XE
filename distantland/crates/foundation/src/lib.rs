//! Storage authority, output addressing, identities, and incremental-state primitives.

use hashbrown::DefaultHashBuilder;

pub use distantland_formats as mge_xe;

/// `IndexMap` configured with the workspace's trusted-input hash builder.
pub type IndexMap<K, V> = indexmap::IndexMap<K, V, DefaultHashBuilder>;
/// `IndexSet` configured with the workspace's trusted-input hash builder.
pub type IndexSet<V> = indexmap::IndexSet<V, DefaultHashBuilder>;

/// Publication permits and durable-file capabilities.
pub mod commit;
pub mod framed_archive;
pub mod identity;
/// Generated-output paths and version constants.
pub mod output;
/// Version-keyed, lock-pinned readers for committed distant-land output.
pub mod output_index;
/// Static-record addressing and packed value storage.
pub mod record_key;
/// Incremental generation-state tables.
pub mod state_db;
/// Complete-or-absent storage authority.
pub mod storage;
/// Unit keys and fingerprint writers.
pub mod units;
