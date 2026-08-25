//! Reverse execution: `bc`/`bs`, trace-end/trace-start signals, and the
//! backend-interrupted SIGINT stop.

mod common;

use common::harness::TestSession;
use common::mock_target::MockTarget;
use ttd_gdbserver::target::StopReason;

#[test]
fn test_reverse_execution() {
    let mut session = TestSession::start(MockTarget::new());
    session.handshake();
    let c = &mut session.client;

    // Delve's reverse continue: Hc (any thread) + bc.
    assert_eq!(c.roundtrip("Hcp-1.-1"), "OK");
    let stop = c.roundtrip("bc");
    // Mock returns TraceStart -> sig 0 ("at start").
    assert!(stop.starts_with("T00"), "stop reply: {stop}");
    assert!(stop.contains("thread:p01.64;"), "stop reply: {stop}");

    // Delve's reverse step: Hc<tid> + bs.
    assert_eq!(c.roundtrip("Hc64"), "OK");
    let stop = c.roundtrip("bs");
    assert!(stop.starts_with("T05"), "stop reply: {stop}");

    session.finish();
}

#[test]
fn test_trace_end_reports_sigkill_for_every_client() {
    let mut session =
        TestSession::start(MockTarget::new().with_fixed_forward(StopReason::TraceEnd));
    session.handshake();

    // rr-style trace end is synthetic SIGKILL; Delve interprets it as
    // "almost exited", plain GDB sees the same wire stop.
    let stop = session.client.roundtrip("vCont;c");
    assert!(stop.starts_with("T09"), "stop reply: {stop}");

    session.finish();
}

/// rr's two-stage end sequence (`in_debuggee_end_state` in `GdbServer.cc`):
/// a recorded process exit first stops with the synthetic SIGKILL (the stop
/// stays rewindable, Delve's "almost exited"); only a *forward* resume from
/// that stop reports the real exit `W00`.
#[test]
fn test_process_exit_is_sigkill_first_then_w00_on_forward_resume() {
    let mut session =
        TestSession::start(MockTarget::new().with_fixed_forward(StopReason::ProcessExit));
    session.handshake();
    let c = &mut session.client;

    let stop = c.roundtrip("vCont;c");
    assert!(
        stop.starts_with("T09"),
        "the exit stop must be the rewindable synthetic SIGKILL, got: {stop}"
    );

    let stop = c.roundtrip("vCont;c");
    assert!(
        stop.starts_with("W00"),
        "a forward resume from the exit stop must report the real exit, got: {stop}"
    );

    session.finish();
}

/// A backward resume leaves the end state (rr processes reverse-exec
/// requests normally from `in_debuggee_end_state`): after `bc`, the next
/// forward continue replays again and reports the exit stop (T09), not W00.
#[test]
fn test_backward_continue_leaves_the_end_state() {
    let mut session =
        TestSession::start(MockTarget::new().with_fixed_forward(StopReason::ProcessExit));
    session.handshake();
    let c = &mut session.client;

    assert!(c.roundtrip("vCont;c").starts_with("T09"));

    assert_eq!(c.roundtrip("Hcp-1.-1"), "OK");
    let stop = c.roundtrip("bc");
    assert!(stop.starts_with("T00"), "mock backward stop: {stop}");

    let stop = c.roundtrip("vCont;c");
    assert!(
        stop.starts_with("T09"),
        "forward continue after a backward op must replay, not report W00: {stop}"
    );

    session.finish();
}

/// A backend-reported `StopReason::Interrupted` (the backend's interrupt
/// handle aborted the replay — response to ^C) must surface on the wire as
/// T02/SIGINT; collapsing it into the catch-all T05 would make a Delve
/// client loop-resume.
#[test]
fn test_backend_interrupted_stop_reports_sigint() {
    let mut session =
        TestSession::start(MockTarget::new().with_fixed_forward(StopReason::Interrupted));
    session.handshake();

    let stop = session.client.roundtrip("vCont;c");
    assert!(
        stop.starts_with("T02"),
        "backend Interrupted must report SIGINT (T02), got: {stop}"
    );
    assert!(
        stop.contains("thread:p01.64;"),
        "stop reply must carry the thread id, got: {stop}"
    );

    session.finish();
}
