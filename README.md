# ttd-gdbserver

[中文文档](README.zh-CN.md)

**rr for Windows.** Replay WinDbg TTD traces over GDB Remote Serial Protocol —
time-travel debugging for GDB, LLDB and Delve. One Rust binary: point your
debugger at it and replay a recording without opening WinDbg.

- **Reverse debugging on Windows** — step and continue backward through any
  `.run` trace, restart at any position.
- **Any GDB RSP client** — GDB, LLDB, Delve; Go debugging through Delve is
  verified end-to-end.
- **Read-only replay (x86_64)** — no memory or register writes.

## Quick start

```bash
# one session, then exit
ttd-gdbserver <trace.run> [--listen 127.0.0.1:1234]
# keep serving reconnects until Ctrl+C (position, breakpoints, checkpoints persist)
ttd-gdbserver <trace.run> --accept-multiclient
```

The server accepts one session at a time. Ctrl+C stops accepting, lets the
current session finish, releases the trace and exits 0; press it again to force.

With a TTD-enabled `dlv` (see [Delve integration](#delve-integration)):

```bash
dlv ttd <trace.run>
```

## Delve integration

Delve is the first client wired up and the only one verified end-to-end. The
Delve-side changes live on the `ttd-dbg` branch of
[github.com/lwintermelon/delve](https://github.com/lwintermelon/delve); the
client contract is documented in
[`doc/delve-integration.md`](doc/delve-integration.md).

Reversible commands: `step`, `next`, `stepout`, `step-instruction` and their
`rev` variants, plus `restart [pos]`.

## How it works

```text
GDB / LLDB / Delve ──GDB RSP──►  protocol frontend
                                      │
                                 debug target
                                      │
                                 TTD Replay DLL ──► .run trace
```

The frontend speaks RSP only; the target exposes instruction-level operations
only — execution control, memory reads, breakpoints (simulated as TTD
watchpoints), thread and register access, position navigation. There is no
symbol layer: the server replays a recording, it does not interpret your
program. Design decisions live in
[`doc/architecture.md`](doc/architecture.md); what the TTD replay API provides
and how it maps onto this codebase is in
[`doc/ttd-internals.md`](doc/ttd-internals.md).

### There is no single "now"

A live debugger stops every thread, so all threads share one instant and
"read this address now" has one answer. A recorded trace has no such instant:
it has a timeline of steps, and the cursor sits at **one step of one thread**.
Everything a client asks is answered relative to that step.

Two consequences, worth knowing before reading a stack:

- **Registers are per thread and exact.** `thread 2` then `bt` uses thread 2's
  last step, not "the same moment as thread 1". That is the honest reading of
  the recording: the two threads were not stopped at a common point.
- **Memory is recovered, not replayed.** A read returns the newest data line
  the trace index holds for that address, which is exact for anything the
  stopped thread recently touched (its stack, its locals) and can be an older
  value for an address only other threads wrote, long before. The value is
  still a real value from the recording — just not necessarily the one that
  existed at the cursor's position.

So the useful rule is: **trust registers, then the current frame, then
recently-touched memory; for anything else, prefer a watchpoint.** A write
watchpoint stops at the instruction that performed the write, where the new
value is exact, and stepping back restores the old one. `monitor ttd mem`
prints the trace position each run of returned bytes came from, so a
suspicious read can be triaged instead of guessed at.

### Watchpoints are a strength, not a compromise

GDB's `Z2`–`Z4` (write / read / access) are hardware debug registers on a live
process: four of them, per thread, and only for aligned data of limited width.
Here they are TTD memory watchpoint events, which gives:

- **no practical count limit** — the engine replays the stream and reports
  accesses that match, instead of trapping on a comparator;
- **exact hits, in both directions** — `reverse-continue` reaches the previous
  write to the same address, which a hardware watchpoint cannot do;
- **read watchpoints that are not second-class** — on hardware they share the
  same four DR slots as write watchpoints and only cover 1/2/4/8-byte aligned
  ranges; here a read hit comes from the engine knowing each load it executed,
  so it costs the same as a write watchpoint and covers an arbitrary range.

Verified in `tests/target_watchpoint_hits.rs`: a write stop already shows the
new value, and `step-back` restores the old one.

## GDB RSP surface

Multi-process (single pid), all-stop.

- **Registers / memory / threads** — `g`/`G`, `p`/`P` (`P` → `E`), `m`/`M`
  (`M` → `E`), `H`, `qfThreadInfo`/`qsThreadInfo`, `qThreadExtraInfo`.
- **Execution** — `vCont;c`, `vCont;s`, `vCont;r` (range step), `s`/`c`,
  `bc`/`bs` (reverse), `vRun` (restart at a position), `D`.
- **Breakpoints** — `Z0`/`Z1` (simulated), `Z2`–`Z4` data watchpoints.
- **Introspection** — `qXfer:features:read`, `qXfer:exec-file:read`,
  `qXfer:auxv:read`, `qXfer:libraries:read`, `qOffsets`, `qGetTLSAddr`,
  `qSupported`, `qAttached`, `qRcmd`.
- **Absent by design** — `qXfer:memory-map` (GDB treats it as the complete
  readable set and would refuse stack/TEB reads), `QNonStop`, fork/vfork
  events, launch configuration (replay has no "spawn"),
  `QListThreadsInStopReply`.

Replay is read-only (`M`, `P`, `X` → `E`); signals are not delivered — the
recorded stream is the only one that matters. A thread id absent from the trace
has no state and is answered with `E`.

### `monitor ttd …`

| Command | Output |
|---|---|
| `monitor ttd info` | trace lifetime, current position, index status, module/thread/exception counts |
| `monitor ttd threads` | every thread that ever existed (UTID, OS tid, active range) |
| `monitor ttd module <addr>` | address → module + range |
| `monitor ttd mem <addr> [len]` | bytes plus the trace position each run of them was recovered from (default 16 bytes, max 256) |
| `monitor ttd events [n]` | recorded exception events (code, address, pc, position; default 20) |
| `monitor ttd stats` | queries, cursor seeks, steps, replays, physical watchpoint adds/removes |

## Building

Windows x86_64, MSVC and the TTD replay SDK; `cargo build` produces the single
binary.

The GDB RSP engine is [gdbstub](https://github.com/lwintermelon/gdbstub), a git
dependency on branch `ttd-dbg` — not on crates.io, because the branch carries
the protocol extensions this server needs.

The TTD boundary is an [`cxx`](https://cxx.rs) bridge: `src/ttd/cpp/ttd_cxx.hpp` /
`ttd_cxx.cpp` own the SDK handles and expose a typed C++ surface;
`src/ttd/ffi.rs` is the bridge; `src/ttd/` is the safe wrapper the rest of the
crate uses. The bridge carries ownership, callbacks, memory slices and plain
value structs, so the Rust side has no bindgen-generated raw FFI layer. The
upstream [`ttd`](https://github.com/hugsy/ttd-rust) bindings were the starting
point but do not expose the memory-watchpoint callback, interrupt, memory
policy, range queries or per-thread context a debugger frontend needs; the
constraints that shaped this surface are in
[`doc/ttd-internals.md` §13](doc/ttd-internals.md).

## Testing

```powershell
cargo test            # protocol and unit tests; trace-backed tests report as ignored
mise run test:rust    # full suite against the corpus recordings from mise.toml [env]
```

Trace-backed tests read `TTD_TRACE_PATH`, and the exception tests
`TTD_FAULT_TRACE_PATH`.

### End-to-end gates

`tests/e2e/` drives the server through real clients — Delve for Go, GDB for
C/C++ — and diffs the **transcripts** rather than asserting on specific values:

| Gate | Compares | A diff means |
|---|---|---|
| `forward` | replay vs one recorded live run (addresses, tids, positions normalized) | the replay diverges from the real execution |
| `reverse` | replay vs replay of the same trace, nothing masked | reverse stepping is not deterministic |

Scripts only navigate and print (`p/x $pc`, `bt`, `info locals`): the whole
transcript is the oracle, so a regression shows up as a diff line instead of
hiding in a hand-written check. The seven programs under
`tests/e2e/programs/` each combine a feature set (see the corpus table in
[`tests/e2e/README.md`](tests/e2e/README.md)), and their `.dlv`/`.gdb` files are
ordinary command scripts you can also drive interactively.

```powershell
mise run test               # both gates, whole corpus
python tests/e2e/e2e.py forward --only go-core   # one case, lower level
```
