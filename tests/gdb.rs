//! GDB RSP frontend integration tests over TCP with a mock backend.
//!
//! These tests replay the *exact* packet sequence Delve's gdbserial client
//! sends (see delve/pkg/proc/gdbserial/gdbserver_conn.go handshake()), then
//! exercise the reversible-debugging packets: vCont / bc / bs / vRun /
//! qRRCmd checkpoints.
//!
//! No TTD required - the backend is a deterministic in-memory mock.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use ttd_gdbserver::gdb::session::run_gdb_session;
use ttd_gdbserver::target::{
    BreakpointKind, DebugError, DebugTarget, ModuleInfo, StopReason, TargetDiagnostics,
    ThreadDiagnostics, ThreadExtraInfoData,
};
use ttd_gdbserver::ttd::types::{TtdPosition, TtdX64Regs};

// ─── Mock backend ──────────────────────────────────────────────

#[derive(Default)]
struct MockTarget {
    position: TtdPosition,
    current_tid: Option<u64>,
}

impl MockTarget {
    fn new() -> Self {
        Self {
            position: TtdPosition {
                sequence: 1,
                steps: 0,
            },
            current_tid: Some(100),
        }
    }

    fn mock_regs(&self) -> TtdX64Regs {
        TtdX64Regs {
            rax: 0x11,
            rcx: 0x22,
            rip: 0x1000,
            rsp: 0x7ffc_0000,
            rbp: 0x7ffc_0100,
            eflags: 0x202,
            cs: 0x33,
            ..Default::default()
        }
    }
}

impl DebugTarget for MockTarget {
    fn step(&mut self) -> Result<StopReason, DebugError> {
        self.position.steps += 1;
        Ok(StopReason::StepComplete)
    }
    fn step_back(&mut self) -> Result<StopReason, DebugError> {
        self.position.steps = self.position.steps.saturating_sub(1);
        Ok(StopReason::StepComplete)
    }
    fn continue_forward(&mut self) -> Result<StopReason, DebugError> {
        self.position.steps += 100;
        Ok(StopReason::Breakpoint {
            bp_id: 1,
            addr: 0xdead,
        })
    }
    fn continue_backward(&mut self) -> Result<StopReason, DebugError> {
        self.position.steps = 0;
        Ok(StopReason::TraceStart)
    }
    fn goto(&mut self, pos: TtdPosition) -> Result<StopReason, DebugError> {
        self.position = pos;
        Ok(StopReason::PositionReached)
    }

    fn read_memory(&self, addr: u64, buf: &mut [u8]) -> usize {
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (addr + i as u64) as u8;
        }
        buf.len()
    }

    fn set_breakpoint(&mut self, addr: u64, _thread_id: Option<u64>) -> u64 {
        addr
    }
    fn set_data_breakpoint(&mut self, addr: u64, _size: u64, _kind: BreakpointKind) -> u64 {
        addr
    }
    fn remove_breakpoint(&mut self, _id: u64) -> bool {
        true
    }

    fn active_thread_ids(&self) -> Vec<u64> {
        vec![100]
    }
    fn thread_state(&self, thread_id: Option<u64>) -> Option<(TtdX64Regs, u64)> {
        let tid = thread_id.or(self.current_tid)?;
        if tid != 100 {
            return None;
        }
        Some((self.mock_regs(), 0x7ffe_0000))
    }
    fn thread_info(&self, _thread_id: u64) -> Option<ThreadExtraInfoData> {
        // Real backend: return None for unknown threads. The mock only
        // knows thread 100.
        if _thread_id == 100 {
            Some(ThreadExtraInfoData {
                unique_id: 1,
                current_position: self.position,
                teb: 0x7ffe_0000,
            })
        } else {
            None
        }
    }
    fn current_thread_id(&self) -> Option<u64> {
        self.current_tid
    }
    fn set_current_thread(&mut self, id: u64) {
        self.current_tid = Some(id);
    }

    fn modules(&self) -> Vec<ModuleInfo> {
        vec![]
    }
    fn diagnostics(&self) -> TargetDiagnostics {
        TargetDiagnostics {
            first_position: self.lifetime().0,
            last_position: self.lifetime().1,
            current_position: self.position,
            threads: vec![ThreadDiagnostics {
                unique_id: 1,
                os_thread_id: 100,
                active_time: (self.position, self.lifetime().1),
            }],
            exception_count: 0,
            module_count: 0,
        }
    }
    fn position(&self) -> TtdPosition {
        self.position
    }
    fn lifetime(&self) -> (TtdPosition, TtdPosition) {
        (
            TtdPosition {
                sequence: 0,
                steps: 0,
            },
            TtdPosition {
                sequence: 9,
                steps: 1000,
            },
        )
    }
}

// ─── Minimal RSP test client ──────────────────────────────────

struct RspClient {
    stream: TcpStream,
}

impl RspClient {
    fn connect(addr: std::net::SocketAddr) -> Self {
        Self {
            stream: TcpStream::connect(addr).unwrap(),
        }
    }

    /// Send `$payload#ck`.
    fn send(&mut self, payload: &str) {
        let mut pkt = String::with_capacity(payload.len() + 4);
        pkt.push('$');
        pkt.push_str(payload);
        let sum: u8 = payload.bytes().fold(0u8, |a, b| a.wrapping_add(b));
        pkt.push('#');
        pkt.push_str(&format!("{:02x}", sum));
        self.stream.write_all(pkt.as_bytes()).unwrap();
    }

