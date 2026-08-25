//! `monitor ttd …` — rr-style custom commands for trace introspection.
//!
//! gdb sends `monitor ttd <subcmd> [<args>]`; the reply is plain text, one
//! line per record, exactly like rr's `when`/`info` commands.
//!
//!   - `info`            : trace lifetime, current position, and the
//!     module/thread/exception counts
//!   - `threads`         : every thread that ever existed
//!   - `module <addr>`   : resolve an address to a module + range
//!   - `mem <addr> [n]`  : read memory and print the trace position each run
//!     of bytes was recovered from (TTD may assemble one read from several)
//!   - `events [n]`      : the first n recorded exception events (default 20)
//!   - `stats`           : replay/query/cursor-seek and physical-watchpoint
//!     counters (seek ratio is the position-cache health metric)
//!
//! Rendering is separated from the protocol layer (it only needs a
//! [`TraceView`]) so it can be unit-tested without a stub or a socket.

use std::fmt;

use crate::target::{DebugTarget, ModuleInfo, TargetDiagnostics, TargetStats};
use crate::ttd::types::{TtdExceptionEvent, TtdMemRange};

/// Shown for anything we don't understand, including a bare `monitor`.
pub const USAGE: &str = "supported: ttd info | ttd threads | ttd module <addr> | ttd mem <addr> [len] | ttd events [n] | ttd stats";

/// Byte count `monitor ttd mem` reads when the caller gives no length.
const DEFAULT_MEM_LEN: usize = 16;
/// Upper bound on `monitor ttd mem`'s length argument; a dump is for a handful
/// of fields, not for streaming a buffer through the RSP console.
const MEM_LEN_CAP: usize = 256;
/// Ranges requested per `monitor ttd mem` read. A multi-position read of a
/// dozen fields does not produce more, and a higher cap only lengthens the
/// reply.
const MEM_RANGES_CAP: u32 = 16;

/// What the monitor commands read out of the target.
///
/// Blanket-implemented for every [`DebugTarget`]. The indirection exists so
/// [`render`] can be tested against a plain struct instead of a live backend.
pub trait TraceView {
    fn diagnostics(&self) -> TargetDiagnostics;
    fn modules(&self) -> Vec<ModuleInfo>;
    fn exceptions(&self) -> Vec<TtdExceptionEvent>;
    fn stats(&self) -> TargetStats;
    /// Read `len` bytes and report where each run of them came from.
    fn read_memory_ranges(&self, addr: u64, len: usize) -> (Vec<u8>, Vec<TtdMemRange>);
}

impl<T: DebugTarget> TraceView for T {
    fn diagnostics(&self) -> TargetDiagnostics {
        DebugTarget::diagnostics(self)
    }
    fn modules(&self) -> Vec<ModuleInfo> {
        DebugTarget::modules(self)
    }
    fn exceptions(&self) -> Vec<TtdExceptionEvent> {
        DebugTarget::exceptions(self)
    }
    fn stats(&self) -> TargetStats {
        DebugTarget::stats(self)
    }
    fn read_memory_ranges(&self, addr: u64, len: usize) -> (Vec<u8>, Vec<TtdMemRange>) {
        let mut buf = vec![0u8; len];
        let (n, ranges) = DebugTarget::read_memory_ranges(self, addr, &mut buf, MEM_RANGES_CAP);
        buf.truncate(n);
        (buf, ranges)
    }
}

/// Render the reply for `monitor <cmd>` into `out`.
pub fn render(cmd: &str, view: &dyn TraceView, out: &mut dyn fmt::Write) {
    let mut words = cmd.split_whitespace();
    let head = words.next().unwrap_or("");
    if head != "ttd" {
        let _ = writeln!(out, "ttd-gdbserver: unknown monitor subcommand '{head}'");
        let _ = writeln!(out, "{USAGE}");
        return;
    }
    let sub = words.next().unwrap_or("");
    // Re-join the remainder with single spaces so the subcommands can pull a
    // single argument (`module 0x401000`, `events 5`).
    let args: Vec<&str> = words.collect();
    match sub {
        "info" => info(&view.diagnostics(), out),
        "threads" => threads(&view.diagnostics(), out),
        "module" => module(args.first().copied(), &view.modules(), out),
        "mem" => mem(view, &args, out),
        "events" => events(args.first().copied(), &view.exceptions(), out),
        "stats" => stats(&view.stats(), out),
        other => {
            let _ = writeln!(out, "ttd: unknown subcommand '{other}'");
            let _ = writeln!(out, "{USAGE}");
        }
    }
}

