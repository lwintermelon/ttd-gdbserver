//! Shared TCP harness for the GDB RSP integration suites.
//!
//! Both the mock-backend suite (`tests/gdb`) and the real-trace suite
//! (`tests/gdb_ttd`) start one session the same way, so the boilerplate lives
//! here instead of being copy-pasted into every test module.

use std::net::{SocketAddr, TcpListener};
use std::thread::{self, JoinHandle};

use ttd_gdbserver::gdb::session::run_gdb_session;
use ttd_gdbserver::target::DebugTarget;

use crate::rsp::RspClient;

/// Start a one-session GDB stub in a background thread and return its
/// address + join handle.
pub fn start_server<T: DebugTarget + Send + 'static>(target: T) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test listener");
    let addr = listener.local_addr().expect("listener address");
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept test client");
        let _ = run_gdb_session(target, stream);
    });
    (addr, handle)
}

/// An open session: TCP client + the server thread owning it. Call
/// [`Self::finish`] to close the client and join the server, which also keeps
/// test failures from leaking threads.
pub struct TestSession {
    pub client: RspClient,
    server: JoinHandle<()>,
}

impl TestSession {
    pub fn start<T: DebugTarget + Send + 'static>(target: T) -> Self {
        let (addr, server) = start_server(target);
        Self {
            client: RspClient::connect(addr),
            server,
        }
    }

    pub fn handshake(&mut self) {
        delve_handshake(&mut self.client);
    }

    pub fn finish(self) {
        drop(self.client);
        self.server.join().expect("server thread");
    }
}

/// Run Delve's exact handshake against a stub.
pub fn delve_handshake(c: &mut RspClient) {
    // Connection-establishment ack (Delve sends a bare '+').
    c.ack();

    assert_eq!(c.roundtrip("QStartNoAckMode"), "OK");
    assert_eq!(c.roundtrip("QThreadSuffixSupported"), "");
    let supported =
        c.roundtrip("qSupported:multiprocess+;swbreak+;hwbreak+;no-resumed+;xmlRegisters=i386");
    assert!(
        supported.contains("PacketSize="),
        "qSupported reply: {supported}"
    );

    // qRegisterInfo unsupported -> Delve falls back to qXfer:features:read.
    assert_eq!(c.roundtrip("qRegisterInfo0"), "");

    let xml = c.roundtrip("qXfer:features:read:target.xml:0,fff");
    assert!(xml.starts_with('l') || xml.starts_with('m'));
    // target.xml references the per-feature files via <xi:include>.
    for f in ["64bit-core.xml", "64bit-sse.xml", "64bit-seg.xml"] {
        assert!(xml.contains(f), "target.xml missing {f}: {xml}");
    }
    // The core feature file carries the GPRs; segments carry fs/gs_base.
    let core = c.roundtrip("qXfer:features:read:64bit-core.xml:0,fff");
    assert!(core.starts_with('l') || core.starts_with('m'));
    for reg in ["rip", "rsp", "rbp", "rcx"] {
        assert!(core.contains(reg), "64bit-core.xml missing {reg}: {core}");
    }
    let seg = c.roundtrip("qXfer:features:read:64bit-seg.xml:0,fff");
    assert!(seg.starts_with('l') || seg.starts_with('m'));
    for reg in ["gs_base", "fs_base"] {
        assert!(seg.contains(reg), "64bit-seg.xml missing {reg}: {seg}");
    }

    assert_eq!(c.roundtrip("QListThreadsInStopReply"), "");
    assert_eq!(c.roundtrip("x0,0"), "");
}
