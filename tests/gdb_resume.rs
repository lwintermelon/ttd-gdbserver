//! Resume actions: `vCont` range stepping, signal-passing acks, `vCtrlC`,
//! and the event-loop concurrency contracts (non-blocking backend queries,
//! buffered stop reasons on ^C).

mod common;

use std::io::Write as _;
use std::sync::mpsc;
use std::time::Duration;

use ttd_gdbserver::target::{ModuleInfo, StopReason};

use common::harness::{TestSession, delve_handshake, start_server};
use common::mock_target::{ForwardStop, MockTarget};
use common::rsp::{RspClient, from_hex};

#[test]
fn test_vcont_range_step_routes_to_step_for_empty_range() {
    // gdb's range step (vCont;r:start,end) means "step while PC is in
    // [start, end)". TTD can't predict the exit point, so we single-step
    // until the PC leaves the range. The mock's step is a no-op (just bumps
    // `position.steps()`); an empty range (start == end) is a GSP-defined
    // single-step equivalent, so we expect T05 and a thread reply — the same
    // shape as vCont;s:64.
    let mut session = TestSession::start(MockTarget::new());
    session.handshake();
    let c = &mut session.client;

    // vCont doesn't ack separately: the next packet is the stop reply.
    c.ack();
    c.send("vCont;r1000,1000:64");
    let stop = c.recv();
    let s = String::from_utf8_lossy(&stop);
    assert!(
        s.starts_with("T05") && s.contains("thread:p01.64;"),
        "empty range must behave like a single step, got: {s}"
    );

    session.finish();
}

#[test]
fn test_qpass_signals_acks_ok() {
    // gdb may enumerate which signals it wants delivered. Replay is
    // read-only; we ACK without doing anything.
    let mut session = TestSession::start(MockTarget::new());
    session.handshake();
    let c = &mut session.client;

    c.ack();
    c.send("QPassSignals:0203");
    assert_eq!(String::from_utf8_lossy(&c.recv()), "OK");

    c.ack();
    c.send("QProgramSignals:0203");
    assert_eq!(String::from_utf8_lossy(&c.recv()), "OK");

    session.finish();
}

#[test]
fn test_vctrlc_acks_ok() {
    // vCtrlC is extended-remote's preferred ^C; the stub must ACK so the
    // client knows the signal was registered. This test only checks the ack
    // (no replay is in flight, so the abort path is a no-op here); the raw
    // 0x03 form is handled by BlockingEventLoop::on_interrupt.
    let mut session = TestSession::start(MockTarget::new());
    session.handshake();
    let c = &mut session.client;

    c.ack();
    c.send("vCtrlC");
    assert_eq!(String::from_utf8_lossy(&c.recv()), "OK");

    session.finish();
}

/// A packet that needs the backend must never block while a replay is in
/// flight.
///
/// The event loop is single-threaded: parking it on the backend lock also
/// stops it from polling the connection, so ^C could no longer be delivered
/// for the whole duration of the replay. `qGetTLSAddr` is the canary — it is
/// answered `E01` immediately instead of waiting for the replay.
#[test]
fn test_backend_packets_do_not_block_while_replay_runs() {
    let (release_tx, release_rx) = mpsc::channel();
    let target = MockTarget::new()
        .with_modules(vec![ModuleInfo {
            base_addr: 0x1_4000_0000,
            size: 0x1000,
            name: "test.exe".to_string(),
        }])
        .with_forward(ForwardStop::Gated {
            release: release_rx,
            reason: StopReason::TraceEnd,
        });
    let (addr, server) = start_server(target);
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    // Without the non-blocking contract these reads hang for as long as the
    // replay runs, so bound them: a regression fails (timeout) instead of
    // wedging the suite.
    c.stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();

    // Start the replay; the worker parks on the gate holding the backend.
    c.send("vCont;c");
    std::thread::sleep(Duration::from_millis(50));

    // Answered from the protocol layer, without touching the backend.
    assert_eq!(c.roundtrip("qGetTLSAddr:p1.64,0,0"), "E01");
    // Standard read-only queries are subject to the same rule: a running
    // target answers `E` instead of parking the event loop on its lock.
    let mem = c.roundtrip("m1000,10");
    assert!(
        mem.starts_with('E'),
        "a memory read during a replay must fail fast, got: {mem}"
    );

    // Trait methods whose only error path is fatal answer with a neutral,
    // non-fatal result instead of tearing the session down.
    assert_eq!(
        c.roundtrip("qfThreadInfo"),
        "m",
        "a thread listing during a replay must answer an empty list, not die"
    );

    c.ack();
    c.send("qThreadExtraInfo,p1.64");
    let info = String::from_utf8(from_hex(std::str::from_utf8(&c.recv()).unwrap()))
        .expect("qThreadExtraInfo reply must be hex UTF-8");
    assert_eq!(
        info, "target is running",
        "qThreadExtraInfo during a replay must answer without the backend"
    );

    let offsets = c.roundtrip("qOffsets");
    assert!(
        offsets.contains("TextSeg=0140001000"),
        "qOffsets during a replay must use the cached text segment address, got: {offsets}"
    );

    // `R` has no reply in the fork, but it must not kill the session either.
    c.send("R00");
    let r_reply = c.recv();
    assert!(
        r_reply.is_empty(),
        "R during a replay must stay non-fatal, got: {:?}",
        String::from_utf8_lossy(&r_reply)
    );

    // Release the replay and let the session finish normally.
    drop(release_tx);
    let stop = c.recv();
    assert!(
        stop.starts_with(b"T09"),
        "trace end is the rr-style synthetic SIGKILL: {}",
        String::from_utf8_lossy(&stop)
    );

    drop(c);
    server.join().unwrap();
}

