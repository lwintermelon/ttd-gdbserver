//! GDB RSP frontend against a real TTD trace.
//!
//! Requires `TTD_TRACE_PATH`. Tests auto-skip when the env var is not set.
//!
//! TTD_TRACE_PATH=path/to/trace.run cargo test --test gdb_ttd -- --nocapture

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::thread;

use ttd_gdbserver::gdb::session::run_gdb_session;
use ttd_gdbserver::target::{DebugTarget, TtdProcess};

fn trace_path() -> Option<String> {
    std::env::var("TTD_TRACE_PATH").ok()
}

// ─── Minimal RSP client (same as tests/gdb.rs) ────────────────

struct RspClient {
    stream: TcpStream,
}

impl RspClient {
    fn connect(addr: std::net::SocketAddr) -> Self {
        Self {
            stream: TcpStream::connect(addr).unwrap(),
        }
    }

    fn send(&mut self, payload: &str) {
        let mut pkt = String::with_capacity(payload.len() + 4);
        pkt.push('$');
        pkt.push_str(payload);
        let sum: u8 = payload.bytes().fold(0u8, |a, b| a.wrapping_add(b));
        pkt.push('#');
        pkt.push_str(&format!("{:02x}", sum));
        self.stream.write_all(pkt.as_bytes()).unwrap();
    }

    fn recv(&mut self) -> Vec<u8> {
        let mut b = [0u8; 1];
        loop {
            self.stream.read_exact(&mut b).unwrap();
            if b[0] == b'$' {
                break;
            }
        }
        let mut body = Vec::new();
        loop {
            self.stream.read_exact(&mut b).unwrap();
            if b[0] == b'#' {
                break;
            }
            body.push(b[0]);
        }
        let mut ck = [0u8; 2];
        self.stream.read_exact(&mut ck).unwrap();
        decode(&body)
    }

    fn roundtrip(&mut self, payload: &str) -> String {
        self.send(payload);
        String::from_utf8(self.recv()).unwrap()
    }
}

fn decode(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        match body[i] {
            b'}' if i + 1 < body.len() => {
                out.push(body[i + 1] ^ 0x20);
                i += 2;
            }
            b'*' if i + 1 < body.len() && !out.is_empty() => {
                let n = (body[i + 1].wrapping_sub(29)) as usize;
                let last = out[out.len() - 1];
                out.extend(std::iter::repeat_n(last, n));
                i += 2;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn from_hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
        .collect()
}

fn start_server(trace: &str) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
    let process = TtdProcess::open(Path::new(trace)).expect("open trace");
    let exe = process.modules().first().map(|m| m.name.clone());
    let interrupt = process.interrupt();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let _ = run_gdb_session(process, stream, &Some(interrupt), &exe);
    });
    (addr, handle)
}

fn handshake(c: &mut RspClient) {
    c.stream.write_all(b"+").unwrap();
    assert_eq!(c.roundtrip("QStartNoAckMode"), "OK");
    assert_eq!(c.roundtrip("QThreadSuffixSupported"), "");
    let supported =
        c.roundtrip("qSupported:multiprocess+;swbreak+;hwbreak+;no-resumed+;xmlRegisters=i386");
    assert!(supported.contains("PacketSize="));
    assert_eq!(c.roundtrip("qRegisterInfo0"), "");
    let xml = c.roundtrip("qXfer:features:read:target.xml:0,fff");
    assert!(xml.contains("64bit-core.xml"), "target.xml: {}", xml);
    let core = c.roundtrip("qXfer:features:read:64bit-core.xml:0,fff");
    assert!(core.contains("rip"), "64bit-core.xml: {}", core);
    assert_eq!(c.roundtrip("QListThreadsInStopReply"), "");
    assert_eq!(c.roundtrip("x0,0"), "");
}

