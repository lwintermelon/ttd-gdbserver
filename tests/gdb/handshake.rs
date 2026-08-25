use crate::support::*;

#[test]
fn test_handshake() {
    let mut session = TestSession::start(MockTarget::new());
    session.handshake();
    session.finish();
}

#[test]
fn test_thread_listing_and_process_info() {
    let mut session = TestSession::start(MockTarget::new());
    session.handshake();
    let c = &mut session.client;

    // tid 100 = 0x64; gdbstub negotiates multiprocess with Delve, so thread
    // ids carry the p<pid>. prefix (FAKE_PID = 1).
    assert_eq!(c.roundtrip("qfThreadInfo"), "mp01.64");
    assert_eq!(c.roundtrip("qsThreadInfo"), "l");

    let pi = c.roundtrip("qProcessInfo");
    assert!(pi.starts_with("pid:"), "qProcessInfo: {pi}");
    assert!(pi.contains("ptrsize:8"), "qProcessInfo: {pi}");

    let _ = c.roundtrip("qAttached"); // "1" or whatever the stub reports
    assert_eq!(c.roundtrip("qC"), "");

    session.finish();
}

#[test]
fn test_registers_and_memory() {
    let mut session = TestSession::start(MockTarget::new());
    session.handshake();
    let c = &mut session.client;

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
        "write resp: {resp}"
    );
    let resp = c.roundtrip("P10=ffffffffffffffff");
    assert!(
        resp.is_empty() || resp.starts_with('E'),
        "write resp: {resp}"
    );

    session.finish();
}

#[test]
fn test_detach_closes_session() {
    let mut session = TestSession::start(MockTarget::new());
    session.handshake();

    assert_eq!(session.client.roundtrip("D"), "OK");
    // `finish` closes the client and joins; the server must exit cleanly.
    session.finish();
}
