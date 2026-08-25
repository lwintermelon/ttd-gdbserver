# ttd-gdbserver

[中文文档](README.zh-CN.md)

**rr for Windows.** Replay WinDbg TTD traces over GDB Remote Serial Protocol —
time-travel debugging for GDB, LLDB, and Delve. One Rust binary: attach your
debugger and replay traces without opening WinDbg.

- **Reverse debugging on Windows** — step and continue backward through any
  `.run` trace, restart at any position.
- **Any GDB RSP client** — GDB, LLDB, Delve. Go debugging through Delve
  verified end-to-end.
- **Read-only replay (x86_64)** — no memory/register writes; inspect the
  exact recorded execution without re-running.

## Quick start

```bash
# GDB RSP frontend (GDB / LLDB / Delve)
ttd-gdbserver <trace.run> [--listen 127.0.0.1:1234]
```

The server accepts one session at a time; engine state persists across sessions.
Pressing Ctrl+C stops the server gracefully: it finishes the current
session, releases the trace files, and exits.

To replay-debug with Delve (needs a TTD-enabled `dlv` built from the fork,
see [Delve integration](#delve-integration)):

```bash
dlv ttd <trace.run>
```

## Why GDB Remote Serial Protocol

GDB RSP is a general protocol between debuggers and debug backends, with wide
support: GDB, LLDB and Delve all ship GDB RSP clients and all support reverse
debugging. Choosing it makes ttd-gdbserver a general mechanism — like rr on
Linux — that any GDB RSP-speaking debugger can attach to for time-travel
debugging, rather than being tied to one frontend.

The protocol is old and far from modern, but it is the de-facto standard: rr
serves GDB through GDB RSP, and Delve's rr backend (`pkg/proc/gdbserial`) is
a ready-made GDB RSP client. Reusing this mature path means Delve needs only
minimal changes to get rr-equivalent reversible debugging.

The project has been verified end-to-end debugging Go programs through Delve
(see below), so it is basically usable.

## Delve integration

Delve is the first client wired up (Go debugging verified end-to-end). The
Delve-side changes live on the `ttd-dbg` branch of
[github.com/lwintermelon/delve](https://github.com/lwintermelon/delve); see
[`doc/delve-integration.md`](doc/delve-integration.md).

Quick start (after building a TTD-enabled dlv from that branch):

```bash
dlv ttd <trace.run>
```

Supported reversible commands (Delve terminal): `step` / `next` / `stepout` /
`step-instruction` and their `rev` reverse variants, `restart [pos]`.

## Try it yourself

The `examples/` directory has a small Go program (`examples/go-sample/`) and
an `init.txt` command script that exercises breakpoints, stepping, reverse
execution, and restart. To reproduce the full flow:

```powershell
# 1. Build the sample Go program (needs Go; -N -l disables optimizations so
#    locals are inspectable)
cd examples\go-sample
go build -gcflags="all=-N -l" -o sample.exe .

# 2. Record it with TTD (needs admin; ttd.exe is TTD's recorder)
ttd.exe sample.exe
#    → produces sample.run (and sample.idx on first replay)

# 3. Replay-debug it with Delve, driving the session from init.txt
$env:DELVE_TTD_SERVER = "C:\path\to\ttd-gdbserver.exe"
$env:TERM = "dumb"
dlv.exe ttd sample.run --init ..\init.txt
```

The `init.txt` script runs to completion and exits on its own. You can also
drop the `--init` flag and drive the session interactively — all reversible
commands (`rev step`, `reverse-continue`, `restart`, ...) are available.

## Architecture

```text
GDB / LLDB / Delve ──GDB RSP──► src/gdb/ (GdbTarget)
                                          │ DebugTarget trait
                                     src/target/ (TtdProcess)
                                          │
                                     src/ttd/ (FFI) ──► TTD Replay DLL
```

The GDB frontend is built on the `DebugTarget` trait (`src/target.rs`):
instruction-level operations only — forward/reverse execution, memory reads,
breakpoints/watchpoints, thread/register access, position navigation. No
symbol-layer concepts.

## GDB RSP surface

The stub is a multi-process (single-process-pid) all-stop GDB RSP server.
It implements the standard packets gdb, LLDB and Delve expect for Windows
replay, plus a few replay-specific conventions:

- **Registers / memory / threads**: `g`/`G`, `p`/`P` (P is `E`), `m`/`M`
  (M is `E`), `H`, `qfThreadInfo`/`qsThreadInfo`, `qThreadExtraInfo`.
- **Execution**: `vCont;c`/`vCont;s`/`vCont;r` (range step), `s`/`c`,
  `bc`/`bs` (reverse), `vRun` (restart at a position), `D` (detach). Range
  step runs on the backend's watchpoint-free step cursor, so source-level
  `next`/`stepout` do not pay a query-cursor seek per instruction.
- **Breakpoints**: `Z0`/`Z1` (simulated as TTD watchpoints), `Z2`/`Z3`/`Z4`
  data watchpoints.
- **Introspection**: `qXfer:features:read` (target description XML),
  `qXfer:exec-file:read`, `qXfer:auxv:read` (synthetic `AT_ENTRY`),
  `qXfer:libraries:read` (PE module list), `qXfer:memory-map:read`
  (module-backed `ram` regions), `qOffsets`, `qGetTLSAddr` (TEB),
  `qSupported`, `qAttached`, `qRcmd` (`monitor ttd …`).
- **Signal / mode**: `QPassSignals`/`QProgramSignals`/`vCtrlC` ACK; no
  `QNonStop` (all-stop only), no fork/vfork events, no launch-config
  packets — replay has no "spawn". `QListThreadsInStopReply` is also
  declined: supporting it needs a fourth gdbstub fork patch and only saves
  one round-trip per stop.

### Replay vs live semantics

Replay is read-only: memory/register writes (`M`, `P`, `X`) return `E`.
Signals are not delivered — the recorded signal stream is the only one that
matters. Thread continue (`vCont;c:tid`) behaves like whole-trace continue:
the other threads advance only as the cursor moves, because TTD replay is
single-threaded on the host. This matches what rr advertises. A thread id that
never appears in the trace has no state and is answered with `E`.

### TTD custom commands (RR-style)

GDB users get time-travel introspection via `monitor`:

| Command | Output |
|---|---|
| `monitor ttd info` | trace lifetime, current position, module/thread/exception counts |
| `monitor ttd threads` | every thread that ever existed (UTID, OS tid, active range) |
| `monitor ttd module <addr>` | resolve an address to a module + range |
| `monitor ttd events [n]` | the first n recorded exception events (code, address, pc, position) |
| `monitor ttd stats` | replay counters: queries, cursor seeks, steps, replays, physical watchpoint adds/removes |


## Building

```powershell
$env:TTD_SDK_DIR = "<Microsoft.TimeTravelDebugging.Apis NuGet package dir>"
cargo build
```

Requires the Windows TTD SDK and MSVC. The TTD SDK is the NuGet package
`Microsoft.TimeTravelDebugging.Apis` (C++ headers + `TTDReplay.lib`); using
the TTD Replay API is documented in the
[WinDbg-Samples TTD directory](https://github.com/microsoft/WinDbg-Samples/tree/master/TTD).
Point the `TTD_SDK_DIR` environment variable at the package directory —
`build.rs` derives `sdk\include` / `sdk\lib\x64` from it. The runtime DLLs
(`TTDReplay.dll` etc.) come with the TTD installation; add their directory
to `PATH`.

The GDB RSP engine is [gdbstub](https://github.com/lwintermelon/gdbstub) (the
`ttd-dbg` branch, pulled in as a git dependency; it carries three small
patches, each also submitted upstream as a separate PR); the x86-64 register
file reuses `gdbstub_arch` from the same fork (`X86_64CoreRegs`, GPRs +
x87/SSE).

## Testing

### Environment

The TTD runtime DLLs (`TTDReplay.dll` etc.) must be on `PATH`; the integration
tests also need a `.run` trace (`TTD_TRACE_PATH` pointing at it).

### Unit & integration tests

| Test | Covers | Needs TTD? |
|---|---|---|
| `cargo test --lib` | unit tests: breakpoint mgmt, stop-reason mapping, TargetRunner concurrency, packet parsing, qXfer/XML, checkpoints, `monitor ttd` rendering | No |
| `cargo test --test gdb` | GDB RSP handshake/reverse, mock backend, GDB RSP surface, multi-session persistence | No |
| `cargo test --test target` | TtdProcess target layer | Yes (reported `ignored` without trace) |
| `cargo test --test gdb_ttd` | GDB RSP + real trace | Yes (reported `ignored` without trace) |
| `cargo test --test ttd` | TTD FFI layer | Yes (reported `ignored` without trace) |

```powershell
# Without a TTD trace (the trace-dependent tests report as ignored)
cargo test --lib --test gdb

# With a TTD trace (full suite)
$env:TTD_TRACE_PATH = "<path.run>"
cargo test
```

### End-to-end (dlv ttd)

Replay-debug a Go program trace with Delve (needs a TTD-enabled dlv built
from the Delve fork first, see `doc/delve-integration.md`):

```powershell
$env:DELVE_TTD_SERVER = "C:\path\to\ttd-gdbserver.exe"
$env:TERM = "dumb"
dlv.exe ttd <trace.run> --init <script>
```

`TERM=dumb` is required — otherwise long output triggers Delve's pager, which
swallows script input.
