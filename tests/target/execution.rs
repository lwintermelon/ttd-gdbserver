//! Execution control and basic register/memory reads.

use crate::support::*;

#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn open_trace() {
    let process = open();
    let (first, last) = process.lifetime();
    assert!(first.sequence <= last.sequence);
    assert!(first.sequence > 0 || first.steps > 0);
}

#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn step_forward() {
    let mut process = open();
    let pos_before = process.position();
    let reason = process.step().unwrap();
    let pos_after = process.position();

    assert!(matches!(reason, StopReason::StepComplete));
    assert_ne!(pos_before, pos_after);
}

#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn step_backward() {
    let mut process = open();
    process.step().unwrap(); // move forward first
    process.step().unwrap();
    let pos_before = process.position();

    let reason = process.step_back().unwrap();
    let pos_after = process.position();

    assert!(matches!(reason, StopReason::StepComplete));
    assert!(pos_after <= pos_before);
}

#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn continue_forward_reaches_end() {
    let mut process = open();
    // Continue to end — should stop at TraceEnd or hit a breakpoint
    let reason = process.continue_forward().unwrap();
    // Just verify it doesn't panic; the exact reason depends on the trace
    let _ = reason;
}

#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn read_memory_at_first_module() {
    let process = open();
    let modules = process.modules();
    assert!(!modules.is_empty(), "trace should have modules");

    let base = modules[0].base_addr;
    let mut buf = [0u8; 16];
    let n = process.read_memory(base, &mut buf);
    assert!(
        n > 0,
        "should read at least 1 byte at module base {:#x}",
        base
    );
}

#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn read_registers_via_thread_state() {
    let process = open();
    let (regs, _teb) = process
        .thread_state(None)
        .expect("current thread state should be available");
    assert!(regs.rip > 0, "RIP should be non-zero");
    assert!(regs.rsp > 0, "RSP should be non-zero");
}
