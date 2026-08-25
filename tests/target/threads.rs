//! Thread queries, cursor-position caching, and goto/step interaction.

use crate::support::*;

#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn active_threads_with_state() {
    let process = open();
    let ids = process.active_thread_ids();
    assert!(!ids.is_empty());
    let (regs, _teb) = process
        .thread_state(Some(ids[0]))
        .expect("active thread state should be available");
    assert!(regs.rip > 0);
}

/// TTD returns a zeroed context for threads it does not know; serving that as
/// "registers" (RIP = 0, RSP = 0) would make a stale thread id look like a
/// stopped-at-null thread instead of an error.
#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn thread_state_rejects_unknown_threads() {
    let process = open();
    assert!(
        process.thread_state(Some(FOREIGN_TID)).is_none(),
        "a tid that never existed in the trace must have no state"
    );
    // A live thread still resolves.
    let ids = process.active_thread_ids();
    assert!(!ids.is_empty());
    assert!(
        process.thread_state(Some(ids[0])).is_some(),
        "an active thread must have state"
    );
}

/// The persistent query cursor is positioned lazily while steps run on their
/// own cursor, so the two can drift. Cross-check against a raw TTD cursor
/// seeked to the same position — an oracle that shares none of `TtdProcess`'s
/// bookkeeping, so a stale cache shows up as a register/memory mismatch.
#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn queries_after_steps_match_a_raw_cursor_at_the_same_position() {
    let mut stepped = open();
    for _ in 0..8 {
        stepped.step().unwrap();
    }
    let (regs, teb) = stepped.thread_state(None).expect("state after steps");
    let tid = stepped.current_thread_id().expect("current thread") as u32;

    // Independent oracle: a fresh engine + cursor positioned by hand.
    let engine = TtdEngine::open(Path::new(&trace_path())).expect("oracle engine");
    let cursor = engine.create_cursor().expect("oracle cursor");
    cursor.set_position(stepped.position());

    let expected = cursor.read_regs_thread(tid);
    assert_eq!(regs.rip, expected.rip, "RIP must match at equal positions");
    assert_eq!(regs.rsp, expected.rsp, "RSP must match at equal positions");
    assert_eq!(
        teb,
        cursor.teb_thread(tid),
        "TEB must match at equal positions"
    );

    // Memory read through the lazily positioned cursor must agree too.
    let mut a = [0u8; 32];
    let mut b = [0u8; 32];
    let na = stepped.read_memory(regs.rip, &mut a);
    let nb = cursor.read_memory(regs.rip, &mut b);
    assert!(na > 0 && nb > 0, "code at RIP must be readable");
    assert_eq!(a, b, "memory must match at equal positions");
}

/// The point of the position cache, asserted deterministically instead of by
/// wall-clock: after the first query of a stop, further queries must not
/// re-seek the cursor (`SetPosition` may replay from a keyframe, so it is the
/// expensive part of a query).
#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn repeated_queries_do_not_reseek_the_cursor() {
    let mut process = open();

    // First query of the session: one seek, one query.
    let before = process.stats();
    process.thread_state(None).expect("state");
    let after = process.stats();
    assert_eq!(after.queries, before.queries + 1, "{after:?}");
    assert_eq!(after.cursor_seeks, before.cursor_seeks + 1, "{after:?}");

    // Now stopped: a burst of queries must be served from the cache.
    let before = process.stats();
    for _ in 0..20 {
        process.thread_state(None).expect("state");
        let mut buf = [0u8; 16];
        process.read_memory(0x1000, &mut buf);
    }
    let after = process.stats();
    assert_eq!(after.queries, before.queries + 40, "{after:?}");
    assert_eq!(
        after.cursor_seeks, before.cursor_seeks,
        "40 queries at one position must not re-seek the cursor: {after:?}"
    );

    // Moving on must seek again — otherwise the cache is serving stale state.
    // The very first step has to position the step cursor too (it has never
    // been used), so it costs one seek for the step plus one for the query
    // that follows.
    let before = process.stats();
    process.step().unwrap();
    process.thread_state(None).expect("state");
    let after = process.stats();
    assert_eq!(after.steps, before.steps + 1, "{after:?}");
    assert_eq!(after.cursor_seeks, before.cursor_seeks + 2, "{after:?}");

    // A consecutive step is the case the cache exists for: the step cursor is
    // already at the right position, so only the query re-seeks.
    let before = process.stats();
    process.step().unwrap();
    process.thread_state(None).expect("state");
    let after = process.stats();
    assert_eq!(after.steps, before.steps + 1, "{after:?}");
    assert_eq!(after.cursor_seeks, before.cursor_seeks + 1, "{after:?}");
}

/// A `goto` must move both cursors' notion of "where we are": stepping right
/// after a jump has to continue from the destination, not from the position
/// the step cursor happened to be left at.
#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn step_after_goto_continues_from_the_destination() {
    let mut process = open();
    let start = process.position();
    for _ in 0..4 {
        process.step().unwrap();
    }
    let moved = process.position();
    assert!(moved > start);

    process.goto(start).unwrap();
    // Stepping from the jump target must land where stepping from `start`
    // originally did — proof that the step cursor followed the goto.
    process.step().unwrap();
    let after_jump_step = process.position();

    let mut reference = open();
    reference.step().unwrap();
    assert_eq!(
        after_jump_step,
        reference.position(),
        "step after goto must continue from the goto destination"
    );
}

#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn current_thread() {
    let process = open();
    assert!(
        process.current_thread_id().is_some(),
        "should have a current thread"
    );
}

/// `open` must not leave `current_thread_id` pointing at a thread that has no
/// state at the trace's first position: TTD returns a zeroed context for
/// unknown/stale threads, and serving RIP=0 to the first `g` packet would make
/// a live thread look null.
#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn current_thread_at_open_has_state() {
    let process = open();
    let tid = process
        .current_thread_id()
        .expect("a trace must have a current thread at the first position");
    let (regs, _teb) = process
        .thread_state(Some(tid))
        .expect("the initial current thread must have a state at the first position");
    assert!(
        regs.rip > 0,
        "initial current thread {tid:#x} must not have a zeroed context"
    );
}
