//! The complete-or-absent storage authority.
//!
//! Every write to a committed tree goes through `authority::WriterSession`, which holds the
//! exclusive writer lock, classifies the cache from `generation_state.bin`, and publishes one
//! complete state after all required artifacts are durable. `fault` names the boundaries where a
//! writer can be interrupted; the hooks are compiled into release builds so fault injection
//! exercises the production path.

/// Writer-session authority and cache classification.
pub mod authority;
pub mod capacity;
/// Durable write and synchronization primitives.
pub mod durable;
/// Product fault-injection hooks.
pub mod fault;
pub mod lock;
/// Canonical and superseded output-path classification.
pub mod path;
/// Committed generation-state persistence.
pub mod state;