/// `monitor ttd info`.
fn info(d: &TargetDiagnostics, out: &mut dyn fmt::Write) {
    let _ = writeln!(out, "trace: loaded (TTD replay engine)");
    let _ = writeln!(out, "version: {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(out, "lifetime: {} .. {}", d.first_position, d.last_position);
    let _ = writeln!(out, "current: {}", d.current_position);
    let _ = writeln!(out, "index: {}", index_status_label(d.index_status));
    let _ = writeln!(out, "modules: {}", d.module_count);
    let _ = writeln!(out, "threads (lifetime): {}", d.threads.len());
    let _ = writeln!(out, "exception events: {}", d.exception_count);
}

/// The index line of `monitor ttd info`.
///
/// Memory queries are only accurate against a complete index, so an
/// incomplete one is called out instead of being left implicit.
const fn index_status_label(status: crate::ttd::types::TtdIndexStatus) -> &'static str {
    use crate::ttd::types::TtdIndexStatus;
    match status {
        TtdIndexStatus::Loaded => "loaded",
        TtdIndexStatus::NotPresent => "missing (memory reads are unreliable)",
        TtdIndexStatus::Unloadable => "unusable (memory reads are unreliable)",
        TtdIndexStatus::Unknown => "unknown (memory reads may be unreliable)",
    }
}

/// `monitor ttd threads` — every thread that ever existed, in TTD's order.
fn threads(d: &TargetDiagnostics, out: &mut dyn fmt::Write) {
    let _ = writeln!(
        out,
        "{:>6}  {:>10}  {:>14}  {:>14}",
        "UTID", "OS_TID", "active_min", "active_max"
    );
    for t in &d.threads {
        let _ = writeln!(
            out,
            "{:>6}  0x{:08x}  {:>9x}:{:>04x}  {:>9x}:{:>04x}",
            t.unique_id,
            t.os_thread_id,
            t.active_time.0.sequence(),
            t.active_time.0.steps(),
            t.active_time.1.sequence(),
            t.active_time.1.steps(),
        );
    }
}

/// Parse a hex address with an optional `0x` prefix.
fn parse_hex(text: &str) -> Option<u64> {
    // Strip one optional 0x/0X prefix (repeated prefixes like "0x0x10" are
    // left in place so the parse fails loudly instead of silently dropping).
    let hex = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .unwrap_or(text);
    u64::from_str_radix(hex, 16).ok()
}

/// `monitor ttd module <hex-addr>`.
fn module(arg: Option<&str>, modules: &[ModuleInfo], out: &mut dyn fmt::Write) {
    let Some(rest) = arg else {
        let _ = writeln!(out, "usage: monitor ttd module <hex-addr>");
        return;
    };
    let Some(addr) = parse_hex(rest) else {
        let _ = writeln!(out, "ttd: bad address '{rest}'");
        return;
    };
    // Avoid `base_addr + size`, which can wrap for a malformed module range:
    // the comparison is the same and cannot overflow.
    match modules
        .iter()
        .find(|m| addr >= m.base_addr && addr - m.base_addr < m.size)
    {
        Some(m) => {
            let end = m.base_addr.saturating_add(m.size);
            let _ = writeln!(
                out,
                "{:#x}..{:#x} (+{:#x}) {}",
                m.base_addr, end, m.size, m.name
            );
        }
        None => {
            let _ = writeln!(out, "no module contains {addr:#x}");
        }
    }
}