    /// Read one packet response, skipping bare +/- acks. Returns the
    /// decoded body (escapes undone, run-length expanded).
    fn recv(&mut self) -> Vec<u8> {
        let mut b = [0u8; 1];
        // Wait for '$'
        loop {
            self.stream.read_exact(&mut b).unwrap();
            if b[0] == b'$' {
                break;
            }
            assert!(b[0] == b'+' || b[0] == b'-', "unexpected byte {:#x}", b[0]);
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
        let sum: u8 = body.iter().fold(0u8, |a, &x| a.wrapping_add(x));
        assert_eq!(
            format!("{:02x}", sum),
            std::str::from_utf8(&ck).unwrap().to_lowercase()
        );
        decode(&body)
    }

    fn roundtrip(&mut self, payload: &str) -> String {
        self.send(payload);
        String::from_utf8(self.recv()).unwrap()
    }
}

/// Undo `}` escapes and `*` run-length (mirrors the client side of RSP).
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

fn start_server(target: MockTarget) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let _ = run_gdb_session(target, stream, &None, &Some("test.exe".to_string()));
    });
    (addr, handle)
}

/// Run Delve's exact handshake against the stub.
fn delve_handshake(c: &mut RspClient) {
    // Connection-establishment ack (Delve sends a bare '+').
    c.stream.write_all(b"+").unwrap();

    assert_eq!(c.roundtrip("QStartNoAckMode"), "OK");
    assert_eq!(c.roundtrip("QThreadSuffixSupported"), "");
    let supported =
        c.roundtrip("qSupported:multiprocess+;swbreak+;hwbreak+;no-resumed+;xmlRegisters=i386");
    assert!(
        supported.contains("PacketSize="),
        "qSupported reply: {}",
        supported
    );

    // qRegisterInfo unsupported -> Delve falls back to qXfer:features:read.
    assert_eq!(c.roundtrip("qRegisterInfo0"), "");

    let xml = c.roundtrip("qXfer:features:read:target.xml:0,fff");
    assert!(xml.starts_with('l') || xml.starts_with('m'));
    // target.xml references the per-feature files via <xi:include>.
    for f in ["64bit-core.xml", "64bit-sse.xml", "64bit-seg.xml"] {
        assert!(xml.contains(f), "target.xml missing {}: {}", f, xml);
    }
    // The core feature file carries the GPRs; segments carry fs/gs_base.
    let core = c.roundtrip("qXfer:features:read:64bit-core.xml:0,fff");
    assert!(core.starts_with('l') || core.starts_with('m'));
    for reg in ["rip", "rsp", "rbp", "rcx"] {
        assert!(
            core.contains(reg),
            "64bit-core.xml missing {}: {}",
            reg,
            core
        );
    }
    let seg = c.roundtrip("qXfer:features:read:64bit-seg.xml:0,fff");
    assert!(seg.starts_with('l') || seg.starts_with('m'));
    for reg in ["gs_base", "fs_base"] {
        assert!(seg.contains(reg), "64bit-seg.xml missing {}: {}", reg, seg);
    }

    assert_eq!(c.roundtrip("QListThreadsInStopReply"), "");
    assert_eq!(c.roundtrip("x0,0"), "");
}

// ─── Tests ────────────────────────────────────────────────────

#[test]
fn test_handshake() {
    let (addr, server) = start_server(MockTarget::new());
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);
    drop(c);
    server.join().unwrap();
}

#[test]
fn test_thread_listing_and_process_info() {
    let (addr, server) = start_server(MockTarget::new());
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    // tid 100 = 0x64; gdbstub negotiates multiprocess with Delve, so thread
    // ids carry the p<pid>. prefix (FAKE_PID = 1).
    assert_eq!(c.roundtrip("qfThreadInfo"), "mp01.64");
    assert_eq!(c.roundtrip("qsThreadInfo"), "l");

    let pi = c.roundtrip("qProcessInfo");
    assert!(pi.starts_with("pid:"), "qProcessInfo: {}", pi);
    assert!(pi.contains("ptrsize:8"), "qProcessInfo: {}", pi);

    let _ = c.roundtrip("qAttached"); // "1" or whatever the stub reports
    assert_eq!(c.roundtrip("qC"), "");

    drop(c);
    server.join().unwrap();
}

#[test]
fn test_registers_and_memory() {
    let (addr, server) = start_server(MockTarget::new());
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    // Delve selects the thread with Hg, then reads registers.
    assert_eq!(c.roundtrip("Hg64"), "OK");
    let g = c.roundtrip("g");
    let buf = from_hex(&g);
    // Layout: 16 GPRs, rip, eflags(4), 6 segs(4 each), st(80), fctrl..fop(32),
    // xmm(256), mxcsr(4), fs_base, gs_base.
    let rip = u64::from_le_bytes(buf[16 * 8..16 * 8 + 8].try_into().unwrap());
    assert_eq!(rip, 0x1000);
    let gs_base_off = 17 * 8 + 4 + 6 * 4 + 80 + 8 * 4 + 256 + 4 + 8;
    let gs_base = u64::from_le_bytes(buf[gs_base_off..gs_base_off + 8].try_into().unwrap());
    assert_eq!(gs_base, 0x7ffe_0000);

    // 'p' for rip (regnum 16 = 0x10).
    let p = c.roundtrip("p10");
    assert_eq!(p, "0010000000000000");

    // Memory read (mock fills (addr+i) as u8; 0x1000 & 0xff == 0).
    let m = c.roundtrip("m1000,8");
    assert_eq!(m, "0001020304050607");

    // Writes are unsupported (read-only replay): non-fatal error response.
    let resp = c.roundtrip("M1000,2:abcd");
    assert!(
        resp.is_empty() || resp.starts_with('E'),
        "write resp: {}",
        resp
    );
    let resp = c.roundtrip("P10=ffffffffffffffff");
    assert!(
        resp.is_empty() || resp.starts_with('E'),
        "write resp: {}",
        resp
    );

    drop(c);
    server.join().unwrap();
}

