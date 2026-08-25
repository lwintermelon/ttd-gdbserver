//! Trace introspection over the wire: `qXfer` auxv/libraries, the declined
//! memory-map annex, `qThreadExtraInfo`, `qGetTLSAddr`, and `qOffsets`.

mod common;

use ttd_gdbserver::target::ModuleInfo;

use common::harness::TestSession;
use common::mock_target::MockTarget;
use common::rsp::from_hex;

#[test]
fn test_auxv_uses_module_base() {
    // qXfer:auxv:read reports AT_ENTRY = the runtime base of the first
    // module. Delve reads this via EntryPointFromAuxv and computes
    // `StaticBase = entryPoint - opth.ImageBase`; for a non-ASLR PE image
    // this yields StaticBase = 0 (base == ImageBase), which is exactly what
    // Delve needs to relocate DWARF correctly. AT_ENTRY here is the module
    // base, not the real process entry point — reporting the entry point
    // breaks symbol relocation for Delve.
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
    let first = *resp.first().expect("auxv reply must not be empty");
    assert!(
        first == b'l' || first == b'm',
        "auxv reply must start with l/m, got {:?}",
        first as char
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
fn test_qxfer_libraries_lists_shared_modules_only() {
    // `modules()` puts the main executable first (qOffsets/auxv rely on it).
    // It must NOT appear in the library list: GDB already has it from
    // `qXfer:exec-file`, and a library entry for it would make GDB re-open
    // the trace's recorded path instead of the client-supplied executable.
    let modules = vec![
        ModuleInfo {
            base_addr: 0x7ff6_0000_0000,
            size: 0x20_0000,
            name: r"C:\path with space\test.exe".to_string(),
        },
        ModuleInfo {
            base_addr: 0x1_4000_0000,
            size: 0x10000,
            name: r"C:\Windows\System32\ntdll.dll".to_string(),
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
        !supported.contains("qXfer:memory-map:read+"),
        "qSupported must not advertise a module-only memory map (GDB would stop reading stack): {supported}"
    );

    // First read: full XML, single packet.
    c.send("qXfer:libraries:read::0,fff");
    let mut body = c.recv();
    let first = *body.first().expect("libraries reply must not be empty");
    assert!(
        first == b'l' || first == b'm',
        "libraries reply must start with l/m, got {:?}",
        first as char
    );
    let xml = std::str::from_utf8(&body[1..]).expect("libraries XML must be UTF-8");
    assert!(xml.starts_with("<library-list"), "got: {xml}");
    assert!(
        xml.contains(r#"name="C:\Windows\System32\ntdll.dll""#),
        "ntdll.dll entry missing: {xml}"
    );
    assert!(
        !xml.contains("test.exe"),
        "the main executable must not be listed as a shared library: {xml}"
    );
    assert!(
        xml.contains(r#"<segment address="0x140001000"/>"#),
        "ntdll address missing: {xml}"
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
fn test_qxfer_memory_map_is_not_advertised() {
    let mut session = TestSession::start(MockTarget::new());
    session.handshake();
    let c = &mut session.client;

    // GDB would treat an advertised map as the complete readable set and stop
    // sending `m` for stack addresses outside it. Falling back to remote
    // reads is what lets the positioned cursor serve C locals.
    c.send("qXfer:memory-map:read::0,fff");
    let body = c.recv();
    assert!(
        !body.starts_with(b"m") && !body.starts_with(b"l"),
        "memory-map must not be advertised, got: {body:?}"
    );

    session.finish();
}

/// The qThreadExtraInfo reply is what gdb shows in `info threads` as
/// `Thread N.M (reply)`. Our reply includes TTD's `UniqueId` (stable for the
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

/// On Windows GDB asks for the TIB (TEB) with `qGetTIBAddr` and derives TLS
/// from it; `qGetTLSAddr` is the generic form. Both return the thread's TEB.
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

    c.ack();
    c.send("qGetTIBAddr:p1.64");
    let body = c.recv();
    assert_eq!(
        String::from_utf8_lossy(&body),
        "7ffe0000",
        "qGetTIBAddr must return the TEB address"
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

/// qOffsets reports the runtime address of the text segment, not the image
/// base. GDB relocates `.text` to this address and keeps the rest of the PE
/// image at fixed offsets; reporting the base made every symbol 0x1000 low.
#[test]
fn test_qoffsets_reports_the_text_segment() {
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
    // when the value is too wide for the natural hex width. The executable
    // is not on disk under the test name, so the parser falls back to the
    // conventional PE text RVA (0x1000).
    assert!(
        s.contains("TextSeg=0140001000"),
        "qOffsets must report the text segment address: {s}"
    );
    assert!(
        !s.contains("DataSeg="),
        "qOffsets must leave data sections at the text-relative offset: {s}"
    );

    session.finish();
}
