//! Shared support for `ttd-gdbserver`'s integration tests.
//!
//! Integration tests are separate crates and cannot share a `tests/common`
//! module without each one redeclaring it. This small helper crate is a
//! normal dev-dependency of the package instead, so every test target imports
//! the same compiled helpers:
//!
//! - [`rsp`] — GDB RSP wire client and Delve/custom-packet helpers
//! - [`harness`] — one-session TCP server startup + Delve handshake
//! - [`mock_target`] — configurable in-memory [`ttd_gdbserver::target::DebugTarget`]
//!
//! It depends back on `ttd-gdbserver` (a Cargo-allowed dev-dependency cycle).

use std::path::Path;

use ttd_gdbserver::ttd::TtdEngine;

pub mod harness;
pub mod mock_target;
pub mod rsp;

/// Create a loaded engine from the `TTD_TRACE_PATH` environment variable.
///
/// Set it to the `.run` file before running the trace-dependent suites:
/// `TTD_TRACE_PATH=path/to/file.run cargo test --test ttd`.
pub fn create_engine() -> TtdEngine {
    let path = std::env::var("TTD_TRACE_PATH")
        .expect("TTD_TRACE_PATH env var must be set to the .run trace file");
    TtdEngine::open(Path::new(&path)).expect("Failed to create engine and load trace")
}
