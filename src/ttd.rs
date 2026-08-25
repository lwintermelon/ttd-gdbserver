//! TTD Replay API bindings and safe handles.
//!
//! - `bindings.rs` — bindgen output for the SDK enums/constants.
//! - `ffi.rs` — bindgen output for the C shim's projection types + functions.
//! - `engine.rs` — `TtdEngine`, owner of the C++ replay engine.
//! - `cursor.rs` — `TtdCursor` and the safe callback-slot wrappers.
//! - `types.rs` / `error.rs` — re-exported projection types with extra trait
//!   impls, and the FFI error enum.
//!
//! Layout is `ttd.rs` + `ttd/` (no `mod.rs`).

pub mod bindings;
pub mod cursor;
pub mod engine;
pub mod error;
pub mod ffi;
pub mod types;

pub use cursor::TtdCursor;
pub use engine::TtdEngine;
pub use error::TtdError;
pub use types::*;