/// `monitor ttd mem <hex-addr> [len]` — bytes plus the position each run of
/// them was recovered from.
///
/// TTD may assemble one read from several trace positions, so a buffer is not
/// guaranteed to be a single-instant snapshot. This prints the evidence
/// instead of hiding it: one line per contiguous run, with the sequence id of
/// the data line that served it.
///
/// An old sequence id is not by itself a stale value — a location nothing has
/// written since is correctly served from that old line. What the line does
/// say is *how far back* the newest recoverable data sits.
fn mem(view: &dyn TraceView, args: &[&str], out: &mut dyn fmt::Write) {
    let Some(rest) = args.first() else {
        let _ = writeln!(out, "usage: monitor ttd mem <hex-addr> [len]");
        return;
    };
    let Some(addr) = parse_hex(rest) else {
        let _ = writeln!(out, "ttd: bad address '{rest}'");
        return;
    };
    let len = match args.get(1) {
        None => DEFAULT_MEM_LEN,
        Some(s) => match s.parse::<usize>() {
            Ok(v) if v > 0 && v <= MEM_LEN_CAP => v,
            _ => {
                let _ = writeln!(out, "ttd: bad length '{s}' (1..={MEM_LEN_CAP})");
                return;
            }
        },
    };

    let current = view.diagnostics().current_position;
    let (bytes, ranges) = view.read_memory_ranges(addr, len);
    let _ = writeln!(
        out,
        "{addr:#x}: {} of {len} byte(s) at {current}",
        bytes.len()
    );
    if ranges.is_empty() {
        let _ = writeln!(
            out,
            "  provenance unavailable (backend cannot attribute bytes to a position)"
        );
    } else {
        for r in &ranges {
            // `sequence` is fragment-granular by definition, so there is no
            // step offset to print for the data line itself.
            let _ = writeln!(
                out,
                "  {:#x} +{:#x} seq {:#x}",
                r.Address,
                r.size(),
                r.Sequence
            );
        }
        if ranges.len() == MEM_RANGES_CAP as usize {
            let _ = writeln!(out, "  (range list capped at {MEM_RANGES_CAP})");
        }
    }
    if !bytes.is_empty() {
        let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let _ = writeln!(out, "  bytes: {}", hex.join(" "));
    }
}

/// `monitor ttd stats`.
fn stats(s: &TargetStats, out: &mut dyn fmt::Write) {
    let _ = writeln!(out, "queries: {}", s.queries);
    let _ = writeln!(out, "cursor seeks: {}", s.cursor_seeks);
    let _ = writeln!(out, "steps: {}", s.steps);
    let _ = writeln!(out, "replays: {}", s.replays);
    let _ = writeln!(out, "watchpoint adds: {}", s.watchpoint_adds);
    let _ = writeln!(out, "watchpoint removes: {}", s.watchpoint_removes);
    if s.queries > 0 {
        // A diagnostic ratio, printed to three decimals: a counter that large
        // would have lost precision long before the f64 conversion matters,
        // and the alternative (integer rounding) is strictly less informative.
        #[allow(clippy::cast_precision_loss)]
        let per_query = s.cursor_seeks as f64 / s.queries as f64;
        let _ = writeln!(out, "seeks per query: {per_query:.3}");
    }
}