#[test]
fn test_breakpoints_and_continue() {
    let (addr, server) = start_server(MockTarget::new());
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    assert_eq!(c.roundtrip("Z0,2000,1"), "OK");
    let stop = c.roundtrip("vCont;c");
    assert!(stop.starts_with("T05"), "stop reply: {}", stop);
    assert!(stop.contains("thread:p01.64;"), "stop reply: {}", stop);
    assert_eq!(c.roundtrip("z0,2000,1"), "OK");

    // Step via vCont.
    let stop = c.roundtrip("vCont;s:64");
    assert!(stop.starts_with("T05"), "stop reply: {}", stop);

    // Data watchpoint.
    assert_eq!(c.roundtrip("Z2,3000,4"), "OK");
    assert_eq!(c.roundtrip("z2,3000,4"), "OK");

    drop(c);
    server.join().unwrap();
}

#[test]
fn test_reverse_execution() {
    let (addr, server) = start_server(MockTarget::new());
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    // Delve's reverse continue: Hc (any thread) + bc.
    assert_eq!(c.roundtrip("Hcp-1.-1"), "OK");
    let stop = c.roundtrip("bc");
    // Mock returns TraceStart -> sig 0 ("at start").
    assert!(stop.starts_with("T00"), "stop reply: {}", stop);
    assert!(stop.contains("thread:p01.64;"), "stop reply: {}", stop);

    // Delve's reverse step: Hc<tid> + bs.
    assert_eq!(c.roundtrip("Hc64"), "OK");
    let stop = c.roundtrip("bs");
    assert!(stop.starts_with("T05"), "stop reply: {}", stop);

    drop(c);
    server.join().unwrap();
}

#[test]
fn test_trace_end_reports_sigkill() {
    // A mock that ends the trace on forward continue must report T09 so
    // Delve enters its "almost exited" state (can still rewind).
    struct EndTarget(MockTarget);
    impl DebugTarget for EndTarget {
        fn step(&mut self) -> Result<StopReason, DebugError> {
            self.0.step()
        }
        fn step_back(&mut self) -> Result<StopReason, DebugError> {
            self.0.step_back()
        }
        fn continue_forward(&mut self) -> Result<StopReason, DebugError> {
            Ok(StopReason::TraceEnd)
        }
        fn continue_backward(&mut self) -> Result<StopReason, DebugError> {
            self.0.continue_backward()
        }
        fn goto(&mut self, pos: TtdPosition) -> Result<StopReason, DebugError> {
            self.0.goto(pos)
        }
        fn read_memory(&self, addr: u64, buf: &mut [u8]) -> usize {
            self.0.read_memory(addr, buf)
        }
        fn set_breakpoint(&mut self, addr: u64, t: Option<u64>) -> u64 {
            self.0.set_breakpoint(addr, t)
        }
        fn set_data_breakpoint(&mut self, a: u64, s: u64, k: BreakpointKind) -> u64 {
            self.0.set_data_breakpoint(a, s, k)
        }
        fn remove_breakpoint(&mut self, id: u64) -> bool {
            self.0.remove_breakpoint(id)
        }
        fn active_thread_ids(&self) -> Vec<u64> {
            self.0.active_thread_ids()
        }
        fn thread_state(&self, id: Option<u64>) -> Option<(TtdX64Regs, u64)> {
            self.0.thread_state(id)
        }
        fn thread_info(&self, id: u64) -> Option<ThreadExtraInfoData> {
            self.0.thread_info(id)
        }
        fn current_thread_id(&self) -> Option<u64> {
            self.0.current_thread_id()
        }
        fn set_current_thread(&mut self, id: u64) {
            self.0.set_current_thread(id)
        }
        fn modules(&self) -> Vec<ModuleInfo> {
            vec![]
        }
        fn diagnostics(&self) -> TargetDiagnostics {
            self.0.diagnostics()
        }
        fn position(&self) -> TtdPosition {
            self.0.position()
        }
        fn lifetime(&self) -> (TtdPosition, TtdPosition) {
            self.0.lifetime()
        }
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let _ = run_gdb_session(EndTarget(MockTarget::new()), stream, &None, &None);
    });

    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);
    let stop = c.roundtrip("vCont;c");
    assert!(stop.starts_with("T09"), "stop reply: {}", stop);

    drop(c);
    server.join().unwrap();
}

#[test]
fn test_qrrcmd_when_and_checkpoints() {
    let (addr, server) = start_server(MockTarget::new());
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    // New-style qRRCmd (Delve uses it when thread suffixes are supported).
    let when = c.roundtrip("qRRCmd:when:-1");
    let when = String::from_utf8(from_hex(&when)).unwrap();
    assert_eq!(when, "1:0"); // position {sequence:1, steps:0}

    // Checkpoint at current position.
    let resp = c.roundtrip("qRRCmd:checkpoint:-1:");
    let resp = String::from_utf8(from_hex(&resp)).unwrap();
    assert!(resp.starts_with("Checkpoint 1 at "), "got: {}", resp);

    // List checkpoints: header line + tab-separated rows.
    let resp = c.roundtrip("qRRCmd:info checkpoints:-1");
    let resp = String::from_utf8(from_hex(&resp)).unwrap();
    let lines: Vec<&str> = resp.lines().collect();
    assert!(lines.len() >= 2, "got: {:?}", resp);
    let fields: Vec<&str> = lines[1].split('\t').collect();
    assert_eq!(fields.len(), 3, "got: {:?}", lines[1]);
    assert_eq!(fields[0], "1");
    assert_eq!(fields[1], "1:0");

    // Delete it.
    let resp = c.roundtrip("qRRCmd:delete checkpoint:-1:31"); // hex("1") = 31
    let resp = String::from_utf8(from_hex(&resp)).unwrap();
    assert_eq!(resp, "Deleted checkpoint 1");

    // Old-style (hex-encoded command) must also work.
    let when_hex: String = "when".bytes().map(|b| format!("{:02x}", b)).collect();
    let resp = c.roundtrip(&format!("qRRCmd:{}", when_hex));
    let resp = String::from_utf8(from_hex(&resp)).unwrap();
    assert_eq!(resp, "1:0");

    drop(c);
    server.join().unwrap();
}

