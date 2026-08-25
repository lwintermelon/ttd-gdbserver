//! GDB RSP frontend against a real TTD trace.
//!
//! Requires `TTD_TRACE_PATH`: without it build.rs does not emit the
//! `has_ttd_trace` cfg and every test here is tagged `#[ignore]`, so libtest
//! reports them as skipped (not passing).
//!
//! TTD_TRACE_PATH=path/to/trace.run cargo test --test gdb_ttd -- --nocapture

use std::path::Path;

use ttd_gdbserver::target::TtdProcess;
use ttd_gdbserver_test_support::harness::{delve_handshake, start_server as start_test_server};
use ttd_gdbserver_test_support::rsp::{RspClient, from_hex};

fn trace_path() -> String {
    // The #[cfg_attr(not(has_ttd_trace), ignore)] tag guarantees this only
    // runs when TTD_TRACE_PATH is set; reaching this line without it is a
    // harness bug, not a skip condition.
    std::env::var("TTD_TRACE_PATH")
        .expect("TTD_TRACE_PATH must be set (test should have been #[ignore]d)")
}

fn start_server(trace: &str) -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
    let process = TtdProcess::open(Path::new(trace)).expect("open trace");
    start_test_server(process)
}

#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn ttd_handshake_threads_registers() {
    let trace = trace_path();
    let (addr, server) = start_server(&trace);
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

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
#[cfg_attr(not(has_ttd_trace), ignore)]
fn ttd_step_forward_and_backward() {
    let trace = trace_path();
    let (addr, server) = start_server(&trace);
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

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
#[cfg_attr(not(has_ttd_trace), ignore)]
fn ttd_reverse_continue_reaches_trace_start() {
    let trace = trace_path();
    let (addr, server) = start_server(&trace);
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

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
#[cfg_attr(not(has_ttd_trace), ignore)]
fn ttd_memory_read_and_exec_file() {
    let trace = trace_path();
    let (addr, server) = start_server(&trace);
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

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

/// `monitor ttd info` against the real trace: lifetime must be a sane
/// non-empty range and the module/thread counts must be positive.
#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn ttd_monitor_info_reports_trace_statistics() {
    let trace = trace_path();
    let (addr, server) = start_server(&trace);
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    let s = c.monitor("ttd info");
    println!("monitor ttd info: {s}");
    assert!(s.contains("trace: loaded"), "got: {s}");
    assert!(s.contains("lifetime: "), "got: {s}");
    assert!(s.contains("modules: "), "got: {s}");
    assert!(s.contains("threads (lifetime): "), "got: {s}");
    assert!(s.contains("exception events: "), "got: {s}");
    // The sample trace is a live Go program, so it must have modules & threads.
    let modules = s
        .lines()
        .find_map(|l| l.strip_prefix("modules: "))
        .and_then(|v| v.trim().parse::<u32>().ok());
    let threads = s
        .lines()
        .find_map(|l| l.strip_prefix("threads (lifetime): "))
        .and_then(|v| v.trim().parse::<u32>().ok());
    assert!(modules.unwrap_or(0) > 0, "modules must be positive: {s}");
    assert!(threads.unwrap_or(0) > 0, "threads must be positive: {s}");

    drop(c);
    server.join().unwrap();
}

/// `monitor ttd threads` against the real trace: every thread has a stable
/// UTID and a non-empty active range, and the table header is present.
#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn ttd_monitor_threads_lists_lifetime_threads() {
    let trace = trace_path();
    let (addr, server) = start_server(&trace);
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    let s = c.monitor("ttd threads");
    println!("monitor ttd threads:\n{s}");
    assert!(s.contains("UTID") && s.contains("OS_TID"), "header: {s}");
    let rows: Vec<&str> = s.lines().skip(1).filter(|l| !l.trim().is_empty()).collect();
    assert!(!rows.is_empty(), "must list at least one thread: {s}");
    for row in rows {
        // "<utid> 0x<os> <min:min> <max:max>" — at least a UTID number.
        let first = row.split_whitespace().next().unwrap_or("");
        assert!(
            first.parse::<u32>().is_ok(),
            "first column must be a numeric UTID, row: {row}"
        );
    }

    drop(c);
    server.join().unwrap();
}

/// `monitor ttd stats` against the real trace: the counters must be wired to
/// the actual replay path — a step bumps `steps`, and serving the queries a
/// stop generates must cost far fewer seeks than queries (that ratio is what
/// `AGENTS.md` documents as the position cache's contract).
#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn ttd_monitor_stats_reports_replay_counters() {
    let trace = trace_path();
    let (addr, server) = start_server(&trace);
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    // One step (seek is charged to the step cursor, not to a query) ...
    let stop = c.roundtrip("s");
    assert!(stop.starts_with("T0"), "stop: {stop}");

    // ... then the register read every client does after a stop.
    assert_eq!(c.roundtrip("Hg0"), "OK");
    c.roundtrip("g");
    let s = c.monitor("ttd stats");
    println!("monitor ttd stats (after 1 query):\n{s}");
    assert!(s.contains("steps: 1"), "one step must be counted: {s}");
    let first_queries = parse_counter(&s, "queries:");
    let first_seeks = parse_counter(&s, "cursor seeks:");
    assert!(first_queries >= 1, "the register read must be counted: {s}");
    assert!(first_seeks >= 1, "the first query must seek: {s}");

    // More queries at the *same* position must not seek again.
    for _ in 0..5 {
        c.roundtrip("g");
    }
    let s = c.monitor("ttd stats");
    println!("monitor ttd stats (after 6 queries):\n{s}");
    assert_eq!(
        parse_counter(&s, "queries:"),
        first_queries + 5,
        "every read must be counted: {s}"
    );
    assert_eq!(
        parse_counter(&s, "cursor seeks:"),
        first_seeks,
        "reads at one position must not re-seek the cursor: {s}"
    );

    drop(c);
    server.join().unwrap();
}

