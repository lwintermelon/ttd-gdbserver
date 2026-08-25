//! Shared support for the integration tests.
//!
//! Integration tests are separate crates, so they cannot share a module
//! unless each one declares it. Every test crate under `tests/` therefore
//! starts with `mod common;`, and this is the standard `tests/common/mod.rs`
//! layout: Cargo treats a `tests/` entry as a test target only when it is a
//! `.rs` file or a directory holding `main.rs`, so this file is compiled into
//! each test binary instead of becoming one of its own.
//!
//! - [`rsp`] — GDB RSP wire client and Delve/custom-packet helpers
//! - [`harness`] — one-session TCP server startup + Delve handshake
//! - [`mock_target`] — configurable in-memory [`DebugTarget`]
//!
//! Test crates import what they use straight from these modules rather than
//! through a glob re-export, because each binary uses only a slice of the
//! surface and a re-export would warn as unused in every other one.
//!
//! [`DebugTarget`]: ttd_gdbserver::target::DebugTarget

// Each test binary links only the slice of this surface it uses, so anything
// the current binary ignores is not dead code — it belongs to another one.
#![allow(dead_code)]

pub mod harness;
pub mod mock_target;
pub mod rsp;

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use ttd_gdbserver::target::{DebugTarget, TtdProcess};
use ttd_gdbserver::ttd::types::{TtdPosition, TtdX64Regs};
use ttd_gdbserver::ttd::{ReplayCursor, ReplayEngine};

/// Serialize trace opens so the first run cannot race several `BuildIndex`
/// calls for the same `.idx` file when libtest runs a suite's tests in
/// parallel. Once the index exists this is cheap; it only covers the open,
/// not the subsequent (independent) cursors.
static TRACE_OPEN_LOCK: Mutex<()> = Mutex::new(());

fn lock_trace_open() -> MutexGuard<'static, ()> {
    TRACE_OPEN_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The trace the Rust integration tests run against: `TTD_TRACE_PATH`.
///
/// There is deliberately no built-in default path — a recording's location is
/// machine/deployment configuration, not source. `mise.toml`'s `[env]` block
/// supplies the corpus's recording (so `mise run test` works anywhere a clone
/// has its local traces); CI points the variable wherever its trace lives, or
/// leaves it unset and the suite reports those tests as `ignored`.
///
/// Every test that calls this is tagged
/// `#[cfg_attr(not(has_ttd_trace), ignore)]`, and `build.rs` sets that cfg
/// from the same variable, so reaching the `expect` is a harness bug rather
/// than a skip condition.
pub fn trace_path() -> String {
    std::env::var("TTD_TRACE_PATH")
        .ok()
        .filter(|value| !value.is_empty())
        .expect("TTD_TRACE_PATH must be set (see mise.toml [env] or regression/README.md)")
}

/// The trace the exception tests run against: `TTD_FAULT_TRACE_PATH`.
///
/// A second variable rather than a second directory convention: the exception
/// tests need a recording that actually contains an exception, and which file
/// that is belongs to the environment.
pub fn fault_trace_path() -> String {
    std::env::var("TTD_FAULT_TRACE_PATH")
        .ok()
        .filter(|value| !value.is_empty())
        .expect("TTD_FAULT_TRACE_PATH must be set (see mise.toml [env])")
}

/// Open the configured trace through the public target API.
pub fn open_trace() -> TtdProcess {
    let _guard = lock_trace_open();
    TtdProcess::open(Path::new(&trace_path())).expect("open the canonical trace")
}

/// Open the fault trace (one recorded hardware access violation).
pub fn open_fault_trace() -> TtdProcess {
    let _guard = lock_trace_open();
    TtdProcess::open(Path::new(&fault_trace_path())).expect("open the fault trace")
}

/// Create a loaded TTD engine from the configured trace.
pub fn create_engine() -> ReplayEngine {
    let _guard = lock_trace_open();
    ReplayEngine::open(Path::new(&trace_path())).expect("create engine and load trace")
}

/// Position a raw cursor at `pos`.
pub fn seek(cursor: &mut ReplayCursor, pos: TtdPosition) {
    cursor.set_position(pos);
}

/// Read up to `buf.len()` bytes into `buf`, returning how many were read.
pub fn read_memory(cursor: &ReplayCursor, addr: u64, buf: &mut [u8]) -> usize {
    if buf.is_empty() {
        return 0;
    }
    match cursor.read_memory(addr, buf.len()) {
        Ok(bytes) => {
            let n = bytes.len().min(buf.len());
            buf[..n].copy_from_slice(&bytes[..n]);
            n
        }
        Err(_) => 0,
    }
}

/// Registers of the current thread.
pub fn current_regs(process: &TtdProcess) -> TtdX64Regs {
    process.thread_state(None).expect("current thread state").0
}

/// Registers of a thread that is not part of the trace at all.
pub const FOREIGN_TID: u64 = 0xdead_beef;