#[test]
fn test_vrun_restart_position() {
    let (addr, server) = start_server(MockTarget::new());
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    // Restart at SEQ:STEPS = A:1F (Delve restart -> vRun;;<hex ascii>).
    let pos_ascii = "A:1F";
    let hexpos: String = pos_ascii.bytes().map(|b| format!("{:02x}", b)).collect();
    let stop = c.roundtrip(&format!("vRun;;{}", hexpos));
    assert!(stop.starts_with("T05"), "stop reply: {}", stop);

    // Verify the position moved.
    let when = c.roundtrip("qRRCmd:when:-1");
    let when = String::from_utf8(from_hex(&when)).unwrap();
    assert_eq!(when, "A:1F");

    // Restart from checkpoint: create one, then vRun to c1.
    c.roundtrip("qRRCmd:checkpoint:-1:");
    let hexpos: String = "c1".bytes().map(|b| format!("{:02x}", b)).collect();
    let stop = c.roundtrip(&format!("vRun;;{}", hexpos));
    assert!(stop.starts_with("T05"), "stop reply: {}", stop);
    let when = c.roundtrip("qRRCmd:when:-1");
    let when = String::from_utf8(from_hex(&when)).unwrap();
    assert_eq!(when, "A:1F"); // checkpoint was taken at A:1F

    // Plain vRun (no position) -> trace start.
    let stop = c.roundtrip("vRun;");
    assert!(stop.starts_with("T05"), "stop reply: {}", stop);
    let when = c.roundtrip("qRRCmd:when:-1");
    let when = String::from_utf8(from_hex(&when)).unwrap();
    assert_eq!(when, "0:0");

    drop(c);
    server.join().unwrap();
}

#[test]
fn test_detach_closes_session() {
    let (addr, server) = start_server(MockTarget::new());
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    assert_eq!(c.roundtrip("D"), "OK");
    drop(c);
    // Server must exit cleanly after detach.
    server.join().unwrap();
}

#[test]
fn test_auxv_uses_module_base() {
    // qXfer:auxv:read reports AT_ENTRY = the runtime base of the first
    // module. Delve reads this via EntryPointFromAuxv and computes
    // `StaticBase = entryPoint - opth.ImageBase`; for a non-ASLR PE
    // image this yields StaticBase = 0 (base == ImageBase), which is
    // exactly what Delve needs to relocate DWARF correctly. (We used to
    // report the real process entry point here, which broke symbol
    // relocation for Delve — the entry point is NOT the module base.)
    let modules = vec![ModuleInfo {
        base_addr: 0x0001_4000_0000,
        size: 0x10000,
        name: "test.exe".to_string(),
    }];
    let (addr, server) = start_server_modules(ModulesTarget::new(modules));
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    c.send("qXfer:auxv:read::0,40");
    let resp = c.recv();
    assert!(
        !resp.starts_with(b"E"),
        "auxv must not error when modules are present"
    );
    // qXfer reply format: 'l'/'m' prefix byte, then raw binary payload.
    assert!(
        resp[0] == b'l' || resp[0] == b'm',
        "auxv reply must start with l/m, got {:?}",
        resp[0] as char
    );
    let buf = &resp[1..];
    assert_eq!(buf.len(), 32, "expected 4 u64s, got {} bytes", buf.len());
    let tag = u64::from_le_bytes(buf[0..8].try_into().unwrap());
    let val = u64::from_le_bytes(buf[8..16].try_into().unwrap());
    assert_eq!(tag, 9, "first tag must be AT_ENTRY");
    assert_eq!(
        val, 0x0001_4000_0000,
        "AT_ENTRY must be the first module base"
    );
    let null_tag = u64::from_le_bytes(buf[16..24].try_into().unwrap());
    assert_eq!(null_tag, 0, "terminator must be AT_NULL");

    // Offset reads: a partial read at offset 8 should yield just the value.
    c.send("qXfer:auxv:read::8,8");
    let resp = c.recv();
    let buf = &resp[1..];
    assert_eq!(buf.len(), 8);
    let val = u64::from_le_bytes(buf[..8].try_into().unwrap());
    assert_eq!(val, 0x0001_4000_0000);

    drop(c);
    server.join().unwrap();
}

#[test]
fn test_auxv_errors_when_no_modules() {
    // The mock has no modules, so qXfer:auxv must return E (non-fatal).
    let (addr, server) = start_server(MockTarget::new());
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    let resp = c.roundtrip("qXfer:auxv:read::0,40");
    assert!(
        resp.starts_with('E'),
        "auxv must error when no modules: {resp}"
    );

    drop(c);
    server.join().unwrap();
}