/// `monitor ttd events [n]`.
fn events(arg: Option<&str>, all: &[TtdExceptionEvent], out: &mut dyn fmt::Write) {
    let n: usize = match arg {
        None => 20,
        Some(s) => {
            let Ok(v) = s.parse() else {
                let _ = writeln!(out, "ttd: bad event count '{s}'");
                return;
            };
            v
        }
    };
    let _ = writeln!(out, "exception events: {} (showing up to {n})", all.len());
    for ev in all.iter().take(n) {
        let _ = writeln!(
            out,
            "{:#010x} at {:#x} (pc {:#x}) pos {}",
            ev.code, ev.record_address, ev.program_counter, ev.position
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::ThreadDiagnostics;
    use crate::ttd::types::TtdPosition;

    /// A [`TraceView`] with fixed contents — no engine, no socket.
    struct Stub {
        diagnostics: TargetDiagnostics,
        modules: Vec<ModuleInfo>,
        exceptions: Vec<TtdExceptionEvent>,
        stats: TargetStats,
        /// Bytes `mem` will see, and the ranges they are attributed to.
        memory: (Vec<u8>, Vec<TtdMemRange>),
    }

    impl TraceView for Stub {
        fn diagnostics(&self) -> TargetDiagnostics {
            self.diagnostics.clone()
        }
        fn modules(&self) -> Vec<ModuleInfo> {
            self.modules.clone()
        }
        fn exceptions(&self) -> Vec<TtdExceptionEvent> {
            self.exceptions.clone()
        }
        fn stats(&self) -> TargetStats {
            self.stats
        }
        fn read_memory_ranges(&self, _addr: u64, _len: usize) -> (Vec<u8>, Vec<TtdMemRange>) {
            self.memory.clone()
        }
    }

    fn pos(sequence: u64, steps: u64) -> TtdPosition {
        TtdPosition::new(sequence, steps)
    }

    fn stub() -> Stub {
        Stub {
            diagnostics: TargetDiagnostics {
                first_position: pos(1, 0),
                last_position: pos(0x2F3, 0x1A),
                current_position: pos(0x10, 5),
                threads: vec![ThreadDiagnostics {
                    unique_id: 2,
                    os_thread_id: 0x3784,
                    active_time: (pos(1, 0), pos(0x2F3, 0x1A)),
                }],
                exception_count: 1,
                module_count: 2,
                index_status: crate::ttd::types::TtdIndexStatus::Loaded,
            },
            modules: vec![
                ModuleInfo {
                    base_addr: 0x1_4000_0000,
                    size: 0x1000,
                    name: "sample.exe".to_string(),
                },
                ModuleInfo {
                    base_addr: 0x7ff_0000,
                    size: 0x2000,
                    name: "ntdll.dll".to_string(),
                },
            ],
            exceptions: vec![TtdExceptionEvent {
                code: 0xC000_0005,
                record_address: 0xdead,
                program_counter: 0x1_4000_1000,
                position: pos(0x12, 3),
            }],
            stats: TargetStats {
                queries: 120,
                cursor_seeks: 4,
                steps: 30,
                replays: 2,
                watchpoint_adds: 3,
                watchpoint_removes: 2,
            },
            // Two runs from different trace positions: the shape TTD produces
            // when one read is assembled from several data lines.
            memory: (
                vec![0x40, 0xab, 0x68, 0x56, 0x87, 0x17, 0x00, 0x00],
                vec![
                    crate::ttd::memory_range(0x1787_5670_0090, 4, 0x7be),
                    crate::ttd::memory_range(0x1787_5670_0094, 4, 0x7bf),
                ],
            ),
        }
    }

    fn run(cmd: &str) -> String {
        let mut out = String::new();
        render(cmd, &stub(), &mut out);
        out
    }

    #[test]
    fn info_reports_lifetime_and_counts() {
        let s = run("ttd info");
        assert!(s.contains("trace: loaded"), "{s}");
        assert!(s.contains("lifetime: 1:0 .. 2f3:1a"), "{s}");
        assert!(s.contains("current: 10:5"), "{s}");
        assert!(s.contains("index: loaded"), "{s}");
        assert!(s.contains("modules: 2"), "{s}");
        assert!(s.contains("threads (lifetime): 1"), "{s}");
        assert!(s.contains("exception events: 1"), "{s}");
    }

    #[test]
    fn threads_table_has_header_and_one_row_per_thread() {
        let s = run("ttd threads");
        let lines: Vec<&str> = s.lines().collect();
        assert!(
            lines[0].contains("UTID") && lines[0].contains("OS_TID"),
            "{s}"
        );
        assert_eq!(lines.len(), 2, "{s}");
        assert!(lines[1].contains("0x00003784"), "{s}");
    }

    #[test]
    fn module_resolves_address_and_reports_misses() {
        let s = run("ttd module 0x140000800");
        assert!(s.contains("0x140000000..0x140001000"), "{s}");
        assert!(s.contains("sample.exe"), "{s}");

        // Just past the end of the module is a miss, not a hit.
        assert!(run("ttd module 0x140001000").contains("no module contains"));
        assert!(run("ttd module 0xdeadbeef").contains("no module contains"));
    }

    #[test]
    fn module_rejects_bad_arguments() {
        assert!(run("ttd module").contains("usage: monitor ttd module"));
        assert!(run("ttd module xyz").contains("bad address"));
        // Addresses are always hex, with or without the 0x prefix.
        assert!(run("ttd module 4096").contains("no module contains 0x4096"));
        assert!(run("ttd module 7ff0000").contains("ntdll.dll"));
        // Upper-case 0X prefix is accepted too.
        assert!(run("ttd module 0X140000800").contains("sample.exe"));
        // A doubled prefix is not silently stripped into a different address.
        assert!(run("ttd module 0x0x140000800").contains("bad address"));
    }

    /// A module whose range ends at the top of the address space must not
    /// overflow the display or the containment check.
    #[test]
    fn module_range_near_u64_max_does_not_overflow() {
        let modules = vec![ModuleInfo {
            base_addr: u64::MAX - 0x10,
            size: 0x20,
            name: "top.dll".to_string(),
        }];
        let mut out = String::new();
        module(Some("fffffffffffffff8"), &modules, &mut out);
        assert!(out.contains("top.dll"), "{out}");
        assert!(out.contains("0xffffffffffffffff"), "{out}");
    }

    #[test]
    fn events_defaults_to_twenty_and_honours_a_limit() {
        let s = run("ttd events");
        assert!(s.contains("exception events: 1 (showing up to 20)"), "{s}");
        assert!(s.contains("0xc0000005 at 0xdead"), "{s}");

        let s = run("ttd events 0");
        assert!(s.lines().count() == 1, "limit 0 must print no rows: {s}");
    }

    /// A count that is not a plain number must be rejected loudly: falling
    /// back to the default of 20 would hide a typo behind plausible output
    /// (the user sees 20 events and assumes that is the answer).
    #[test]
    fn events_rejects_unparsable_counts() {
        for bad in ["0x5", "-1", "abc", "99999999999999999999999"] {
            let s = run(&format!("ttd events {bad}"));
            assert!(s.contains("bad event count"), "must reject {bad}: {s}");
            assert!(
                !s.contains("exception events:"),
                "{bad} must print no rows: {s}"
            );
        }
        // Parsable counts (including the default) still work.
        assert!(run("ttd events").contains("exception events: 1"));
        assert!(run("ttd events 1").contains("exception events: 1"));
    }

    #[test]
    fn stats_reports_counters_and_the_seek_ratio() {
        let s = run("ttd stats");
        assert!(s.contains("queries: 120"), "{s}");
        assert!(s.contains("cursor seeks: 4"), "{s}");
        assert!(s.contains("steps: 30"), "{s}");
        assert!(s.contains("replays: 2"), "{s}");
        assert!(s.contains("watchpoint adds: 3"), "{s}");
        assert!(s.contains("watchpoint removes: 2"), "{s}");
        assert!(s.contains("seeks per query: 0.033"), "{s}");
    }

    /// A target that keeps no counters must not print a 0/0 ratio.
    #[test]
    fn stats_without_queries_omits_the_ratio() {
        let mut out = String::new();
        super::stats(&TargetStats::default(), &mut out);
        assert!(out.contains("queries: 0"), "{out}");
        assert!(!out.contains("seeks per query"), "{out}");
    }

    /// `mem` must show how a buffer was assembled: one line per run, each
    /// with the position it was recovered from. Two runs at different
    /// sequences mean the bytes are not a single-instant snapshot.
    #[test]
    fn mem_reports_provenance_per_run() {
        let s = run("ttd mem 0x178756700090 8");
        assert!(s.contains("8 of 8 byte(s) at 10:5"), "{s}");
        assert!(s.contains("0x178756700090 +0x4 seq 0x7be"), "{s}");
        assert!(s.contains("0x178756700094 +0x4 seq 0x7bf"), "{s}");
        assert!(s.contains("bytes: 40 ab 68 56 87 17 00 00"), "{s}");
    }

    #[test]
    fn mem_without_provenance_says_so() {
        let mut stub = stub();
        stub.memory = (vec![0xaa, 0xbb], Vec::new());
        let mut out = String::new();
        render("ttd mem 0x1000", &stub, &mut out);
        assert!(out.contains("provenance unavailable"), "{out}");
        assert!(out.contains("bytes: aa bb"), "{out}");
    }

    #[test]
    fn mem_handles_short_reads_and_bad_arguments() {
        let mut stub = stub();
        stub.memory = (vec![0xaa], Vec::new());
        let mut out = String::new();
        render("ttd mem 0x1000 16", &stub, &mut out);
        assert!(out.contains("1 of 16 byte(s)"), "{out}");

        assert!(run("ttd mem").contains("usage: monitor ttd mem"));
        assert!(run("ttd mem zz").contains("bad address 'zz'"));
        assert!(run("ttd mem 0x10 0").contains("bad length '0'"));
        assert!(run("ttd mem 0x10 9999").contains("bad length '9999'"));
    }

    #[test]
    fn unknown_commands_print_usage() {
        assert!(run("ttd bogus").contains("unknown subcommand 'bogus'"));
        assert!(run("ttd bogus").contains(USAGE));
        assert!(run("wat").contains("unknown monitor subcommand 'wat'"));
        assert!(run("").contains("unknown monitor subcommand ''"));
        assert!(run("ttd").contains(USAGE));
    }

    /// The blanket impl is what lets the protocol layer pass a `DebugTarget`
    /// straight in. This is a compile-time check: if the blanket impl or either
    /// bound drifts, the test target stops compiling.
    #[test]
    fn debug_target_satisfies_trace_view() {
        fn requires_trace_view<T: DebugTarget + TraceView>() {}
        requires_trace_view::<crate::target::TtdProcess>();
    }
}
