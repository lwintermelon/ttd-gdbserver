//! Range stepping (`vCont;r` / Delve `next`) fast-path contracts.

use crate::support::*;

/// Range stepping is Delve's `next` hot path. The backend must read the PC
/// from the step cursor it is already using, not issue a persistent-cursor
/// query after every instruction: the optimized path costs no extra cursor
/// seeks and no extra query-cursor reads, so a source-level `next` no longer
/// replays from a keyframe per instruction.
#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn range_step_reuses_step_cursor_without_query_seeks() {
    let mut process = open();

    // Prime the step cursor, then warm the persistent cursor at the same
    // position via the normal register query so the baseline contains exactly
    // the seeks a real stop would already have paid for.
    process.step().unwrap();
    let tid = process
        .current_thread_id()
        .expect("step must leave a current thread");
    let pc = current_regs(&process).rip;

    let before = process.stats();
    let reason = process
        .step_range(Some(tid), pc, pc.saturating_add(1))
        .expect("range step");
    let after = process.stats();

    assert!(
        matches!(reason, StopReason::StepComplete),
        "range step must stop with StepComplete, got {reason:?}"
    );
    assert_eq!(
        after.steps,
        before.steps + 1,
        "a one-byte range must leave the range after one instruction: {after:?}"
    );
    assert_eq!(
        after.cursor_seeks, before.cursor_seeks,
        "range step must not seek either cursor after priming: {after:?}"
    );
    assert_eq!(
        after.queries, before.queries,
        "range step must read the PC from the step cursor, not query the persistent one: {after:?}"
    );
}

/// Range stepping is watchpoint-free: a client breakpoint at the current PC
/// must not stop the range loop. This mirrors GDB single-step semantics and
/// also proves the optimized `step_range` path did not accidentally start
/// using the persistent cursor (the one that carries the client watchpoints).
#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn range_step_ignores_a_breakpoint_at_the_current_pc() {
    let mut process = open();
    let pc = current_regs(&process).rip;
    let id = process.set_breakpoint(pc, None).unwrap();

    let reason = process
        .step_range(None, pc, pc.saturating_add(1))
        .expect("range step");
    assert!(
        matches!(reason, StopReason::StepComplete),
        "a breakpoint inside the range must not stop range stepping, got {reason:?}"
    );
    assert!(
        process.position() > process.lifetime().0,
        "range step must still advance the trace"
    );

    assert!(process.remove_breakpoint(id));
}