#[test]
fn test_ctrl_c_reports_buffered_stop_reason() {
    // Regression test for a race in the GDB event loop: when the replay
    // worker finishes between the event loop's last `take_stop_reason`
    // poll and the `^C` arriving, the old `on_interrupt` saw no replay
    // in flight and returned `None`, leaving the stub in the `Running`
    // state with the stop reason buffered on the channel. The next
    // `wait_for_stop_reason` poll would eventually find the reason, but
    // the response was delayed by up to one poll interval (~5 ms). The
    // fix in `TtdEventLoop::on_interrupt` drains the channel first, so
    // a buffered stop reason is reported immediately on ^C.
    //
    // We can't easily observe the bug from a black-box test (the eventual
    // T05 arrives in both cases), so this test asserts the post-fix
    // contract: a ^C that races past a finished worker still results in
    // the worker's stop reason being delivered as a T05 reply.
    use std::sync::mpsc;
    use std::sync::Mutex;
    use std::time::Duration;

    struct GatedTarget {
        gate: Mutex<Option<mpsc::Receiver<()>>>,
        position: TtdPosition,
    }
    impl DebugTarget for GatedTarget {
        fn step(&mut self) -> Result<StopReason, DebugError> {
            Ok(StopReason::StepComplete)
        }
        fn step_back(&mut self) -> Result<StopReason, DebugError> {
            Ok(StopReason::StepComplete)
        }
        fn continue_forward(&mut self) -> Result<StopReason, DebugError> {
            // Block until the test releases the gate. While we wait, the
            // event loop polls `take_stop_reason` (returns nothing) and
            // `conn.peek()` (returns nothing because no ^C has arrived
            // yet) — so the replay is visibly in flight. When the test
            // releases the gate, the worker returns the breakpoint
            // reason, then the GdbTarget worker sets `replay_running =
            // false` and pushes the reason onto the channel. The narrow
            // window between those two operations is the race the fix
            // closes.
            let rx = self.gate.lock().unwrap().take().expect("gate consumed");
            let _ = rx.recv();
            Ok(StopReason::Breakpoint {
                bp_id: 1,
                addr: 0xdead,
            })
        }
        fn continue_backward(&mut self) -> Result<StopReason, DebugError> {
            Ok(StopReason::TraceStart)
        }
        fn goto(&mut self, pos: TtdPosition) -> Result<StopReason, DebugError> {
            self.position = pos;
            Ok(StopReason::PositionReached)
        }
        fn read_memory(&self, _addr: u64, buf: &mut [u8]) -> usize {
            buf.len()
        }
        fn set_breakpoint(&mut self, _addr: u64, _t: Option<u64>) -> u64 {
            1
        }
        fn set_data_breakpoint(&mut self, _a: u64, _s: u64, _k: BreakpointKind) -> u64 {
            1
        }
        fn remove_breakpoint(&mut self, _id: u64) -> bool {
            true
        }
        fn active_thread_ids(&self) -> Vec<u64> {
            vec![100]
        }
        fn thread_state(&self, _id: Option<u64>) -> Option<(TtdX64Regs, u64)> {
            Some((TtdX64Regs::default(), 0))
        }
        fn thread_info(&self, _id: u64) -> Option<ThreadExtraInfoData> {
            Some(ThreadExtraInfoData {
                unique_id: 1,
                current_position: TtdPosition {
                    sequence: 1,
                    steps: 0,
                },
                teb: 0,
            })
        }
        fn current_thread_id(&self) -> Option<u64> {
            Some(100)
        }
        fn set_current_thread(&mut self, _id: u64) {}
        fn modules(&self) -> Vec<ModuleInfo> {
            vec![]
        }
        fn position(&self) -> TtdPosition {
            self.position
        }
        fn diagnostics(&self) -> TargetDiagnostics {
            TargetDiagnostics {
                first_position: self.lifetime().0,
                last_position: self.lifetime().1,
                current_position: self.position,
                threads: vec![],
                exception_count: 0,
                module_count: 0,
            }
        }
        fn lifetime(&self) -> (TtdPosition, TtdPosition) {
            (
                TtdPosition {
                    sequence: 0,
                    steps: 0,
                },
                TtdPosition {
                    sequence: 9,
                    steps: 1000,
                },
            )
        }
    }

    let (tx, rx) = mpsc::channel();
    let target = GatedTarget {
        gate: Mutex::new(Some(rx)),
        position: TtdPosition {
            sequence: 1,
            steps: 0,
        },
    };

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let _ = run_gdb_session(target, stream, &None, &None);
    });

    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    // Start the replay — the worker spawns and blocks on the gate.
    c.send("vCont;c");
    // Let the event loop settle into the in-flight state.
    std::thread::sleep(Duration::from_millis(20));
    // Release the worker. It returns the Breakpoint reason, the
    // GdbTarget worker then sets `replay_running = false` and pushes the
    // stop reason. With a slow scheduler the on_interrupt triggered by
    // ^C may see `replay_in_flight` as already false; the fix's
    // `take_stop_reason` first still finds the buffered reason.
    tx.send(()).unwrap();
    // Send ^C. Whatever the timing, the response must be a stop reply
    // — either the worker's T05 (the Breakpoint reason it pushed) or
    // T02 (SIGINT, if ^C landed first and the stub then aborted the
    // worker). The bug being fixed is that a buffered T05 gets lost
    // entirely; the test is satisfied as long as we get a stop reply.
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

// ─── GSP coverage: qXfer:libraries:read + qXfer:memory-map:read ─────

