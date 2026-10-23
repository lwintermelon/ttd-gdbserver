//! TTD Replay API bindings and safe handles.
//!
//! - `ffi.rs` — the cxx bridge to the C++ wrapper in `ttd/cpp/ttd_cxx.*`. Raw,
//!   and private to this module: everything callers use is re-exported here.
//! - `engine.rs` — [`ReplayEngine`], owner of the SDK replay engine.
//! - `cursor.rs` — [`ReplayCursor`], positions, memory, threads, callbacks.
//! - `callback.rs` — panic escalation for callbacks crossing the FFI boundary.
//! - `types.rs` / `error.rs` — the value vocabulary and the FFI error enum.
//!
//! **This module is the only place in the crate allowed to use `unsafe`.**

pub mod error;
pub mod types;

mod callback;
mod cursor;
mod engine;
mod ffi;
mod interrupt;

pub use cursor::{ActiveThread, ReplayCursor};
pub use engine::ReplayEngine;
pub use error::TtdError;
pub use types::*;

#[cfg(test)]
pub(crate) use types::memory_range;

pub(crate) use interrupt::InterruptSlot;
