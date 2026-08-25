use std::net::TcpListener;
use std::thread;

use crate::support::*;

#[test]
fn test_engine_state_persists_across_sequential_sessions() {
    // The documented contract: the server accepts one session at a time and
    // backend state (position, breakpoints) survives into the next session.
    // `serve_sessions(.., Some(2))` makes the accept loop observable.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        // Box<dyn Error> is not Send; carry the error as text instead.
        serve_sessions(listener, MockTarget::new(), Some(2), || false)
            .map(|_| ())
            .map_err(|e| e.to_string())
            .unwrap()
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

    server.join().unwrap();
}

#[test]
fn test_zero_session_bound_accepts_nobody() {
    // `Some(0)` is a real bound: it must return the backend untouched
    // instead of accepting one connection before checking the bound.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let backend = serve_sessions(listener, MockTarget::new(), Some(0), || false).unwrap();
    assert_eq!(
        backend.position(),
        TtdPosition {
            sequence: 1,
            steps: 0
        }
    );
}

#[test]
fn test_shutdown_flag_stops_accept_loop_without_clients() {
    // A shutdown flag observed between accept attempts must end the loop and
    // return the backend — this is the ^C graceful-exit path.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    // already shut down: no session may be accepted
    let backend = serve_sessions(listener, MockTarget::new(), None, || true).unwrap();
    assert_eq!(
        backend.position(),
        TtdPosition {
            sequence: 1,
            steps: 0
        }
    );
}
