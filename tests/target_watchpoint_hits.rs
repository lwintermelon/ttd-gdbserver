//! What a client observes at a watchpoint stop, against a real trace.
//!
//! GDB semantics: a write watchpoint stops *after* the write, so the new value
//! must be visible at the stop and the old one reachable by reversing. TTD
//! reports the hit at the instruction that performed the access, and a cursor
//! placed there sees the post-write state — which is what the frontend serves.
//!
//! Requires `TTD_TRACE_PATH`.

mod common;

use common::open_trace;
use ttd_gdbserver::target::{BreakpointKind, DebugTarget, StopReason};

fn read_u64(process: &dyn DebugTarget, addr: u64) -> u64 {
    let mut buf = [0u8; 8];
    assert_eq!(process.read_memory(addr, &mut buf), 8, "read {addr:#x}");
    u64::from_le_bytes(buf)
}

/// The stack slot the next `call` overwrites: a write watchpoint there stops
/// at the call, and the slot already holds the pushed return address.
#[test]
#[cfg_attr(not(has_ttd_trace), ignore = "requires TTD_TRACE_PATH")]
fn write_watchpoint_stop_shows_the_new_value() {
    let mut process = open_trace();
    let (regs, _) = process.thread_state(None).expect("current thread state");
    let target = regs.Rsp - 8;

    let before = read_u64(&process, target);
    let bp = process
        .set_data_breakpoint(target, 8, BreakpointKind::Write)
        .expect("write watchpoint");
    let reason = process.continue_forward().expect("continue");
    assert!(
        matches!(reason, StopReason::Watchpoint { .. }),
        "expected a write-watchpoint stop, got {reason:?}"
    );

    let at_stop = read_u64(&process, target);
    assert_ne!(
        at_stop, before,
        "the stop must follow the write, not precede it"
    );

    // Reversing one instruction undoes the write, so the old value is
    // reachable — the property a hardware watchpoint cannot offer.
    process.step_back().expect("step back over the write");
    let after_back = read_u64(&process, target);
    assert_eq!(
        after_back, before,
        "stepping back over the write must restore the previous value"
    );

    assert!(process.remove_breakpoint(bp));
}

/// A watchpoint stop keeps the *values* reachable in both directions: forward
/// again re-applies the write.
#[test]
#[cfg_attr(not(has_ttd_trace), ignore = "requires TTD_TRACE_PATH")]
fn write_watchpoint_round_trips() {
    let mut process = open_trace();
    let (regs, _) = process.thread_state(None).expect("current thread state");
    let target = regs.Rsp - 8;

    let bp = process
        .set_data_breakpoint(target, 8, BreakpointKind::Write)
        .expect("write watchpoint");
    let reason = process.continue_forward().expect("continue");
    assert!(
        matches!(reason, StopReason::Watchpoint { .. }),
        "{reason:?}"
    );
    let new_value = read_u64(&process, target);
    let stop_position = process.position();

    process.step_back().expect("step back");
    assert_ne!(read_u64(&process, target), new_value);

    process.step().expect("step forward");
    assert_eq!(
        read_u64(&process, target),
        new_value,
        "replaying forward to the same position must show the same value"
    );
    assert!(
        process.position() >= stop_position,
        "the position must not move backwards on a forward step"
    );

    assert!(process.remove_breakpoint(bp));
}
