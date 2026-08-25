use crate::support::*;

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

#[test]
fn test_vrun_position_sequence_starting_with_c() {
    // Regression: the checkpoint branch used to run before the position
    // branch and matched any position whose hex sequence starts with
    // 'c'/'C', so a restart to "C0:1F" was silently swallowed. Positions
    // carry ':', so they must be parsed before the bare "c<N>" form.
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
    // A "c99" restart with no such checkpoint must be ignored (position
    // unchanged), not silently misparsed.
    let mut session = TestSession::start(MockTarget::new());
    session.handshake();
    let c = &mut session.client;

    let stop = c.vrun("c99");
    assert!(stop.starts_with("T05"), "stop reply: {stop}");
    assert_eq!(
        c.when(),
        "1:0",
        "unknown checkpoint must not move the position"
    );

    session.finish();
}
