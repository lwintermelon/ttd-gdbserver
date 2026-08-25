use crate::support::*;

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
fn test_trace_end_reports_sigkill() {
    let mut session =
        TestSession::start(MockTarget::new().with_fixed_forward(StopReason::TraceEnd));
    session.handshake();

    // A forward continue at trace end must report T09 so Delve enters its
    // "almost exited" state (can still rewind).
    let stop = session.client.roundtrip("vCont;c");
    assert!(stop.starts_with("T09"), "stop reply: {stop}");

    session.finish();
}

/// A backend-reported `StopReason::Interrupted` (the backend's interrupt
/// handle aborted the replay — response to ^C) must surface on the wire as
/// T02/SIGINT. Before the `Interrupted` variant existed it collapsed into
/// the catch-all T05, and a Delve client would loop-resume.
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
