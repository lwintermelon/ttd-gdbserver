# Delve integration: ttd-gdbserver as a reversible debugging backend

[中文文档](delve-integration.zh-CN.md)

The Delve-side changes are not maintained in this project; they live directly
in the Delve repository at
[github.com/lwintermelon/delve](https://github.com/lwintermelon/delve) (the
`ttd-dbg` branch).

Goal: give Windows TTD traces the same reversible-debugging experience that
`dlv replay` (rr) provides on Linux.

Delve is just one GDB RSP client — GDB and LLDB also support reverse debugging
and can attach to ttd-gdbserver. This document covers the verified Delve path
only; the rationale for the protocol choice is in the root README.

## How it works

```
dlv ttd foo.run ──► gdbserial.TtdReplay()
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
$env:DELVE_TTD_SERVER = "<this project>\target\debug\ttd-gdbserver.exe"

# 3. Record (any Windows process, using TTD's own tool; needs admin)
ttd.exe myapp.exe

# 4. Replay-debug a Go program's trace with Delve
dlv.exe ttd <trace.run> --exe <recorded executable>
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
