use crate::support::*;

#[test]
fn test_monitor_ttd_info() {
    let mut session = TestSession::start(MockTarget::new());
    session.handshake();

    let s = session.client.monitor("ttd info");
    assert!(s.contains("trace: loaded"), "got: {s}");
    assert!(s.contains("lifetime:"), "got: {s}");
    assert!(s.contains("current: 1:0"), "got: {s}");
    assert!(s.contains("modules: 0"), "got: {s}");
    assert!(s.contains("threads (lifetime): 1"), "got: {s}");
    assert!(s.contains("exception events:"), "got: {s}");

    session.finish();
}

#[test]
fn test_monitor_ttd_threads() {
    let mut session = TestSession::start(MockTarget::new());
    session.handshake();

    let s = session.client.monitor("ttd threads");
    // The mock has one thread (UTID 1, OS 0x64, active from current to end).
    assert!(s.contains("UTID"), "header missing: {s}");
    assert!(s.contains("OS_TID"), "header missing: {s}");
    assert!(
        s.contains('1') && s.contains("0x00000064"),
        "row missing: {s}"
    );

    session.finish();
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
    let mut session = TestSession::start(MockTarget::new().with_modules(modules));
    session.handshake();
    let c = &mut session.client;

    // Hit inside a module.
    let s = c.monitor("ttd module 0x140000100");
    assert!(
        s.contains("0x140000000..0x140010000") && s.contains("b.exe"),
        "expected b.exe range, got: {s}"
    );

    // Miss.
    let s = c.monitor("ttd module 0xdeadbeef");
    assert!(s.contains("no module contains"), "got: {s}");

    // Bad address syntax.
    let s = c.monitor("ttd module xyz");
    assert!(s.contains("bad address"), "got: {s}");

    session.finish();
}

#[test]
fn test_monitor_unknown_subcommand_prints_help() {
    let mut session = TestSession::start(MockTarget::new());
    session.handshake();
    let c = &mut session.client;

    let s = c.monitor("ttd bogus");
    assert!(s.contains("unknown subcommand 'bogus'"), "got: {s}");
    assert!(s.contains("ttd info"), "got: {s}");

    // Non-ttd subcommand also prints help.
    let s = c.monitor("wat");
    assert!(s.contains("unknown monitor subcommand"), "got: {s}");

    session.finish();
}