/// Mock backend with a fixed module list — used to test the gdbstub
/// `Libraries` and `MemoryMap` impls end-to-end over the wire.
struct ModulesTarget {
    position: TtdPosition,
    modules: Vec<ModuleInfo>,
    teb: u64,
}
impl ModulesTarget {
    fn new(modules: Vec<ModuleInfo>) -> Self {
        Self {
            position: TtdPosition {
                sequence: 1,
                steps: 0,
            },
            modules,
            teb: 0x7ff7_0000_0000,
        }
    }
}
impl DebugTarget for ModulesTarget {
    fn step(&mut self) -> Result<StopReason, DebugError> {
        Ok(StopReason::StepComplete)
    }
    fn step_back(&mut self) -> Result<StopReason, DebugError> {
        Ok(StopReason::StepComplete)
    }
    fn continue_forward(&mut self) -> Result<StopReason, DebugError> {
        Ok(StopReason::TraceEnd)
    }
    fn continue_backward(&mut self) -> Result<StopReason, DebugError> {
        Ok(StopReason::TraceStart)
    }
    fn goto(&mut self, pos: TtdPosition) -> Result<StopReason, DebugError> {
        self.position = pos;
        Ok(StopReason::PositionReached)
    }
    fn read_memory(&self, _addr: u64, buf: &mut [u8]) -> usize {
        buf.len()
    }
    fn set_breakpoint(&mut self, _addr: u64, _t: Option<u64>) -> u64 {
        1
    }
    fn set_data_breakpoint(&mut self, _a: u64, _s: u64, _k: BreakpointKind) -> u64 {
        1
    }
    fn remove_breakpoint(&mut self, _id: u64) -> bool {
        true
    }
    fn active_thread_ids(&self) -> Vec<u64> {
        vec![100]
    }
    fn thread_state(&self, _id: Option<u64>) -> Option<(TtdX64Regs, u64)> {
        Some((TtdX64Regs::default(), self.teb))
    }
    fn thread_info(&self, _id: u64) -> Option<ThreadExtraInfoData> {
        Some(ThreadExtraInfoData {
            unique_id: 1,
            current_position: self.position,
            teb: self.teb,
        })
    }
    fn current_thread_id(&self) -> Option<u64> {
        Some(100)
    }
    fn set_current_thread(&mut self, _id: u64) {}
    fn modules(&self) -> Vec<ModuleInfo> {
        self.modules.clone()
    }
    fn diagnostics(&self) -> TargetDiagnostics {
        TargetDiagnostics {
            first_position: self.lifetime().0,
            last_position: self.lifetime().1,
            current_position: self.position,
            threads: vec![ThreadDiagnostics {
                unique_id: 1,
                os_thread_id: 100,
                active_time: (self.position, self.lifetime().1),
            }],
            exception_count: 0,
            module_count: self.modules.len() as u32,
        }
    }
    fn position(&self) -> TtdPosition {
        self.position
    }
    fn lifetime(&self) -> (TtdPosition, TtdPosition) {
        (
            TtdPosition {
                sequence: 0,
                steps: 0,
            },
            TtdPosition {
                sequence: 9,
                steps: 1000,
            },
        )
    }
}

fn start_server_modules(target: ModulesTarget) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let _ = run_gdb_session(target, stream, &None, &Some("test.exe".to_string()));
    });
    (addr, handle)
}

#[test]
fn test_qxfer_libraries_uses_module_list() {
    let modules = vec![
        ModuleInfo {
            base_addr: 0x140000000,
            size: 0x10000,
            name: "C:\\Windows\\System32\\ntdll.dll".to_string(),
        },
        ModuleInfo {
            base_addr: 0x7ff6_0000_0000,
            size: 0x200000,
            name: "C:\\path with space\\test.exe".to_string(),
        },
    ];
    let (addr, server) = start_server_modules(ModulesTarget::new(modules));
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    // qSupported must now advertise the new features.
    let supported = c.roundtrip("qSupported:multiprocess+;swbreak+;hwbreak+;no-resumed+");
    assert!(
        supported.contains("qXfer:libraries:read+"),
        "qSupported must advertise qXfer:libraries:read+: {supported}"
    );
    assert!(
        supported.contains("qXfer:memory-map:read+"),
        "qSupported must advertise qXfer:memory-map:read+: {supported}"
    );

    // First read: full XML, single packet.
    c.send("qXfer:libraries:read::0,fff");
    let mut body = c.recv();
    assert!(
        body[0] == b'l' || body[0] == b'm',
        "libraries reply must start with l/m, got {:?}",
        body[0] as char
    );
    let xml = std::str::from_utf8(&body[1..]).expect("libraries XML must be UTF-8");
    assert!(xml.starts_with("<library-list"), "got: {xml}");
    assert!(
        xml.contains("name=\"C:\\Windows\\System32\\ntdll.dll\""),
        "ntdll.dll entry missing: {xml}"
    );
    assert!(
        xml.contains("name=\"C:\\path with space\\test.exe\""),
        "test.exe entry missing: {xml}"
    );
    assert!(
        xml.contains("<segment address=\"0x140000000\"/>"),
        "ntdll address missing: {xml}"
    );
    assert!(
        xml.contains("<segment address=\"0x7ff600000000\"/>"),
        "test.exe address missing: {xml}"
    );
    assert!(xml.ends_with("</library-list>"));

    // Partial read at offset past the end must report `l` (last/empty chunk).
    c.send("qXfer:libraries:read::ffff,fff");
    body = c.recv();
    assert_eq!(body, b"l", "offset past end must be l-only");

    // Partial read at a non-zero offset must return a slice.
    c.send("qXfer:libraries:read::10,5");
    body = c.recv();
    let xml = std::str::from_utf8(&body[1..]).expect("slice must be UTF-8");
    assert_eq!(xml.len(), 5);

    drop(c);
    server.join().unwrap();
}