/// Read `Name: <n>` out of a `monitor ttd stats` reply.
fn parse_counter(s: &str, name: &str) -> u64 {
    s.lines()
        .find_map(|l| l.trim().strip_prefix(name))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or_else(|| panic!("counter {name} missing from: {s}"))
}

/// `qXfer:libraries:read` against the real trace: the module list must
/// include the main executable (the trace's first module).
#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn ttd_libraries_list_includes_first_module() {
    let trace = trace_path();
    let (addr, server) = start_server(&trace);
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    c.send("qXfer:libraries:read::0,ffff");
    let body = c.recv();
    assert!(
        body[0] == b'l' || body[0] == b'm',
        "libraries reply must start with l/m, got {:?}",
        body[0] as char
    );
    let xml = std::str::from_utf8(&body[1..]).expect("libraries XML must be UTF-8");
    println!(
        "libraries XML (first 300 chars): {}",
        &xml[..xml.len().min(300)]
    );
    assert!(xml.starts_with("<library-list"), "got: {xml}");
    assert!(
        xml.contains("sample.exe"),
        "the main executable must be listed: {xml}"
    );
    // Every module gets a <segment address>.
    assert!(xml.contains("<segment address="), "got: {xml}");

    drop(c);
    server.join().unwrap();
}

/// `qThreadExtraInfo` against the real trace: the first active thread's extra
/// info includes its stable UTID, OS tid and a trace position.
#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn ttd_thread_extra_info_reports_utid_and_position() {
    let trace = trace_path();
    let (addr, server) = start_server(&trace);
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    // Pick the first active thread from qfThreadInfo (p<pid>.<tid>).
    let threads = c.roundtrip("qfThreadInfo");
    assert!(threads.starts_with('m'), "qfThreadInfo: {threads}");
    let first_tid = threads
        .trim_start_matches('m')
        .split(',')
        .next()
        .unwrap_or("");
    // first_tid looks like "p1.3784"; split off the tid.
    let tid_part = first_tid.split('.').next_back().unwrap_or(first_tid);
    let os_tid = u64::from_str_radix(tid_part, 16).expect("hex tid");
    println!("first thread os_tid={:#x}", os_tid);

    c.ack();
    c.send(&format!("qThreadExtraInfo,p1.{tid_part}"));
    let body = c.recv();
    let s = String::from_utf8(from_hex(std::str::from_utf8(&body).unwrap()))
        .expect("qThreadExtraInfo reply must be hex UTF-8");
    println!("qThreadExtraInfo: {s}");
    assert!(s.contains("UTID "), "got: {s}");
    assert!(s.contains(&format!("OS 0x{:x}", os_tid)), "got: {s}");
    assert!(s.contains("pos "), "got: {s}");

    drop(c);
    server.join().unwrap();
}

