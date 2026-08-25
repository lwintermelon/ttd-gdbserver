//! Delve-specific packets: `qRRCmd when`/checkpoints and the `vRun` restart
//! forms.

mod common;

use common::harness::TestSession;
use common::mock_target::MockTarget;

#[test]
fn test_qrrcmd_when_and_checkpoints() {
    let mut session = TestSession::start(MockTarget::new());
    session.handshake();
    let c = &mut session.client;

    // New-style qRRCmd (Delve uses it when thread suffixes are supported).
    assert_eq!(c.when(), "1:0"); // position {sequence:1, steps:0}

    // Checkpoint at current position.
    let ack = c.qrrcmd("checkpoint:-1:");
    assert!(ack.starts_with("Checkpoint 1 at "), "got: {ack}");

    // List checkpoints: header line + tab-separated rows.
    let listing = c.qrrcmd("info checkpoints:-1");
    let lines: Vec<&str> = listing.lines().collect();
    assert!(lines.len() >= 2, "got: {listing:?}");
    let fields: Vec<&str> = lines[1].split('\t').collect();
    assert_eq!(fields.len(), 3, "got: {listing:?}");
    assert_eq!(fields[0], "1");
    assert_eq!(fields[1], "1:0");

    // Delete it.
    assert_eq!(c.qrrcmd("delete checkpoint:-1:31"), "Deleted checkpoint 1");

    // Old-style (hex-encoded command) must also work.
    assert_eq!(c.qrrcmd_hex("when"), "1:0");

    session.finish();
}

#[test]
fn test_vrun_restart_position() {
    let mut session = TestSession::start(MockTarget::new());
    session.handshake();
    let c = &mut session.client;

    // Restart at SEQ:STEPS = A:1F (Delve restart -> vRun;;<hex ascii>).
    let stop = c.vrun("A:1F");
    assert!(stop.starts_with("T05"), "stop reply: {stop}");
    assert_eq!(c.when(), "A:1F");

    // Restart from checkpoint: create one, then vRun to c1.
    c.qrrcmd("checkpoint:-1:");
    let stop = c.vrun("c1");
    assert!(stop.starts_with("T05"), "stop reply: {stop}");
    assert_eq!(c.when(), "A:1F"); // checkpoint was taken at A:1F

    // Plain vRun (no position) -> trace start.
    let stop = c.roundtrip("vRun;");
    assert!(stop.starts_with("T05"), "stop reply: {stop}");
    assert_eq!(c.when(), "0:0");

    session.finish();
}

/// Delve always sends a resume immediately after `vRun` (its `Restart`
/// documents this as "what gdb does"). rr answers that resume at the restart
/// position; replaying forward instead would run straight to the trace
/// boundary and silently lose the checkpoint.
#[test]
fn test_resume_after_restart_stops_at_the_restart_position() {
    let mut session = TestSession::start(MockTarget::new());
    session.handshake();
    let c = &mut session.client;

    let stop = c.vrun("A:1F");
    assert!(stop.starts_with("T05"), "stop reply: {stop}");
    assert_eq!(c.when(), "A:1F");

    // MockTarget's forward continue advances the position by 100 steps, so a
    // real replay here would change `when`.
    let stop = c.roundtrip("vCont;c");
    assert!(stop.starts_with("T05"), "stop reply: {stop}");
    assert_eq!(
        c.when(),
        "A:1F",
        "the resume after a restart must not replay past the restart position"
    );

    // The position is live again: a second continue does replay.
    let stop = c.roundtrip("vCont;c");
    assert!(stop.starts_with("T09"), "stop reply: {stop}");
    assert_ne!(c.when(), "A:1F", "a later continue must replay");

    session.finish();
}

#[test]
fn test_vrun_position_sequence_starting_with_c() {
    // Positions carry ':', so they must be parsed before the bare "c<N>"
    // checkpoint form: a checkpoint prefix check running first would swallow
    // any position whose hex sequence starts with 'c'/'C' (e.g. "C0:1F").
    let mut session = TestSession::start(MockTarget::new());
    session.handshake();
    let c = &mut session.client;

    let stop = c.vrun("C0:1F");
    assert!(stop.starts_with("T05"), "stop reply: {stop}");
    assert_eq!(
        c.when(),
        "C0:1F",
        "restart must land at the requested position"
    );

    session.finish();
}

#[test]
fn test_vrun_unknown_checkpoint_is_rejected() {
    // A "c99" restart with no such checkpoint must be reported as an error
    // (not acknowledged as a successful restart) and must leave the position
    // unchanged.
    let mut session = TestSession::start(MockTarget::new());
    session.handshake();
    let c = &mut session.client;

    let stop = c.vrun("c99");
    assert!(
        stop.starts_with('E'),
        "unknown checkpoint must produce an error reply, got {stop:?}"
    );
    assert_eq!(
        c.when(),
        "1:0",
        "unknown checkpoint must not move the position"
    );

    session.finish();
}
