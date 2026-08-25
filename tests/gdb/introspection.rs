use crate::support::*;

#[test]
fn test_auxv_uses_module_base() {
    // qXfer:auxv:read reports AT_ENTRY = the runtime base of the first
    // module. Delve reads this via EntryPointFromAuxv and computes
    // `StaticBase = entryPoint - opth.ImageBase`; for a non-ASLR PE image
    // this yields StaticBase = 0 (base == ImageBase), which is exactly what
    // Delve needs to relocate DWARF correctly. (We used to report the real
    // process entry point here, which broke symbol relocation for Delve —
    // the entry point is NOT the module base.)
    let modules = vec![ModuleInfo {
        base_addr: 0x0001_4000_0000,
        size: 0x10000,
        name: "test.exe".to_string(),
    }];
    let mut session = TestSession::start(MockTarget::new().with_modules(modules));
    session.handshake();
    let c = &mut session.client;

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

    session.finish();
}

#[test]
fn test_auxv_errors_when_no_modules() {
    // The mock has no modules, so qXfer:auxv must return E (non-fatal).
    let mut session = TestSession::start(MockTarget::new());
    session.handshake();

    let resp = session.client.roundtrip("qXfer:auxv:read::0,40");
    assert!(
        resp.starts_with('E'),
        "auxv must error when no modules: {resp}"
    );

    session.finish();
}

#[test]
fn test_qxfer_libraries_uses_module_list() {
    let modules = vec![
        ModuleInfo {
            base_addr: 0x140000000,
            size: 0x10000,
            name: r"C:\Windows\System32\ntdll.dll".to_string(),
        },
        ModuleInfo {
            base_addr: 0x7ff6_0000_0000,
            size: 0x200000,
            name: r"C:\path with space\test.exe".to_string(),
        },
    ];
    let mut session = TestSession::start(MockTarget::new().with_modules(modules));
    session.handshake();
    let c = &mut session.client;

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
        xml.contains(r#"name="C:\Windows\System32\ntdll.dll""#),
        "ntdll.dll entry missing: {xml}"
    );
    assert!(
        xml.contains(r#"name="C:\path with space\test.exe""#),
        "test.exe entry missing: {xml}"
    );
    assert!(
        xml.contains(r#"<segment address="0x140000000"/>"#),
        "ntdll address missing: {xml}"
    );
    assert!(
        xml.contains(r#"<segment address="0x7ff600000000"/>"#),
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

    session.finish();
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
    let mut session = TestSession::start(MockTarget::new().with_modules(modules));
    session.handshake();
    let c = &mut session.client;

    c.send("qXfer:memory-map:read::0,fff");
    let body = c.recv();
    let xml = std::str::from_utf8(&body[1..]).expect("memory-map XML must be UTF-8");
    assert!(xml.starts_with("<memory-map"), "got: {xml}");
    assert!(
        xml.contains(r#"<memory type="ram" start="0x1000" length="0x5000"/>"#),
        "a.dll region missing: {xml}"
    );
    assert!(
        xml.contains(r#"<memory type="ram" start="0x7ff600000000" length="0x200000"/>"#),
        "b.exe region missing: {xml}"
    );
    assert!(xml.ends_with("</memory-map>"));

    session.finish();
}

/// The qThreadExtraInfo reply is what gdb shows in `info threads` as
/// `Thread N.M (reply)`. Our reply includes TTD's UniqueId (stable for the
/// whole trace) and the thread's current trace position so the user can
/// navigate the time axis.
#[test]
fn test_qthread_extra_info_uses_unique_id_and_position() {
    let mut session = TestSession::start(MockTarget::new());
    session.handshake();
    let c = &mut session.client;

    // Select thread 100 (the mock's only thread) and query its extra info.
    // Send a bare `+` first because gdb starts each command with an ack.
    // The packet format is `qThreadExtraInfo,p<pid>.<tid>` (the leading
    // comma is the multi-process prefix gdbstub requires).
    c.ack();
    c.send("qThreadExtraInfo,p1.64");
    let body = c.recv();
    let s = String::from_utf8(from_hex(std::str::from_utf8(&body).unwrap()))
        .expect("qThreadExtraInfo reply must be hex UTF-8");
    assert!(
        s.contains("UTID 1") && s.contains("OS 0x64") && s.contains("pos 1:0"),
        "expected UTID 1 / OS 0x64 / pos 1:0 in: {s}"
    );

    session.finish();
}

/// On Windows the TLS address is the TEB, which we already expose as the
/// gs_base register. gdb's `qGetTLSAddr:p<pid>.<tid>,<offset>,<lm>` asks for
/// it; we ignore `<offset>` and `<lm>` and return the TEB.
#[test]
fn test_qget_tls_addr_returns_teb() {
    let mut session = TestSession::start(MockTarget::new());
    session.handshake();
    let c = &mut session.client;

    // The mock's `thread_info` reports teb = 0x7ffe_0000.
    c.ack();
    c.send("qGetTLSAddr:p1.64,0,0");
    let body = c.recv();
    assert_eq!(
        String::from_utf8_lossy(&body),
        "7ffe0000",
        "qGetTLSAddr must return the TEB address"
    );

    // Bogus thread id (no info) must return an error packet.
    c.ack();
    c.send("qGetTLSAddr:p1.dead,0,0");
    let body = c.recv();
    assert_eq!(
        String::from_utf8_lossy(&body),
        "E01",
        "unknown thread must return E01"
    );

    session.finish();
}

/// qOffsets reports the section/segment relocation offsets. For PE
/// executables we report the image base as both text and data segments;
/// combined with the real entry point in qXfer:auxv, gdb relocates symbols
/// correctly.
#[test]
fn test_qoffsets_uses_main_module_base() {
    let modules = vec![ModuleInfo {
        base_addr: 0x0001_4000_0000,
        size: 0x10000,
        name: "test.exe".to_string(),
    }];
    let mut session = TestSession::start(MockTarget::new().with_modules(modules));
    session.handshake();
    let c = &mut session.client;

    c.ack();
    c.send("qOffsets");
    let body = c.recv();
    let s = String::from_utf8_lossy(&body);
    // gdbstub's `write_num` emits hex values with a single leading `0`
    // when the value is too wide for the natural hex width. The exact
    // format is gdbstub-version-dependent, so we just check both segments
    // are present with the right module base.
    assert!(
        s.contains("TextSeg=0140000000") && s.contains("DataSeg=0140000000"),
        "qOffsets must report main module base: {s}"
    );

    session.finish();
}
