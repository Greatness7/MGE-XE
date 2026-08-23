//! Targeted static-output equivalence support for integration tests.

mod compare;
mod current;
mod logical;
mod snapshot;

pub use snapshot::assert_static_outputs_equivalent;
