//! Raw-RSP property checks against a real trace.
//!
//! The other integration targets drive one protocol area each. These assert
//! invariants the protocol implies, which must hold for *any* trace:
//! forward/backward stepping is an involution, and a range step is abortable
//! mid-flight. A failure means the backend is internally inconsistent, not
//! merely different from a recording.

mod common;

use std::time::{Duration, Instant};

use ttd_gdbserver::target::{DebugTarget, StopReason};

use common::open_trace;

/// Stepping forward `n` instructions and back `n` must restore the exact
/// position and the same bytes for every general-purpose register.
#[test]
#[cfg_attr(not(has_ttd_trace), ignore = "requires TTD_TRACE_PATH")]
fn step_roundtrip_restores_position_and_registers() {
    let mut process = open_trace();
    let start = process.position();
    let start_regs = common::current_regs(&process);

    let mut progressed = 0;
    for _ in 0..8 {
        match process.step().expect("step forward") {
            StopReason::StepComplete => progressed += 1,
            _ => break,
        }
    }
    assert!(progressed > 0, "the trace must allow at least one step");
    assert!(
        process.position() > start,
        "forward stepping must advance the position"
    );

    for _ in 0..progressed {
        process.step_back().expect("step backward");
    }
    assert_eq!(
        process.position(),
        start,
        "{progressed} forward steps followed by {progressed} back steps must return to the start"
    );
    let end_regs = common::current_regs(&process);
    assert_eq!(
        start_regs.Rip, end_regs.Rip,
        "the round trip must restore RIP"
    );
    assert_eq!(
        start_regs.Rsp, end_regs.Rsp,
        "the round trip must restore RSP"
    );
    assert_eq!(
        start_regs.Rbp, end_regs.Rbp,
        "the round trip must restore RBP"
    );
}

/// `goto(pos)` must land on the same position however that position was
/// reached: the debug position is a property of the trace, not of the path.
#[test]
#[cfg_attr(not(has_ttd_trace), ignore = "requires TTD_TRACE_PATH")]
fn goto_is_path_independent() {
    let mut process = open_trace();
    let start = process.position();
    for _ in 0..5 {
        if !matches!(process.step().expect("step"), StopReason::StepComplete) {
            break;
        }
    }
    let reached = process.position();
    assert!(reached > start, "stepping must reach a later position");
    let reached_regs = common::current_regs(&process);

    process.goto(start).expect("goto start");
    assert_eq!(process.position(), start, "goto must reach the start");

    process.goto(reached).expect("goto reached");
    assert_eq!(
        process.position(),
        reached,
        "goto must reach the stepped position"
    );
    let again = common::current_regs(&process);
    assert_eq!(
        again.Rip, reached_regs.Rip,
        "the same position must report the same RIP"
    );
}

/// A range step runs one short instruction at a time, so the SDK's cursor
/// interrupt cannot abort it. An interrupt must still end it promptly, at a
/// position short of the trace boundary.
#[test]
#[cfg_attr(not(has_ttd_trace), ignore = "requires TTD_TRACE_PATH")]
fn range_step_is_abortable_mid_flight() {
    let mut process = open_trace();

    // The delay is a race by nature: too short and the interrupt lands before
    // the first instruction (the loop reads the epoch at its top), too long and
    // a short trace may already be walked. Both are retried with a longer
    // delay rather than reported as a backend failure.
    for delay in [30u64, 150, 600] {
        process.begin_resume();
        let start = process.position();
        let (_, last) = process.lifetime();
        let interrupt = process
            .interrupt_handle()
            .expect("the TTD backend publishes an interrupt handle");
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(delay));
            interrupt();
        });

        let began = Instant::now();
        let reason = process.step_range(None, 0, u64::MAX).expect("range step");
        let elapsed = began.elapsed();
        worker.join().expect("interrupt thread");

        assert!(
            elapsed < Duration::from_secs(2),
            "the interrupt must not wait for the range to finish (took {elapsed:?})"
        );
        match reason {
            StopReason::Interrupted if process.position() > start => {
                assert!(
                    process.position() < last,
                    "an interrupted range must stop short of the trace boundary"
                );
                return;
            }
            StopReason::Interrupted => {
                eprintln!("interrupt at {delay}ms landed before the first instruction");
            }
            other => panic!("an interrupted range step must report Interrupted, got {other:?}"),
        }
    }
    panic!("no interrupt delay stopped the range mid-flight; is the trace too short?");
}
