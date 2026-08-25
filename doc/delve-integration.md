# Delve integration: ttd-gdbserver as a reversible debugging backend

[中文文档](delve-integration.zh-CN.md)

The Delve-side changes are not maintained in this project; they live directly
in the Delve repository at
[github.com/lwintermelon/delve](https://github.com/lwintermelon/delve) (the
`ttd-dbg` branch).

Goal: give Windows TTD traces the same reversible-debugging experience that
`dlv replay` (rr) provides on Linux.

One deliberate difference from rr: rr can also *record* a live process
(`dlv exec --backend=rr` runs the program under `rr record` and replays it
afterwards). ttd-gdbserver is replay-only — recording is the job of the
separate TTD recorder (`ttd.exe`) — so Delve rejects launch-style requests on
this backend and points at the recorder instead.

Delve is just one GDB RSP client — GDB and LLDB also support reverse debugging
and can attach to ttd-gdbserver. This document covers the verified Delve path
only.

## How it works

```
dlv ttd foo.run ──► gdbserial.TtdGdbserverReplay()
                        │  spawns: ttd-gdbserver foo.run --listen 127.0.0.1:0
                        │  parses "Listening on 127.0.0.1:<port>"
                        ▼
                    gdbserial gdbProcess (Delve's existing GDB RSP client,
                    the same code used for the rr backend)
                        │  GDB Remote Serial Protocol (TCP)
                        ▼
                    ttd-gdbserver (this project's src/gdb/)
                        │  DebugTarget trait
                        ▼
                    TtdProcess ──► WinDbg TTD Replay API
```

GDB RSP was chosen because Delve's rr backend (`pkg/proc/gdbserial`) is
already a GDB RSP client with all reversible-debugging semantics built in
(`bc`/`bs` reverse execution, restart). ttd-gdbserver speaks the same
protocol, so the Delve-side changes are minimal.

## Building & using

```powershell
# 1. Clone and build a TTD-enabled dlv (ttd-dbg branch)
git clone -b ttd-dbg https://github.com/lwintermelon/delve
cd delve
go build -o dlv.exe .\cmd\dlv

# 2. Let dlv find ttd-gdbserver: put it on PATH, or
$env:DELVE_TTD_SERVER = "C:\path\to\ttd-gdbserver.exe"
#    Extra ttd-gdbserver flags can be injected via DELVE_TTD_SERVER_FLAGS
#    (the analogue of DELVE_RR_REPLAY_FLAGS), e.g. to see the server's
#    protocol-level debug log on stderr:
# $env:DELVE_TTD_SERVER_FLAGS = "-v"

# 3. Record (any Windows process, using TTD's own tool; needs admin)
ttd.exe myapp.exe

# 4. Replay-debug a Go program's trace with Delve (the executable is
#    reported by ttd-gdbserver via qXfer:exec-file, like `dlv replay`):
dlv.exe ttd <trace.run>

#    Optional: pass an explicit executable when the recorded one is no
#    longer at its original path, or when debugging on a machine other
#    than the one that recorded the trace:
dlv.exe ttd <trace.run> C:\path\to\matching\myapp.exe
```

The recorded corpus under `tests/e2e/programs/` holds ready-made Go and C
programs with their own Delve/GDB command scripts; each directory is a
complete debugging scenario, and the scripts can also be driven interactively
(see [`tests/e2e/README.md`](../tests/e2e/README.md)).

Once inside the Delve terminal, all reversible commands are available:

| Command | Description |
|---|---|
| `continue` / `step` / `next` / `stepout` | Forward execution |
| `reverse-continue` / `rev step` / `rev next` / `rev stepout` | Reverse execution (via `bc`/`bs`) |
| `restart` / `restart <pos>` | Jump to trace start / a position |
| (automatic) | Current position shown after every stop (`<SEQ>:<STEPS>` hex; `state.When` ← `qRRCmd when`, no standalone `when` command) |

Note: the backend is read-only — memory/register writes and call injection are
not supported.

### What a goroutine view means here

Delve resolves goroutines from runtime metadata: `gs_base` is the TEB, the
TLS slot and `m.curg` lead to a `g`, and `runtime.allgs` provides the list.
Those structures are read memory, and the rules from
[`ttd-internals.md` §7.1](ttd-internals.md) apply:

- **Per-thread registers and stacks are exact.** The goroutine Delve reports
  for the *stopped* thread is the recording's answer for that thread's step.
- **A parked goroutine's own state is usually exact** — it stopped changing
  when it parked, so an old data line still holds its current values.
- **A goroutine that was running on another thread when the cursor stopped is
  where staleness can show up.** Its `g` fields are read from the cursor's
  position while another thread was still writing them. In practice this
  affects lock-free runtime structures — run queues, the netpoller, timer
  heaps — more than mutex- or channel-guarded ones, since those are updated at
  sequence events the timeline orders faithfully.
