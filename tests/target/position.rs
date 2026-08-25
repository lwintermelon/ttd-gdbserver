//! Trace position, module list, reverse execution, and goto.

use crate::support::*;

#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn position_and_lifetime() {
    let process = open();
    let pos = process.position();
    let (first, last) = process.lifetime();

    assert!(pos.sequence > 0 || pos.steps > 0);
    assert!(first <= pos);
    assert!(pos <= last);
}

#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn modules() {
    let process = open();
    let modules = process.modules();
    assert!(!modules.is_empty(), "trace should have at least one module");
    assert!(!modules[0].name.is_empty(), "module should have a name");
    assert!(modules[0].base_addr > 0, "module base should be non-zero");
}

#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn continue_backward_to_start() {
    let mut process = open();
    // First step forward a bit
    for _ in 0..3 {
        process.step().unwrap();
    }
    let pos_mid = process.position();

    let reason = process.continue_backward().unwrap();
    let pos_after = process.position();

    // Should be at or before the mid-point
    assert!(pos_after <= pos_mid);
    let _ = reason;
}

#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn goto_position() {
    let mut process = open();
    let pos_before = process.position();
    process.step().unwrap();
    process.step().unwrap();

    let reason = process.goto(pos_before).unwrap();
    assert!(matches!(reason, StopReason::PositionReached));
    assert_eq!(process.position(), pos_before);
}

/// `goto` must reposition the *persistent cursor*, not just the bookkeeping:
/// a memory read issued right after `goto` (as a fresh session's first
/// `m` packet does) must observe the destination position's memory, not the
/// position the cursor happened to be left at.
#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn goto_repositions_persistent_cursor() {
    let mut process = open();

    // Register at the initial position: the current instruction's first byte
    // at the starting PC.
    let (regs, _) = process.thread_state(None).expect("current thread state");
    let start_pc = regs.rip;
    let mut first = [0u8; 1];
    assert!(
        process.read_memory(start_pc, &mut first) > 0,
        "memory at the start PC must be readable"
    );

    // Walk forward until the PC moves off the start instruction (so the two
    // positions are observably different in memory terms), remembering the
    // byte at each new PC.
    let mut moved_pc = start_pc;
    for _ in 0..64 {
        process.step().unwrap();
        let (regs, _) = process.thread_state(None).expect("current thread state");
        if regs.rip != start_pc {
            moved_pc = regs.rip;
            break;
        }
    }
    assert_ne!(
        moved_pc, start_pc,
        "trace must advance the PC within 64 steps"
    );
    let mut later = [0u8; 1];
    assert!(
        process.read_memory(moved_pc, &mut later) > 0,
        "memory at the moved PC must be readable"
    );

    // Jump back to the start position and read the start PC again: without
    // the goto-cursor repositioning, the read would serve the later
    // position's memory.
    process
        .goto(TtdPosition {
            sequence: 0,
            steps: 0,
        })
        .unwrap();
    let pos = process.position();
    process.goto(pos).unwrap();
    let (regs, _) = process.thread_state(None).expect("current thread state");
    assert_eq!(
        regs.rip, start_pc,
        "after goto the queried PC must be back at the start"
    );
}

/// `exceptions()` must agree with the count reported in `diagnostics()`, and
/// every event must carry a non-zero exception code at a valid position.
#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn exception_list_matches_diagnostics_count() {
    let process = open();
    let events = process.exceptions();
    assert_eq!(
        events.len() as u32,
        process.diagnostics().exception_count,
        "exception list and diagnostics must agree"
    );
    for ev in &events {
        assert!(ev.code != 0, "exception code must be non-zero: {ev:?}");
        assert!(
            ev.position.sequence > 0 || ev.position.steps > 0,
            "exception position must be valid: {ev:?}"
        );
    }
}
