//! Trace position, module list, reverse execution, and goto.

mod common;

use ttd_gdbserver::target::{DebugTarget, StopReason};

use common::open_trace;

#[test]
#[cfg_attr(not(has_ttd_trace), ignore = "requires TTD_TRACE_PATH")]
fn position_and_lifetime() {
    let process = open_trace();
    let pos = process.position();
    let (first, last) = process.lifetime();

    assert!(pos.sequence() > 0 || pos.steps() > 0);
    assert!(first <= pos);
    assert!(pos <= last);
}

#[test]
#[cfg_attr(not(has_ttd_trace), ignore = "requires TTD_TRACE_PATH")]
fn modules() {
    let process = open_trace();
    let modules = process.modules();
    assert!(!modules.is_empty(), "trace should have at least one module");
    assert!(!modules[0].name.is_empty(), "module should have a name");
    assert!(modules[0].base_addr > 0, "module base should be non-zero");

    let main = process
        .main_module()
        .expect("a recorded process has a main module");
    assert_eq!(
        (main.base_addr, main.name.as_str()),
        (modules[0].base_addr, modules[0].name.as_str()),
        "main_module must name the same executable as modules()[0]"
    );
    assert!(
        modules.len() > 1,
        "the recording must also contain the Windows loader modules: {modules:?}"
    );
    // exec-file, auxv and qOffsets all treat the first module as the program;
    // it must be a PE executable, not one of the loader DLLs.
    assert!(
        modules[0].name.to_ascii_lowercase().ends_with(".exe"),
        "the first module must be the recorded executable, got {}",
        modules[0].name
    );
}

#[test]
#[cfg_attr(not(has_ttd_trace), ignore = "requires TTD_TRACE_PATH")]
fn continue_backward_to_start() {
    let mut process = open_trace();
    // First step forward a bit
    for _ in 0..3 {
        process.step().unwrap();
    }
    let pos_mid = process.position();

    let reason = process.continue_backward().unwrap();
    let pos_after = process.position();
    assert!(
        matches!(reason, StopReason::TraceStart),
        "an unbroken reverse continue must stop at the trace start, got {reason:?}"
    );
    assert_eq!(
        pos_after,
        process.lifetime().0,
        "the cursor must be at the trace start"
    );
    assert!(pos_after <= pos_mid, "reverse continue must move backward");
}

#[test]
#[cfg_attr(not(has_ttd_trace), ignore = "requires TTD_TRACE_PATH")]
fn goto_position() {
    let mut process = open_trace();
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
#[cfg_attr(not(has_ttd_trace), ignore = "requires TTD_TRACE_PATH")]
fn goto_repositions_persistent_cursor() {
    let mut process = open_trace();

    // Register at the initial position: the current instruction's first byte
    // at the starting PC.
    let start_pos = process.position();
    let (regs, _) = process.thread_state(None).expect("current thread state");
    let start_pc = regs.Rip;
    let mut first = [0u8; 1];
    assert!(
        process.read_memory(start_pc, &mut first) > 0,
        "memory at the start PC must be readable"
    );

    // Walk forward until the PC moves off the start instruction (so the two
    // positions are observably different in memory terms).
    let mut moved_pc = start_pc;
    for _ in 0..64 {
        process.step().unwrap();
        let (regs, _) = process.thread_state(None).expect("current thread state");
        if regs.Rip != start_pc {
            moved_pc = regs.Rip;
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

    // Jump back to the start position. A `goto` must reposition the
    // persistent cursor, not just the bookkeeping: the next memory read (the
    // first packet a client sends after a restart) has to observe the start
    // position's bytes again.
    process.goto(start_pos).unwrap();
    assert_eq!(process.position(), start_pos);
    let mut again = [0u8; 1];
    assert!(
        process.read_memory(start_pc, &mut again) > 0,
        "memory at the start PC must still be readable after goto"
    );
    assert_eq!(
        again, first,
        "goto must reposition the persistent cursor back to the start position"
    );
}

/// `exceptions()` must agree with the count reported in `diagnostics()`, and
/// every event must carry a non-zero exception code at a valid position.
#[test]
#[cfg_attr(not(has_ttd_trace), ignore = "requires TTD_TRACE_PATH")]
fn exception_list_matches_diagnostics_count() {
    let process = open_trace();
    let events = process.exceptions();
    assert_eq!(
        u32::try_from(events.len()).expect("exception count fits u32"),
        process.diagnostics().exception_count,
        "exception list and diagnostics must agree"
    );
    for ev in &events {
        assert!(ev.code != 0, "exception code must be non-zero: {ev:?}");
        assert!(
            ev.position.sequence() > 0 || ev.position.steps() > 0,
            "exception position must be valid: {ev:?}"
        );
    }
}