#[test]
fn test_qxfer_memory_map_uses_module_list() {
    let modules = vec![
        ModuleInfo {
            base_addr: 0x1000,
            size: 0x5000,
            name: "a.dll".to_string(),
        },
        ModuleInfo {
            base_addr: 0x7ff6_0000_0000,
            size: 0x200000,
            name: "b.exe".to_string(),
        },
    ];
    let (addr, server) = start_server_modules(ModulesTarget::new(modules));
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    c.send("qXfer:memory-map:read::0,fff");
    let body = c.recv();
    let xml = std::str::from_utf8(&body[1..]).expect("memory-map XML must be UTF-8");
    assert!(xml.starts_with("<memory-map"), "got: {xml}");
    assert!(
        xml.contains("<memory type=\"ram\" start=\"0x1000\" length=\"0x5000\"/>"),
        "a.dll region missing: {xml}"
    );
    assert!(
        xml.contains("<memory type=\"ram\" start=\"0x7ff600000000\" length=\"0x200000\"/>"),
        "b.exe region missing: {xml}"
    );
    assert!(xml.ends_with("</memory-map>"));

    drop(c);
    server.join().unwrap();
}

// ─── GSP coverage: qThreadExtraInfo ──────────────────────────────

#[test]
fn test_qthread_extra_info_uses_unique_id_and_position() {
    // The qThreadExtraInfo reply is what gdb shows in `info threads` as
    // `Thread N.M (reply)`. Our reply includes TTD's UniqueId (stable for
    // the whole trace) and the thread's current trace position so the
    // user can navigate the time axis.
    let (addr, server) = start_server(MockTarget::new());
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    // qSupported does not need to advertise qThreadExtraInfo+ in
    // gdbstub 0.7.10 (the handler is invoked whenever gdb asks); we just
    // verify the packet is honored and the reply is well-formed.

    // Select thread 100 (the mock's only thread) and query its extra info.
    // Send a bare `+` first because gdb starts each command with an ack.
    // The packet format is `qThreadExtraInfo,p<pid>.<tid>` (the leading
    // comma is the multi-process prefix gdbstub requires).
    c.stream.write_all(b"+").unwrap();
    c.send("qThreadExtraInfo,p1.64");
    let body = c.recv();
    let s = String::from_utf8(from_hex(std::str::from_utf8(&body).unwrap()))
        .expect("qThreadExtraInfo reply must be hex UTF-8");
    assert!(
        s.contains("UTID 1") && s.contains("OS 0x64") && s.contains("pos 1:0"),
        "expected UTID 1 / OS 0x64 / pos 1:0 in: {s}"
    );

    drop(c);
    server.join().unwrap();
}

// ─── GSP coverage: qGetTLSAddr (TEB) ─────────────────────────────

#[test]
fn test_qget_tls_addr_returns_teb() {
    // On Windows the TLS address is the TEB, which we already expose as
    // the gs_base register. gdb's qGetTLSAddr:p<pid>.<tid>,<offset>,<lm>
    // asks for it; we ignore <offset> and <lm> and return the TEB.
    let (addr, server) = start_server(MockTarget::new());
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    // The mock's `thread_info` reports teb = 0x7ffe_0000.
    c.stream.write_all(b"+").unwrap();
    c.send("qGetTLSAddr:p1.64,0,0");
    let body = c.recv();
    let s = String::from_utf8_lossy(&body);
    assert_eq!(s, "7ffe0000", "qGetTLSAddr must return the TEB address");

    // Bogus thread id (no info) must return an error packet.
    c.stream.write_all(b"+").unwrap();
    c.send("qGetTLSAddr:p1.dead,0,0");
    let body = c.recv();
    let s = String::from_utf8_lossy(&body);
    assert_eq!(s, "E01", "unknown thread must return E01");

    drop(c);
    server.join().unwrap();
}

// ─── GSP coverage: qOffsets (SectionOffsets) ──────────────────────

#[test]
fn test_qoffsets_uses_main_module_base() {
    // qOffsets reports the section/segment relocation offsets. For PE
    // executables we report the image base as both text and data
    // segments; combined with the real entry point in qXfer:auxv, gdb
    // relocates symbols correctly.
    let modules = vec![ModuleInfo {
        base_addr: 0x0001_4000_0000,
        size: 0x10000,
        name: "test.exe".to_string(),
    }];
    let (addr, server) = start_server_modules(ModulesTarget::new(modules));
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    c.stream.write_all(b"+").unwrap();
    c.send("qOffsets");
    let body = c.recv();
    let s = String::from_utf8_lossy(&body);
    // gdbstub's `write_num` emits hex values with a single leading `0`
    // when the value is too wide for the natural hex width. The exact
    // format is gdbstub-version-dependent, so we just check both
    // segments are present with the right module base.
    assert!(
        s.contains("TextSeg=0140000000") && s.contains("DataSeg=0140000000"),
        "qOffsets must report main module base: {s}"
    );

    drop(c);
    server.join().unwrap();
}

// ─── GSP coverage: vCont;r (range step) ──────────────────────────

#[test]
fn test_vcont_range_step_routes_to_step_for_empty_range() {
    // gdb's range step (vCont;r:start,end) means "step while PC is in
    // [start, end)". TTD can't predict the exit point, so we
    // single-step until the PC leaves the range. The mock's step is a
    // no-op (just bumps `position.steps`); an empty range (start ==
    // end) is a GSP-defined single-step equivalent, so we expect T05
    // and a thread reply — the same shape as vCont;s:64.
    let (addr, server) = start_server(MockTarget::new());
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    // vCont doesn't ack separately: the next packet is the stop reply.
    c.stream.write_all(b"+").unwrap();
    c.send("vCont;r1000,1000:64");
    let stop = c.recv();
    let s = String::from_utf8_lossy(&stop);
    assert!(
        s.starts_with("T05") && s.contains("thread:p01.64;"),
        "empty range must behave like a single step, got: {s}"
    );

    drop(c);
    server.join().unwrap();
}

