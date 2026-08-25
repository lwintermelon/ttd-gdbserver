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
only; the rationale for the protocol choice is in the root README.

## How it works

```
dlv ttd-gdbserver foo.run ──► gdbserial.TtdGdbserverReplay()
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

A ready-made Go sample program and an `init.txt` command script live in
`examples/` — see the "Try it yourself" section of the root README for the
full build/record/replay flow.

Once inside the Delve terminal, all reversible commands are available:

| Command | Description |
|---|---|
| `continue` / `step` / `next` / `stepout` | Forward execution |
| `reverse-continue` / `rev step` / `rev next` / `rev stepout` | Reverse execution (via `bc`/`bs`) |
| `restart` / `restart <pos>` | Jump to trace start / a position |
| (automatic) | Current position shown after every stop (`<SEQ>:<STEPS>` hex; `state.When` ← `qRRCmd when`, no standalone `when` command) |

Note: the backend is read-only — memory/register writes and call injection are
not supported.

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