/// A race in the GDB event loop: when the replay worker finishes between
/// the event loop's last `take_stop_reason` poll and the `^C` arriving,
/// `on_interrupt` must still drain the buffered stop reason instead of
/// leaving the stub in the `Running` state.
#[test]
fn test_ctrl_c_reports_buffered_stop_reason() {
    let (release_tx, release_rx) = mpsc::channel();
    let target = MockTarget::new().with_forward(ForwardStop::Gated {
        release: release_rx,
        reason: StopReason::Breakpoint {
            bp_id: 1,
            addr: 0xdead,
        },
    });

    let (addr, server) = start_server(target);
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    // Start the replay — the worker spawns and blocks on the gate.
    c.send("vCont;c");
    // Let the event loop settle into the in-flight state.
    std::thread::sleep(Duration::from_millis(20));
    // Release the worker. It returns the Breakpoint reason, the GdbTarget
    // worker then sets `replay_running = false` and pushes the stop reason.
    // With a slow scheduler the on_interrupt triggered by ^C may see
    // `replay_in_flight` as already false; draining the channel with
    // `take_stop_reason` first still finds the buffered reason.
    release_tx.send(()).unwrap();
    // Whatever the timing, the response must be a stop reply — either the
    // worker's T05 or T02 (SIGINT, if ^C landed first). A buffered T05 must
    // not be lost.
    c.stream.write_all(&[0x03]).unwrap();
    let stop = c.recv();
    let s = String::from_utf8_lossy(&stop);
    assert!(
        s.starts_with("T05") || s.starts_with("T02"),
        "expected T05 or T02 stop reply, got: {s}"
    );
    assert!(
        s.contains("thread:p01.64;"),
        "expected thread p1.64 in stop reply, got: {s}"
    );

    drop(c);
    server.join().unwrap();
}

/// `vCont;s` without a thread id is legal RSP ("applies to every thread").
/// The stub carries it out on the selected thread; before the gdbstub fix it
/// answered `PacketUnexpected`, which tore down the whole session.
#[test]
fn test_vcont_step_without_tid_uses_the_selected_thread() {
    let mut session = TestSession::start(MockTarget::new());
    session.handshake();
    let c = &mut session.client;

    // Select thread 100, then step without naming it.
    assert_eq!(c.roundtrip("Hg64"), "OK");
    c.ack();
    c.send("vCont;s");
    let stop = String::from_utf8_lossy(&c.recv()).into_owned();
    assert!(
        stop.starts_with("T05") && stop.contains("thread:p01.64;"),
        "an unqualified step must go to the selected thread, got: {stop}"
    );

    // The session is still usable afterwards, which is the point.
    assert_eq!(c.roundtrip("Hg64"), "OK");
    let next = c.roundtrip("vCont;s:64");
    assert!(next.starts_with("T05"), "stop reply: {next}");

    session.finish();
}
