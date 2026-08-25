//! Logical breakpoints mapped onto physical TTD watchpoints.

use crate::support::*;

#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn set_and_remove_breakpoint() {
    let mut process = open();
    let pc = current_regs(&process).rip;
    let id = process.set_breakpoint(pc, None).unwrap();
    assert!(id > 0);
    assert!(process.remove_breakpoint(id));
}

#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn set_data_breakpoint() {
    let mut process = open();
    let sp = current_regs(&process).rsp;
    let id = process
        .set_data_breakpoint(sp, 4, BreakpointKind::Write)
        .unwrap();
    assert!(id > 0);
    assert!(process.remove_breakpoint(id));
}

/// Regression test for the data-watchpoint access-type bug: the C++ shim used
/// to surface the SDK's `DataAccessType` *enum* value (Read=0, Write=1,
/// Execute=2) in the watchpoint callback, while the Rust matcher compared
/// against a `DataAccessMask` *bitmask* (Write=1<<1). A write watchpoint
/// therefore never matched — `(bp.access & access)` was `(2 & 1) == 0`.
///
/// This test drives the real continue path: a write watchpoint on the stack
/// slot where the next call stores its return address must produce a
/// `StopReason::Watchpoint` stop (previously it fell through to the trace
/// end).
#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn data_watchpoint_write_fires_on_continue() {
    let mut process = open();
    let regs = current_regs(&process);
    // The return-address slot for the next `call` (rsp-8). The replay engine
    // speculatively writes it as soon as a call executes.
    let target = regs.rsp - 8;
    let id = process
        .set_data_breakpoint(target, 8, BreakpointKind::Write)
        .unwrap();
    assert!(id > 0, "watchpoint id must be non-zero");

    let reason = process.continue_forward().unwrap();
    assert!(
        matches!(reason, StopReason::Watchpoint { .. }),
        "write watchpoint on the stack slot must stop the continue, got {reason:?}"
    );
    assert!(
        process.position() > process.lifetime().0,
        "must stop at the write, not at the trace start"
    );
    assert!(process.remove_breakpoint(id));
}

/// The watchpoint callback must honor per-thread filters: a breakpoint
/// registered for a thread that never executes the address must not fire,
/// while a global breakpoint at the same address does. (The continue-time
/// callback now routes through `find_by_watchpoint_hit_and_thread`; before
/// the fix it used the any-thread matcher.)
#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn thread_filtered_breakpoint_does_not_fire_on_other_thread() {
    let mut process = open();
    let pc = current_regs(&process).rip;
    let gid = process.set_breakpoint(pc, None).unwrap();
    // Restrict a second breakpoint to a thread that will never run this code
    // (0xdeadbeef is not a live OS tid in this trace).
    let btid = process.set_breakpoint(pc, Some(0xdead_beef)).unwrap();

    let reason = process.continue_forward().unwrap();
    if let StopReason::Breakpoint { bp_id, .. } = reason {
        assert_eq!(
            bp_id, gid,
            "global breakpoint must fire; the other-thread breakpoint (id {btid}) must not"
        );
    }
    process.remove_breakpoint(gid);
    process.remove_breakpoint(btid);
}

/// Two logical breakpoints at the same `(addr, size, access)` collapse onto
/// one physical TTD watchpoint. Removing one id must keep that watchpoint
/// registered (the survivor still fires); removing the other must remove the
/// physical watchpoint, and a later continue must run all the way to the
/// trace end without stopping.
///
/// `TargetStats::watchpoint_adds`/`watchpoint_removes` make the physical
/// transition observable directly, so this test also fails if the sync starts
/// issuing duplicate `AddMemoryWatchpoint`/`RemoveMemoryWatchpoint` calls.
#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn duplicate_same_address_breakpoints_dedupe_physical_watchpoint() {
    let mut process = open();
    let start = process.lifetime().0;
    let regs = current_regs(&process);
    // The return-address slot the next call writes (same target as the
    // single-watchpoint regression test above).
    let target = regs.rsp - 8;

    let before = process.stats();
    let first_id = process
        .set_data_breakpoint(target, 8, BreakpointKind::Write)
        .unwrap();
    let after_first = process.stats();
    assert_eq!(
        after_first.watchpoint_adds,
        before.watchpoint_adds + 1,
        "the first logical breakpoint must register one physical watchpoint: {after_first:?}"
    );

    let second_id = process
        .set_data_breakpoint(target, 8, BreakpointKind::Write)
        .unwrap();
    assert_ne!(first_id, second_id, "logical ids must remain distinct");
    let after_second = process.stats();
    assert_eq!(
        after_second.watchpoint_adds, after_first.watchpoint_adds,
        "a duplicate logical breakpoint must not add a second physical watchpoint: {after_second:?}"
    );

    // First hit: with two matching logical breakpoints either id may be
    // reported (both are global and cover the same range).
    let first_hit = process.continue_forward().unwrap();
    let first_hit_id = match first_hit {
        StopReason::Watchpoint { bp_id, .. } => bp_id,
        other => panic!("write watchpoint must stop the continue, got {other:?}"),
    };
    assert!(
        first_hit_id == first_id || first_hit_id == second_id,
        "hit id {first_hit_id} must be one of the two registered ids"
    );
    let (removed, survivor) = if first_hit_id == first_id {
        (first_id, second_id)
    } else {
        (second_id, first_id)
    };

    // Delete one of the two ids: the physical watchpoint must stay applied
    // because the survivor still needs it.
    assert!(process.remove_breakpoint(removed));
    let after_remove_one = process.stats();
    assert_eq!(
        after_remove_one.watchpoint_removes, before.watchpoint_removes,
        "removing one of two ids at the same address must not remove the physical watchpoint: {after_remove_one:?}"
    );

    process.goto(start).unwrap();
    let second_hit = process.continue_forward().unwrap();
    match second_hit {
        StopReason::Watchpoint { bp_id, .. } => assert_eq!(
            bp_id, survivor,
            "the surviving logical breakpoint must still stop the replay"
        ),
        other => panic!("surviving watchpoint must still fire, got {other:?}"),
    }

    // Delete the survivor: now the physical watchpoint is removed and the
    // next continue reaches the trace boundary without a watchpoint stop.
    assert!(process.remove_breakpoint(survivor));
    let after_remove_all = process.stats();
    assert_eq!(
        after_remove_all.watchpoint_removes,
        before.watchpoint_removes + 1,
        "removing the last id must remove the physical watchpoint exactly once: {after_remove_all:?}"
    );

    process.goto(start).unwrap();
    let after_all_removed = process.continue_forward().unwrap();
    assert!(
        matches!(after_all_removed, StopReason::TraceEnd),
        "with both logical breakpoints gone the continue must reach the trace end, got {after_all_removed:?}"
    );
}
