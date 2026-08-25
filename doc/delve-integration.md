# Delve integration

The `ttd-dbg` branch of
[github.com/lwintermelon/delve](https://github.com/lwintermelon/delve) provides
`dlv ttd`, using the same `pkg/proc/gdbserial` client as Delve's rr backend.
Delve starts `ttd-gdbserver <trace> --listen 127.0.0.1:0`, parses the selected
port, and connects over GDB RSP.

TTD recording is separate: use `ttd.exe` to record a process, then replay the
resulting `.run` file. This backend does not launch or record a live process.

## Build and use

```powershell
git clone -b ttd-dbg https://github.com/lwintermelon/delve
cd delve
go build -o dlv.exe .\cmd\dlv

# Put ttd-gdbserver.exe on PATH, or configure its absolute path.
$env:DELVE_TTD_SERVER = "C:\path\to\ttd-gdbserver.exe"
# Optional server flags, such as protocol logging:
$env:DELVE_TTD_SERVER_FLAGS = "-v"

dlv.exe ttd <trace.run>
# Supply the matching executable if its recorded path is unavailable:
dlv.exe ttd <trace.run> C:\path\to\myapp.exe
```

Delve learns the executable through `qXfer:exec-file`; an explicit path is
useful when replaying on another machine. The backend is read-only, so memory
writes, register writes, and call injection are unavailable.

| Delve command | Replay operation |
|---|---|
| `continue`, `step`, `next`, `stepout` | Forward execution |
| `reverse-continue`, `rev step`, `rev next`, `rev stepout` | Reverse execution |
| `restart`, `restart <pos>` | Trace start or a saved position |

The current `<SEQ>:<STEPS>` position comes from `qRRCmd when` and appears in
Delve's stop state. The scripts in [`regression/cases/`](../regression/cases/)
are working Delve/GDB examples.

## Goroutines and memory

Delve finds goroutines through `gs_base` (the TEB), TLS, `m.curg`, and
`runtime.allgs`. These memory reads follow the trace's recovery rules:

- The stopped thread's registers, current goroutine, and recently touched stack
  are exact or usually exact.
- A parked goroutine is often accurate because its state stopped changing.
- A goroutine running on another thread may look one step behind. Its fields
  are still values from the recording, not arbitrary data.

Cross-check suspicious reads with `monitor ttd mem <addr>`. Use a write
watchpoint when the exact change to a field matters. See
[`ttd-internals.md`](ttd-internals.md) for why a trace has no common
cross-thread “now”.

## VS Code

For an existing headless Delve server:

```powershell
dlv.exe ttd <trace.run> --headless -l 127.0.0.1:2345
```

```json
{
  "name": "Go TTD attach",
  "type": "go",
  "request": "attach",
  "mode": "remote",
  "host": "127.0.0.1",
  "port": 2345
}
```

To let VS Code start Delve, use this launch configuration instead:

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

Point `go.alternateTools.dlv` at the TTD-enabled `dlv.exe`. Schema warnings for
`mode` and `ttdTracePath` can be ignored. The Go extension enables reverse
controls when Delve reports `supportsStepBack`.

## Server compatibility contract

These behaviors are required by Delve's literal RSP parsing, though the packet
handlers remain client-neutral:

| Behavior | Implementation |
|---|---|
| rr positions, checkpoints, and `vRun;c<N>` restart | `src/gdb/commands.rs` |
| Failed restart returns an error; first resume after restart stops in place | `src/gdb/target.rs`, `target/extended.rs` |
| `qGetTLSAddr` returns the TEB without tracee writes | `src/gdb/target/unknown.rs` |
| `QListThreadsInStopReply` and `QThreadSuffixSupported` are declined | `src/gdb/target/unknown.rs` |
| Signal-selection packets are accepted and ignored; `vCtrlC` interrupts replay | `src/gdb/target/unknown.rs` |
| RLE disabled; no-ack mode enabled | `src/gdb/target.rs` |
| 59-register layout includes `gs_base` | `src/gdb/regs.rs` |
| Rejected breakpoint registration returns `E16` | `src/gdb/target/breakpoints.rs` |
| ^C returns `T02`; trace start/end return `T00`/`T09` | `src/gdb/mapping.rs` |
| `k` closes without a reply; `D` returns `OK` | `src/gdb/session.rs` |
| Process exit first returns rewindable `T09`, then `W00` on another forward resume | `src/gdb/target.rs` |
| Continue filters first-chance C++ exception codes; queries never block behind replay | `src/target/process/execution.rs`, `src/gdb/runner.rs` |
