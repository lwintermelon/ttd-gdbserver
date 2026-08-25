//! Execution control and basic register/memory reads against a real trace.

mod common;

use ttd_gdbserver::target::{DebugTarget, StopReason};

use common::open_trace;

#[test]
#[cfg_attr(not(has_ttd_trace), ignore = "requires TTD_TRACE_PATH")]
fn lifetime_is_well_formed() {
    let process = open_trace();
    let (first, last) = process.lifetime();
    assert!(first.sequence() <= last.sequence());
    assert!(first.sequence() > 0 || first.steps() > 0);
}

#[test]
#[cfg_attr(not(has_ttd_trace), ignore = "requires TTD_TRACE_PATH")]
fn step_forward() {
    let mut process = open_trace();
    let pos_before = process.position();
    let reason = process.step().unwrap();
    let pos_after = process.position();

    assert!(matches!(reason, StopReason::StepComplete));
    assert_ne!(pos_before, pos_after);
}

#[test]
#[cfg_attr(not(has_ttd_trace), ignore = "requires TTD_TRACE_PATH")]
fn step_backward() {
    let mut process = open_trace();
    process.step().unwrap(); // move forward first
    process.step().unwrap();
    let pos_before = process.position();

    let reason = process.step_back().unwrap();
    let pos_after = process.position();

    assert!(matches!(reason, StopReason::StepComplete));
    assert!(pos_after <= pos_before);
}

#[test]
#[cfg_attr(not(has_ttd_trace), ignore = "requires TTD_TRACE_PATH")]
fn continue_forward_reaches_end() {
    let mut process = open_trace();
    // No breakpoints are registered, so a forward continue must run to the
    // recorded trace boundary. Asserting the reason (not just "it returned")
    // keeps a silent early stop or a mapped error from passing this test.
    // A recording that ends in a process exit reports `ProcessExit` there;
    // both mean "the continue was not cut short", so both are accepted.
    let reason = process.continue_forward().unwrap();
    assert!(
        matches!(reason, StopReason::TraceEnd | StopReason::ProcessExit),
        "an unbroken continue must stop at the trace end, got {reason:?}"
    );
    assert_eq!(process.position(), process.lifetime().1);
}

#[test]
#[cfg_attr(not(has_ttd_trace), ignore = "requires TTD_TRACE_PATH")]
fn read_memory_at_first_module() {
    let process = open_trace();
    let modules = process.modules();
    assert!(!modules.is_empty(), "trace should have modules");

    let base = modules[0].base_addr;
    let mut buf = [0u8; 16];
    let n = process.read_memory(base, &mut buf);
    assert!(
        n > 0,
        "should read at least 1 byte at module base {base:#x}"
    );
}

#[test]
#[cfg_attr(not(has_ttd_trace), ignore = "requires TTD_TRACE_PATH")]
fn read_memory_reports_where_each_run_came_from() {
    let process = open_trace();
    let (regs, _teb) = process.thread_state(None).expect("current thread state");
    let mut buf = [0u8; 16];
    let (n, ranges) = process.read_memory_ranges(regs.Rip, &mut buf, 8);
    assert!(n > 0, "the stopped thread's own code must be readable");
    assert!(
        !ranges.is_empty(),
        "TTD reports the position each run of bytes came from"
    );

    // The runs partition the returned bytes: no gaps, no overlaps, nothing
    // outside the buffer.
    let mut offset = 0u64;
    for r in &ranges {
        assert_eq!(
            r.Address - regs.Rip,
            offset,
            "runs must be contiguous and in order: {ranges:?}"
        );
        offset += r.size();
    }
    assert_eq!(
        offset, n as u64,
        "runs must cover exactly the bytes read: {ranges:?}"
    );

    // A read never reaches into the future: every data line it used sits at
    // or before the cursor's position. (The converse does not hold — an old
    // data line is still the correct value for a location nothing wrote.)
    let current = process.position().sequence();
    for r in &ranges {
        assert!(
            r.Sequence <= current,
            "run {r:?} came from beyond the current position {current:#x}"
        );
    }
}

#[test]
#[cfg_attr(not(has_ttd_trace), ignore = "requires TTD_TRACE_PATH")]
fn read_registers_via_thread_state() {
    let process = open_trace();
    let (regs, _teb) = process
        .thread_state(None)
        .expect("current thread state should be available");
    assert!(regs.Rip > 0, "RIP should be non-zero");
    assert!(regs.Rsp > 0, "RSP should be non-zero");
}