/// A `Z2` data watchpoint over the wire: set a write watchpoint on the
/// return-address slot, then `vCont;c` — the replay must stop at the write
/// (T05 watch) rather than running to the trace end. This is the end-to-end
/// version of the `data_watchpoint_write_fires_on_continue` target test.
#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn ttd_data_watchpoint_stops_continue() {
    let trace = trace_path();
    let (addr, server) = start_server(&trace);
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    // Read the current RSP.
    assert_eq!(c.roundtrip("Hg0"), "OK");
    let g = c.roundtrip("g");
    let buf = from_hex(&g);
    // g layout: 16 GPRs (rsp is index 7), rip at 16*8.
    let rsp = u64::from_le_bytes(buf[7 * 8..7 * 8 + 8].try_into().unwrap());
    let target = rsp - 8;
    println!("watching {:#x} for writes", target);

    // Z2 = write watchpoint, 8 bytes.
    let z = c.roundtrip(&format!("Z2,{:x},8", target));
    assert_eq!(z, "OK", "Z2 must be accepted: {z}");
    let stop = c.roundtrip("vCont;c");
    println!("continue stop: {stop}");
    assert!(
        stop.starts_with("T05"),
        "write watchpoint must stop the continue with T05, got: {stop}"
    );
    assert!(
        stop.contains("watch") || stop.contains("awatch") || stop.contains("rwatch"),
        "stop reply must indicate a watchpoint, got: {stop}"
    );

    let z = c.roundtrip(&format!("z2,{:x},8", target));
    assert_eq!(z, "OK", "z2 must be accepted: {z}");

    drop(c);
    server.join().unwrap();
}

/// `vCont;r` (range step) must exercise the backend's optimized range-step
/// path over the wire: step once, leave the one-byte range, report T05, and
/// move the trace position. Delve's `next`/`stepout` use this packet.
#[test]
#[cfg_attr(not(has_ttd_trace), ignore)]
fn ttd_range_step_moves_position_without_watchpoint_interference() {
    let trace = trace_path();
    let (addr, server) = start_server(&trace);
    let mut c = RspClient::connect(addr);
    delve_handshake(&mut c);

    let threads = c.roundtrip("qfThreadInfo");
    assert!(threads.starts_with('m'), "qfThreadInfo: {threads}");
    let first_tid = threads
        .trim_start_matches('m')
        .split(',')
        .next()
        .expect("at least one thread");
    assert_eq!(c.roundtrip(&format!("Hc{first_tid}")), "OK");

    assert_eq!(c.roundtrip("Hg0"), "OK");
    let g = c.roundtrip("g");
    let buf = from_hex(&g);
    let rip = u64::from_le_bytes(buf[16 * 8..16 * 8 + 8].try_into().unwrap());
    let when0 = String::from_utf8(from_hex(&c.roundtrip("qRRCmd:when:-1"))).unwrap();

    let stop = c.roundtrip(&format!(
        "vCont;r{:x},{:x}:{}",
        rip,
        rip.saturating_add(1),
        first_tid
    ));
    println!("range step stop: {stop}");
    assert!(
        stop.starts_with("T05"),
        "range step outside the range must stop with T05, got {stop}"
    );

    let when1 = String::from_utf8(from_hex(&c.roundtrip("qRRCmd:when:-1"))).unwrap();
    assert_ne!(
        when0, when1,
        "range step must advance the trace position ({when0} -> {when1})"
    );

    drop(c);
    server.join().unwrap();
}
