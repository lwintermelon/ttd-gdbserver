# ttd-gdbserver

**rr for Windows.** Replay WinDbg Time Travel Debugging (TTD) traces over GDB
Remote Serial Protocol (RSP), with forward and reverse execution for GDB, LLDB,
and Delve. One Rust binary; no WinDbg UI required.

- Reverse step/continue and restart anywhere in a `.run` recording.
- Breakpoints and read/write/access watchpoints, implemented with TTD events.
- Read-only x86_64 replay: no memory/register writes or call injection.
- Symbols are resolved by the client, not the server.

## Quick start

Build the server, then point it at a recording:

```bash
# Serve one client, then exit.
ttd-gdbserver <trace.run> --listen 127.0.0.1:1234

# Keep accepting one client at a time; replay state survives reconnects.
ttd-gdbserver <trace.run> --accept-multiclient
```

Connect any GDB RSP client to the listening address. With the TTD-enabled
Delve branch, the server is started automatically:

```bash
dlv ttd <trace.run>
```

Delve supports `step`, `next`, `stepout`, `step-instruction`, their `rev`
variants, and `restart [pos]`. See [Delve integration](doc/delve-integration.md)
for installation and VS Code setup.

Ctrl+C stops the listener and lets the current session finish; press it again
to force exit.

## Build

Requirements: Windows x86_64, MSVC, Rust, and the
`Microsoft.TimeTravelDebugging.Apis` TTD SDK. Set `TTD_SDK_DIR` to the package
and put the TTD runtime DLLs on `PATH`, then run:

```powershell
cargo build
```

The RSP frontend uses the `ttd-dbg` branch of
[gdbstub](https://github.com/lwintermelon/gdbstub). A typed `cxx` bridge isolates
the TTD SDK in `src/ttd/`; the rest of the crate is safe Rust.

## Test

```powershell
cargo test                 # unit/protocol tests; trace tests are ignored without traces
mise run test:rust         # Rust tests using the local corpus
mise run test              # real-client forward/reverse baseline diffs
```

The corpus is documented in [`regression/`](regression/README.md). Baselines
are local to one machine and recording. Run `mise run baseline` on trusted code
before changes; tests never create or update baselines implicitly.

## Replay semantics

A trace has no single cross-thread “now”: registers are exact for the selected
thread and position, while memory is recovered from indexed data lines and may
be older or assembled from several positions. Use a write watchpoint when the
exact instruction that changed a value matters. See
[TTD internals](doc/ttd-internals.md) before interpreting suspicious memory.

## Documentation

- [GDB RSP and monitor commands](doc/gdb-rsp.md)
- [Delve integration](doc/delve-integration.md)
- [Architecture and concurrency](doc/architecture.md)
- [TTD replay model and bindings](doc/ttd-internals.md)
- [Transcript regression tests](regression/README.md)
