//! Integration tests for the target layer: `TtdProcess` via the `DebugTarget` trait.
//!
//! These test the target layer directly (no protocol frontend),
//! complementing the full-chain GDB RSP tests in `gdb_ttd.rs`.
//!
//! Requires `TTD_TRACE_PATH`. Tests auto-skip when not set.
//!
//! ```bash
//! TTD_TRACE_PATH=path/to/trace.run cargo test --test target -- --nocapture
//! ```

use std::path::Path;

use ttd_gdbserver::target::{BreakpointKind, DebugTarget, StopReason, TtdProcess};
use ttd_gdbserver::ttd::types::TtdX64Regs;

fn trace_path() -> Option<String> {
    std::env::var("TTD_TRACE_PATH").ok()
}

fn open() -> Option<TtdProcess> {
    let path = trace_path()?;
    TtdProcess::open(Path::new(&path)).ok()
}

/// Registers of the current thread.
fn current_regs(process: &TtdProcess) -> Option<TtdX64Regs> {
    Some(process.thread_state(None)?.0)
}

// ─── Lifecycle ────────────────────────────────────────────────

#[test]
fn open_trace() {
    let Some(engine) = open() else {
        return;
    };
    let (first, last) = engine.lifetime();
    assert!(first.sequence <= last.sequence);
    assert!(first.sequence > 0 || first.steps > 0);
}

// ─── Execution Control ────────────────────────────────────────

#[test]
fn step_forward() {
    let Some(mut engine) = open() else {
        return;
    };
    let pos_before = engine.position();
    let reason = engine.step().unwrap();
    let pos_after = engine.position();

    assert!(matches!(reason, StopReason::StepComplete));
    assert_ne!(pos_before, pos_after);
}

#[test]
fn step_backward() {
    let Some(mut engine) = open() else {
        return;
    };
    engine.step().unwrap(); // move forward first
    engine.step().unwrap();
    let pos_before = engine.position();

    let reason = engine.step_back().unwrap();
    let pos_after = engine.position();

    assert!(matches!(reason, StopReason::StepComplete));
    assert!(pos_after <= pos_before);
}

#[test]
fn continue_forward_reaches_end() {
    let Some(mut engine) = open() else {
        return;
    };
    // Continue to end — should stop at TraceEnd or hit a breakpoint
    let reason = engine.continue_forward().unwrap();
    // Just verify it doesn't panic; the exact reason depends on the trace
    let _ = reason;
}

// ─── Memory ───────────────────────────────────────────────────

#[test]
fn read_memory_at_first_module() {
    let Some(engine) = open() else {
        return;
    };
    let modules = engine.modules();
    assert!(!modules.is_empty(), "trace should have modules");

    let base = modules[0].base_addr;
    let mut buf = [0u8; 16];
    let n = engine.read_memory(base, &mut buf);
    assert!(
        n > 0,
        "should read at least 1 byte at module base {:#x}",
        base
    );
}

// ─── Registers ────────────────────────────────────────────────

#[test]
fn read_registers_via_thread_state() {
    let Some(engine) = open() else {
        return;
    };
    let (regs, _teb) = engine
        .thread_state(None)
        .expect("current thread state should be available");
    assert!(regs.rip > 0, "RIP should be non-zero");
    assert!(regs.rsp > 0, "RSP should be non-zero");
}

// ─── Threads ──────────────────────────────────────────────────

#[test]
fn active_threads_with_state() {
    let Some(engine) = open() else {
        return;
    };
    let ids = engine.active_thread_ids();
    assert!(!ids.is_empty());
    let (regs, _teb) = engine
        .thread_state(Some(ids[0]))
        .expect("active thread state should be available");
    assert!(regs.rip > 0);
}

#[test]
fn current_thread() {
    let Some(engine) = open() else {
        return;
    };
    assert!(
        engine.current_thread_id().is_some(),
        "should have a current thread"
    );
}

// ─── Position ─────────────────────────────────────────────────

#[test]
fn position_and_lifetime() {
    let Some(engine) = open() else {
        return;
    };
    let pos = engine.position();
    let (first, last) = engine.lifetime();

    assert!(pos.sequence > 0 || pos.steps > 0);
    assert!(first <= pos);
    assert!(pos <= last);
}

// ─── Modules ──────────────────────────────────────────────────

#[test]
fn modules() {
    let Some(engine) = open() else {
        return;
    };
    let modules = engine.modules();
    assert!(!modules.is_empty(), "trace should have at least one module");
    assert!(!modules[0].name.is_empty(), "module should have a name");
    assert!(modules[0].base_addr > 0, "module base should be non-zero");
}

// ─── Breakpoints ──────────────────────────────────────────────

#[test]
fn set_and_remove_breakpoint() {
    let Some(mut engine) = open() else {
        return;
    };
    let pc = current_regs(&engine).unwrap().rip;
    let id = engine.set_breakpoint(pc, None);
    assert!(id > 0);
    assert!(engine.remove_breakpoint(id));
}

#[test]
fn set_data_breakpoint() {
    let Some(mut engine) = open() else {
        return;
    };
    let sp = current_regs(&engine).unwrap().rsp;
    let id = engine.set_data_breakpoint(sp, 4, BreakpointKind::Write);
    assert!(id > 0);
    assert!(engine.remove_breakpoint(id));
}

// ─── Reverse execution ────────────────────────────────────────

#[test]
fn continue_backward_to_start() {
    let Some(mut engine) = open() else {
        return;
    };
    // First step forward a bit
    for _ in 0..3 {
        engine.step().unwrap();
    }
    let pos_mid = engine.position();

    let reason = engine.continue_backward().unwrap();
    let pos_after = engine.position();

    // Should be at or before the mid-point
    assert!(pos_after <= pos_mid);
    let _ = reason;
}

#[test]
fn goto_position() {
    let Some(mut engine) = open() else {
        return;
    };
    let pos_before = engine.position();
    engine.step().unwrap();
    engine.step().unwrap();

    let reason = engine.goto(pos_before).unwrap();
    assert!(matches!(reason, StopReason::PositionReached));
    assert_eq!(engine.position(), pos_before);
}
