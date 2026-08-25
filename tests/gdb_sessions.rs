//! The multi-session accept loop: backend state persistence across
//! reconnects, the session bound, and graceful shutdown.

mod common;

use std::io::Read;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use ttd_gdbserver::gdb::session::serve_sessions;
use ttd_gdbserver::target::DebugTarget;
use ttd_gdbserver::ttd::types::TtdPosition;

use common::harness::delve_handshake;
use common::mock_target::MockTarget;
use common::rsp::RspClient;

#[test]
fn test_engine_state_persists_across_sequential_sessions() {
    // The documented contract: the server accepts one session at a time and
    // backend state (position, breakpoints) survives into the next session.
    // `persist` keeps the loop accepting; the shutdown flag ends it after the
    // harness is done with the second session.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let shutdown_flag = Arc::new(AtomicBool::new(false));
    let thread_flag = Arc::clone(&shutdown_flag);
    let server = thread::spawn(move || {
        let shutdown = move || thread_flag.load(Ordering::Acquire);
        serve_sessions(listener, MockTarget::new(), true, &shutdown)
            .map(|_| ())
            .unwrap();
    });

    // Session 1: set a breakpoint, continue onto it, then single-step twice.
    let mut c1 = RspClient::connect(addr);
    delve_handshake(&mut c1);
    assert_eq!(c1.roundtrip("Z0,2000,1"), "OK");
    let stop = c1.roundtrip("vCont;c");
    assert!(stop.starts_with("T05"), "session-1 stop: {stop}");
    let stop = c1.roundtrip("s");
    assert!(stop.starts_with("T05"), "step-1 stop: {stop}");
    let stop = c1.roundtrip("s");
    assert!(stop.starts_with("T05"), "step-2 stop: {stop}");
    drop(c1);

    // Session 2: same backend — position moved AND breakpoint table carried
    // over; after removing it, continue runs to the trace end (T09).
    let mut c2 = RspClient::connect(addr);
    delve_handshake(&mut c2);
    assert_eq!(c2.when(), "1:2", "position must persist across sessions");

    assert_eq!(
        c2.roundtrip("z0,2000,1"),
        "OK",
        "breakpoint must still exist"
    );
    let stop = c2.roundtrip("vCont;c");
    assert!(
        stop.starts_with("T09"),
        "after removing the persisted breakpoint, continue must reach trace end, got {stop}"
    );
    drop(c2);

    shutdown_flag.store(true, Ordering::Release);
    server.join().unwrap();
}

#[test]
fn test_checkpoints_persist_across_sequential_sessions() {
    // Checkpoints are trace-global state, exactly like position and
    // breakpoints: a client that reconnects must still be able to list and
    // restart from the checkpoints a previous session created. (`GdbTarget`
    // hands the table back to the accept loop when the session ends.)
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let shutdown_flag = Arc::new(AtomicBool::new(false));
    let thread_flag = Arc::clone(&shutdown_flag);
    let server = thread::spawn(move || {
        let shutdown = move || thread_flag.load(Ordering::Acquire);
        serve_sessions(listener, MockTarget::new(), true, &shutdown)
            .map(|_| ())
            .unwrap();
    });

    // Session 1: take a checkpoint at 1:0, then step away from it.
    let mut c1 = RspClient::connect(addr);
    delve_handshake(&mut c1);
    let ack = c1.qrrcmd("checkpoint:-1:");
    assert!(ack.starts_with("Checkpoint 1 at 1:0"), "got: {ack}");
    let stop = c1.roundtrip("s");
    assert!(stop.starts_with("T05"), "step stop: {stop}");
    assert_eq!(
        c1.when(),
        "1:1",
        "session must have moved away from the checkpoint"
    );
    drop(c1);

    // Session 2: the checkpoint must have survived the reconnect.
    let mut c2 = RspClient::connect(addr);
    delve_handshake(&mut c2);
    let listing = c2.qrrcmd("info checkpoints:-1");
    let lines: Vec<&str> = listing.lines().collect();
    assert!(
        lines.len() >= 2,
        "checkpoint must survive the reconnect: {listing:?}"
    );
    let fields: Vec<&str> = lines[1].split('\t').collect();
    assert_eq!(fields.len(), 3, "got: {listing:?}");
    assert_eq!(fields[0], "1");
    assert_eq!(
        fields[1], "1:0",
        "checkpoint position must survive the reconnect"
    );

    let stop = c2.vrun("c1");
    assert!(stop.starts_with("T05"), "restart stop: {stop}");
    assert_eq!(
        c2.when(),
        "1:0",
        "restart c1 must land on the checkpoint position"
    );
    drop(c2);

    shutdown_flag.store(true, Ordering::Release);
    server.join().unwrap();
}

#[test]
fn test_kill_closes_connection_without_reply() {
    // Delve's gdbserial expects EOF on `k` (the gdbstub fork's "kill without
    // a reply" patch): any byte — even an error reply — would break Delve's
    // connection teardown. This pins the fork behavior at the wire level.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let shutdown = || false;
        serve_sessions(listener, MockTarget::new(), false, &shutdown)
            .map(|_| ())
            .unwrap();
    });

    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);
    c.send("k");
    let mut byte = [0u8; 1];
    let n = c.stream.read(&mut byte).unwrap();
    assert_eq!(
        n, 0,
        "kill must close the connection without a reply (EOF), got byte {:#x}",
        byte[0]
    );
    drop(c);
    server.join().unwrap();
}

#[test]
fn test_r_restart_packet() {
    // `R` restarts the inferior. gdbstub answers with an *empty* reply (its
    // "not supported" vocabulary) even though our restart hook ran — Delve
    // never sends `R` (it restarts via `vRun`), so this pins the fork's wire
    // behavior rather than a client contract.
    //
    // Note: gdbstub's parser only accepts the long-obsolete `R xx` form
    // (a trailing hex byte); a bare `R` is a malformed command that ends the
    // session, which is fork behavior we do not try to paper over.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let shutdown = || false;
        serve_sessions(listener, MockTarget::new(), false, &shutdown)
            .map(|_| ())
            .unwrap();
    });

    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);
    // The handshake leaves the mock at its initial position (1:0), past the
    // trace start (0:0), so the restart is observable.
    assert_ne!(c.when(), "0:0");
    let reply = c.roundtrip("R00");
    assert!(
        reply.is_empty(),
        "gdbstub answers R with the empty reply, got {reply:?}"
    );
    assert_eq!(
        c.when(),
        "0:0",
        "restart hook must still have jumped to the trace start"
    );
    drop(c);
    server.join().unwrap();
}

#[test]
fn test_shutdown_flag_stops_accept_loop_without_clients() {
    // A shutdown flag observed between accept attempts must end the loop and
    // return the backend — this is the ^C graceful-exit path.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    // already shut down: no session may be accepted
    let shutdown = || true;
    let backend = serve_sessions(listener, MockTarget::new(), true, &shutdown).unwrap();
    assert_eq!(backend.position(), TtdPosition::new(1, 0));
}
