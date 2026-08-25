use crate::support::*;

#[test]
fn test_breakpoints_and_continue() {
    let mut session = TestSession::start(MockTarget::new());
    session.handshake();
    let c = &mut session.client;

    assert_eq!(c.roundtrip("Z0,2000,1"), "OK");
    let stop = c.roundtrip("vCont;c");
    assert!(stop.starts_with("T05"), "stop reply: {stop}");
    assert!(stop.contains("thread:p01.64;"), "stop reply: {stop}");
    assert_eq!(c.roundtrip("z0,2000,1"), "OK");

    // Step via vCont.
    let stop = c.roundtrip("vCont;s:64");
    assert!(stop.starts_with("T05"), "stop reply: {stop}");

    // Data watchpoint.
    assert_eq!(c.roundtrip("Z2,3000,4"), "OK");
    assert_eq!(c.roundtrip("z2,3000,4"), "OK");

    session.finish();
}

/// Watchpoint stop replies carry the access kind: a read watchpoint hit must
/// surface as `rwatch`, not `watch` (write).
#[test]
fn test_watchpoint_stop_reply_reports_read_kind() {
    let target = MockTarget::new().with_fixed_forward(StopReason::Watchpoint {
        bp_id: 1,
        addr: 0x3000,
        access: 0x01, // DataAccessMask::Read
    });

    let mut session = TestSession::start(target);
    session.handshake();
    let c = &mut session.client;

    let stop = c.roundtrip("vCont;c");
    assert!(
        stop.starts_with("T05"),
        "read watchpoint must stop with T05, got: {stop}"
    );
    // GDB spells the read kind `rwatch`; a write hit would be plain `watch:`.
    assert!(
        stop.contains("rwatch"),
        "read watchpoint hit must report rwatch, got: {stop}"
    );

    session.finish();
}

/// A backend whose watchpoint registration always fails (models TTD's
/// AddMemoryWatchpoint rejecting the address). The stub must answer Z0/Z1/Z2
/// with `E16` ("cannot insert breakpoint") instead of acknowledging a
/// breakpoint that will never fire.
#[test]
fn test_breakpoint_registration_failure_is_reported() {
    let mut session = TestSession::start(MockTarget::new().with_failing_breakpoints());
    session.handshake();
    let c = &mut session.client;

    assert_eq!(c.roundtrip("Z0,2000,1"), "E16", "Z0 must report failure");
    assert_eq!(c.roundtrip("Z1,2000,1"), "E16", "Z1 must report failure");
    assert_eq!(c.roundtrip("Z2,3000,4"), "E16", "Z2 must report failure");

    // A failed registration must not wedge the session: stepping still works.
    let stop = c.roundtrip("vCont;s:64");
    assert!(
        stop.starts_with("T05") && stop.contains("thread:p01.64;"),
        "step after failed Z must still stop, got: {stop}"
    );

    session.finish();
}
