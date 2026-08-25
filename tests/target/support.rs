//! Shared helpers and imports for the target-layer integration tests.

pub use std::path::Path;

pub use ttd_gdbserver::target::{BreakpointKind, DebugTarget, StopReason, TtdProcess};
pub use ttd_gdbserver::ttd::TtdEngine;
pub use ttd_gdbserver::ttd::types::{TtdPosition, TtdX64Regs};

/// `TTD_TRACE_PATH` for the recorded trace under test.
pub fn trace_path() -> String {
    // The #[cfg_attr(not(has_ttd_trace), ignore)] tag guarantees this only
    // runs when TTD_TRACE_PATH is set; reaching this line without it is a
    // harness bug, not a skip condition.
    std::env::var("TTD_TRACE_PATH")
        .expect("TTD_TRACE_PATH must be set (test should have been #[ignore]d)")
}

/// Open the configured trace through the public target API.
pub fn open() -> TtdProcess {
    TtdProcess::open(Path::new(&trace_path())).expect("open trace (configured via TTD_TRACE_PATH)")
}

/// Registers of the current thread.
pub fn current_regs(process: &TtdProcess) -> TtdX64Regs {
    process.thread_state(None).expect("current thread state").0
}

/// Registers of a thread that is not part of the trace at all.
pub const FOREIGN_TID: u64 = 0xdead_beef;