#[test]
fn ttd_handshake_threads_registers() {
    let Some(trace) = trace_path() else {
        eprintln!("skipping: TTD_TRACE_PATH not set");
        return;
    };
    let (addr, server) = start_server(&trace);
    let mut c = RspClient::connect(addr);
    handshake(&mut c);

    let threads = c.roundtrip("qfThreadInfo");
    println!("qfThreadInfo: {}", threads);
    assert!(threads.starts_with('m'), "no active threads: {}", threads);
    assert_eq!(c.roundtrip("qsThreadInfo"), "l");

    // Registers of the current thread (select any thread first, like Delve).
    assert_eq!(c.roundtrip("Hg0"), "OK");
    let g = c.roundtrip("g");
    assert!(!g.is_empty(), "empty g response");
    let buf = from_hex(&g);
    let rip = u64::from_le_bytes(buf[16 * 8..16 * 8 + 8].try_into().unwrap());
    println!("rip = {:#x}", rip);
    assert_ne!(rip, 0);

    drop(c);
    server.join().unwrap();
}

#[test]
fn ttd_step_forward_and_backward() {
    let Some(trace) = trace_path() else {
        eprintln!("skipping: TTD_TRACE_PATH not set");
        return;
    };
    let (addr, server) = start_server(&trace);
    let mut c = RspClient::connect(addr);
    handshake(&mut c);

    let when0 = String::from_utf8(from_hex(&c.roundtrip("qRRCmd:when:-1"))).unwrap();
    println!("when (start) = {}", when0);

    // Step forward one instruction (plain 's' steps the current thread).
    let stop = c.roundtrip("s");
    println!("step stop: {}", stop);
    assert!(stop.starts_with("T05"), "stop: {}", stop);

    let when1 = String::from_utf8(from_hex(&c.roundtrip("qRRCmd:when:-1"))).unwrap();
    println!("when (after step) = {}", when1);
    assert_ne!(when0, when1, "position did not advance");

    // Step backward — position must move again (usually back).
    let stop = c.roundtrip("bs");
    println!("bs stop: {}", stop);
    assert!(stop.starts_with("T0"), "stop: {}", stop);

    drop(c);
    server.join().unwrap();
}

#[test]
fn ttd_reverse_continue_reaches_trace_start() {
    let Some(trace) = trace_path() else {
        eprintln!("skipping: TTD_TRACE_PATH not set");
        return;
    };
    let (addr, server) = start_server(&trace);
    let mut c = RspClient::connect(addr);
    handshake(&mut c);

    // Reverse continue with no breakpoints: must stop at trace start (T00).
    assert_eq!(c.roundtrip("Hcp-1.-1"), "OK");
    let stop = c.roundtrip("bc");
    println!("bc stop: {}", stop);
    assert!(
        stop.starts_with("T00"),
        "expected trace-start stop, got {}",
        stop
    );

    drop(c);
    server.join().unwrap();
}

#[test]
fn ttd_memory_read_and_exec_file() {
    let Some(trace) = trace_path() else {
        eprintln!("skipping: TTD_TRACE_PATH not set");
        return;
    };
    let (addr, server) = start_server(&trace);
    let mut c = RspClient::connect(addr);
    handshake(&mut c);

    // Read 16 bytes at the current RIP (via g -> rip).
    assert_eq!(c.roundtrip("Hg0"), "OK");
    let g = c.roundtrip("g");
    let buf = from_hex(&g);
    let rip = u64::from_le_bytes(buf[16 * 8..16 * 8 + 8].try_into().unwrap());
    let m = c.roundtrip(&format!("m{:x},10", rip));
    println!("mem@rip: {}", m);
    assert_eq!(m.len(), 32, "expected 16 bytes of hex, got {}", m);

    // qXfer exec-file returns the first module name/path (possibly chunked).
    let ef = c.roundtrip("qXfer:exec-file:read::0,fff");
    println!("exec-file: {}", ef);
    assert!(
        ef.starts_with('l') || ef.starts_with('m'),
        "exec-file: {}",
        ef
    );

    drop(c);
    server.join().unwrap();
}