- A stale field is a *value from the recording*, not garbage: it is a shape
  Delve can parse, which is why the symptom is usually "a goroutine's state
  looks one step behind" rather than a crash.

Rough guidance: use the trace for the thread that actually stopped, for
anything reached by locks/channels, and for reverse execution. If a goroutine
listing looks inconsistent, cross-check `monitor ttd mem <addr>` (each read's
provenance) before trusting it, and prefer a write watchpoint when a specific
field's history matters.

### VSCode (reverse-debugging GUI)

The VSCode Go extension works as-is: for recorded targets it automatically
enables the reverse-continue / reverse-step controls (Delve reports
`supportsStepBack` in the capabilities event).

**Scenario 1: connect to an already-running replay server.** Start a headless
server:

```powershell
dlv.exe ttd <trace.run> --headless -l 127.0.0.1:2345
```

launch.json:

```json
{
  "name": "go ttd attach",
  "type": "go",
  "request": "attach",
  "mode": "remote",
  "host": "127.0.0.1",
  "port": 2345
}
```

**Scenario 2: start the replay from VSCode.** launch.json (`env` reaches the
dlv process; set `DELVE_TTD_SERVER` there when ttd-gdbserver is not on PATH):

```json
{
  "name": "Replay TTD trace",
  "type": "go",
  "request": "launch",
  "mode": "ttd-gdbserver",
  "ttdTracePath": "${workspaceFolder}\\sample.run",
  "env": {
    "DELVE_TTD_SERVER": "C:\\path\\to\\ttd-gdbserver.exe"
  }
}
```

settings.json points the extension at the TTD-enabled dlv once:

```json
{
  "go.alternateTools": {
    "dlv": "C:\\path\\to\\dlv.exe"
  }
}
```

The schema lint on `mode`/`ttdTracePath` in launch.json can be ignored.

## Server-side compatibility surface

Delve's `pkg/proc/gdbserial` client parses this server's replies literally.
Every server-side decision that exists because of it, in one table (the
packets themselves are generic — `qRRCmd` is rr's vocabulary, `qGetTLSAddr`
and `QPassSignals` are standard RSP; the module that answers them,
`src/gdb/target/unknown.rs`, is client-neutral):

| Behavior | Where |
|---|---|
| `qRRCmd when` / checkpoints / `vRun;c<N>` restart | `src/gdb/commands.rs` (answered in `unknown.rs`) |
| A `vRun`/`R` restart is an error reply on failure, not a silent success | `src/gdb/commands.rs` + `src/gdb/target/extended.rs` |
| The resume a client sends right after a restart stops at the restart position (rr behaviour); replaying past it would lose the checkpoint | `src/gdb/target.rs` (`restart_pending`, `spawn_replay`) |
| `qGetTLSAddr` → TEB (GS base; no writes into tracee memory) | `src/gdb/target/unknown.rs` |
| `QListThreadsInStopReply` / `QThreadSuffixSupported` declined (explicit empty reply) | `src/gdb/target/unknown.rs` |
| `QPassSignals` / `QProgramSignals` accepted + ignored (read-only replay) | `src/gdb/target/unknown.rs` |
| `vCtrlC` aborts the in-flight replay | `src/gdb/target/unknown.rs` |
| `use_rle() = false` — Delve's binary decoder has no RLE | `src/gdb/target.rs` |
| `use_no_ack_mode() = true` | `src/gdb/target.rs` |
| 59-register `g` layout + `gs_base` = TEB to locate the goroutine `g` | `src/gdb/regs.rs` |
| `E16` for rejected breakpoint registration (`Ok(false)` → `NonFatalError(22)`) | `src/gdb/target/breakpoints.rs` + gdbstub |
| `T02` on ^C — dual sources: protocol interrupt + backend `Interrupted` | `src/gdb/session.rs` (`on_interrupt`) and `src/gdb/mapping.rs` |
| `k` → EOF with no reply (gdbstub fork patch), `D` → `OK` | `src/gdb/session.rs` |
| rr-style `T09` (trace end) / `T00` (trace start) stop vocabulary | `src/gdb/mapping.rs` |
| Process exit is also `T09` first (rewindable, Delve's `almostExited`); `W00` only when the client resumes *forward* again from the exit stop — rr's `in_debuggee_end_state` → `handle_exited_state`, minus rr's FATAL on further resumes (ours answers `W00` idempotently) | `src/gdb/target.rs` (`end_state`, `spawn_replay`) |
| Every exception class surfaces as a stop on continue/step (Delve's continue loop resumes past them automatically), except the first-chance codes a live debugger passes through (`0xE06D7363`, `0x20474343`), which a continue replays past | `src/target/process/execution.rs` |
| A query during a replay never blocks or kills the session; fatal-only trait methods answer neutrally | `src/gdb/target/base.rs`, `thread_info.rs`, `description.rs`, `extended.rs` |
| Checkpoints (and position/breakpoints) survive reconnects | `src/gdb/session.rs` |