// ─── GSP coverage: QPassSignals / QProgramSignals / vCtrlC ──────────

#[test]
fn test_qpass_signals_acks_ok() {
    // gdb may enumerate which signals it wants delivered. Replay is
    // read-only; we ACK without doing anything.
    let (addr, server) = start_server(MockTarget::new());
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    c.stream.write_all(b"+").unwrap();
    c.send("QPassSignals:0203");
    let body = c.recv();
    assert_eq!(String::from_utf8_lossy(&body), "OK");

    c.stream.write_all(b"+").unwrap();
    c.send("QProgramSignals:0203");
    let body = c.recv();
    assert_eq!(String::from_utf8_lossy(&body), "OK");

    drop(c);
    server.join().unwrap();
}

#[test]
fn test_vctrlc_acks_ok() {
    // vCtrlC is extended-remote's preferred ^C; the stub must ACK so
    // the client knows the signal was registered. (The actual
    // interruption happens via BlockingEventLoop::on_interrupt.)
    let (addr, server) = start_server(MockTarget::new());
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    c.stream.write_all(b"+").unwrap();
    c.send("vCtrlC");
    let body = c.recv();
    assert_eq!(String::from_utf8_lossy(&body), "OK");

    drop(c);
    server.join().unwrap();
}

// ─── TTD custom commands (monitor ttd …) ──────────────────────────

/// Send a `qRcmd` packet with the given UTF-8 command body (gdb
/// hex-encodes the command after the comma). The reply may be one or
/// more `O<hex>` packets followed by a final `OK`; we accumulate all
/// `O<hex>` chunks into a single decoded string.
fn send_monitor(c: &mut RspClient, cmd: &str) -> String {
    let hex: String = cmd.bytes().map(|b| format!("{:02x}", b)).collect();
    c.stream.write_all(b"+").unwrap();
    c.send(&format!("qRcmd,{hex}"));
    let mut text = String::new();
    loop {
        let body = c.recv();
        let s = std::str::from_utf8(&body).unwrap_or("");
        if s == "OK" {
            break;
        }
        if let Some(rest) = s.strip_prefix('O') {
            // Hex-decode and append.
            if !text.is_empty() {
                // gdb merges adjacent `O<hex>` packets into one console
                // line at the user level; we don't need to add a separator.
            }
            if rest.len() % 2 == 0 && rest.chars().all(|c| c.is_ascii_hexdigit()) {
                let decoded = from_hex(rest);
                text.push_str(&String::from_utf8_lossy(&decoded));
            } else {
                text.push_str(rest);
            }
        } else {
            // Non-`O` reply; surface as-is.
            text.push_str(s);
            break;
        }
    }
    text
}

#[test]
fn test_monitor_ttd_info() {
    let (addr, server) = start_server(MockTarget::new());
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    let s = send_monitor(&mut c, "ttd info");
    assert!(s.contains("trace: loaded"), "got: {s}");
    assert!(s.contains("lifetime:"), "got: {s}");
    assert!(s.contains("current: 1:0"), "got: {s}");
    assert!(s.contains("modules: 0"), "got: {s}");
    assert!(s.contains("threads (lifetime): 1"), "got: {s}");
    assert!(s.contains("exception events:"), "got: {s}");

    drop(c);
    server.join().unwrap();
}

#[test]
fn test_monitor_ttd_threads() {
    let (addr, server) = start_server(MockTarget::new());
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    let s = send_monitor(&mut c, "ttd threads");
    // The mock has one thread (UTID 1, OS 0x64, active from current to end).
    assert!(s.contains("UTID"), "header missing: {s}");
    assert!(s.contains("OS_TID"), "header missing: {s}");
    assert!(
        s.contains("1") && s.contains("0x00000064"),
        "row missing: {s}"
    );

    drop(c);
    server.join().unwrap();
}

#[test]
fn test_monitor_ttd_module_resolves_address() {
    let modules = vec![
        ModuleInfo {
            base_addr: 0x10000,
            size: 0x5000,
            name: "a.dll".to_string(),
        },
        ModuleInfo {
            base_addr: 0x0001_4000_0000,
            size: 0x10000,
            name: "b.exe".to_string(),
        },
    ];
    let (addr, server) = start_server_modules(ModulesTarget::new(modules));
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    // Hit inside a module.
    let s = send_monitor(&mut c, "ttd module 0x140000100");
    assert!(
        s.contains("0x140000000..0x140010000") && s.contains("b.exe"),
        "expected b.exe range, got: {s}"
    );

    // Miss.
    let s = send_monitor(&mut c, "ttd module 0xdeadbeef");
    assert!(s.contains("no module contains"), "got: {s}");

    // Bad address syntax.
    let s = send_monitor(&mut c, "ttd module xyz");
    assert!(s.contains("bad address"), "got: {s}");

    drop(c);
    server.join().unwrap();
}

#[test]
fn test_monitor_unknown_subcommand_prints_help() {
    let (addr, server) = start_server(MockTarget::new());
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    let s = send_monitor(&mut c, "ttd bogus");
    assert!(s.contains("unknown subcommand 'bogus'"), "got: {s}");
    assert!(s.contains("ttd info"), "got: {s}");

    // Non-ttd subcommand also prints help.
    let s = send_monitor(&mut c, "wat");
    assert!(s.contains("unknown monitor subcommand"), "got: {s}");

    drop(c);
    server.join().unwrap();
}
