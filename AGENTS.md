# AGENTS.md — architecture and conventions

## Scope

Rust time-travel debugger backed by the WinDbg TTD Replay API, with a GDB RSP
frontend. Windows x86_64, read-only replay, no server-side symbols.

Read the dynamics and invariants in `doc/architecture.md`, the replay model in
`doc/ttd-internals.md`, and the client contract in `doc/delve-integration.md`.
This file is the module map and contributor checklist; do not duplicate those
documents here.

## Layers

```text
GDB client ←RSP→ src/gdb/ ←DebugTarget→ src/target/ → src/ttd/ → TTD DLL
```

| Layer | Main types | Location |
|---|---|---|
| Protocol API | `GdbTarget<T: DebugTarget>` | `src/gdb/target.rs` |
| Target API | `DebugTarget` | `src/target.rs` |
| Target implementation | `TtdProcess` | `src/target/process.rs` |
| SDK wrappers | `ReplayEngine`, `ReplayCursor` | `src/ttd/` |
| C++ SDK boundary | `Engine`, `Cursor` | `src/ttd/cpp/ttd_cxx.*` |

`DebugTarget` uses TTD positions/registers directly and exposes only
instruction-level execution, reads, breakpoints, threads, and navigation. There
are no writes, expression evaluation, or source/line concepts in the target.

## Module map

- `src/ttd/`: `ffi.rs` is the cxx bridge; `engine.rs`/`cursor.rs` own SDK
  handles; `types.rs` defines values and masks; `callback.rs` contains panic
  escalation; `interrupt.rs` owns the cross-thread replay interrupt slot.
- `src/target/process/`: `target_impl.rs` is the single `DebugTarget` impl;
  `execution.rs` implements replay, stop classification, stepping, and range
  interruption. `process.rs` owns construction and cursor positioning.
- `src/target/`: one primary type per file. Important pieces are `cursors.rs`
  (two persistent cursor roles), `breakpoint_state.rs` (logical/physical
  reconciliation), `threads.rs`, `trace_metadata.rs`, `watchpoint.rs`,
  `breakpoint.rs`, and `stop_reason.rs`.
- `src/gdb/`: `runner.rs` owns worker scheduling and nonblocking queries;
  `session.rs` owns TCP sessions; `resume.rs` prioritizes resume actions;
  `mapping.rs`, `packet.rs`, `commands.rs`, `xml.rs`, and `pe.rs` contain pure,
  unit-tested wire logic. `checkpoint.rs` stores trace-global checkpoints.
- `src/gdb/target/`: gdbstub handlers split by protocol area. `unknown.rs`
  handles packets gdbstub has no trait for; `test_support.rs` is test-only.
- `tests/common/`: shared RSP, harness, and mock-target support. Every
  integration test crate starts with `mod common;`.

## Load-bearing invariants

- **Two cursors:** the persistent cursor carries watchpoints and serves
  queries; the watchpoint-free step cursor advances exactly one instruction.
  Both remember positions lazily, invalidate after partial failures, and keep
  the engine alive. Range stepping never seeks the query cursor.
- **Breakpoint simulation:** logical breakpoints are separate from physical
  watchpoints. Sync is incremental and fallible; rejected registration rolls
  back and returns `E16`. Removal is address-keyed end to end. Hit matching
  includes access, range, and thread.
- **Scheduling:** replay runs on a worker. Backend queries use `try_lock` and
  never park the event loop. Fatal-only handlers reply neutrally while busy.
- **Interruption:** blocking replay publishes an `InterruptSlot`; range steps
  poll an epoch instead. `begin_resume` resets the epoch so an old ^C cannot
  cancel the next operation. Session teardown interrupts and joins the worker.
- **FFI:** callbacks never unwind into C++. Callback context is heap-owned.
  Unsafe code is confined to `src/ttd/`; `src/lib.rs` denies it elsewhere.
- **Queries:** unknown tids return no state rather than TTD's zeroed context.
  Immutable thread/module/exception metadata is captured at open. Memory uses
  the current cursor position and the `GloballyAggressive` policy.
- **Indexing:** open ensures a complete index. A leftover `.idxt` invalidates
  the sibling `.idx` and forces a rebuild. Index progress callbacks must not
  be null.
- **Protocol:** preserve the stop vocabulary and Delve behaviors documented in
  `doc/gdb-rsp.md` and `doc/delve-integration.md`. In particular, process exit
  is rewindable `T09` before a later forward resume returns `W00`.
- **Persistence:** with `--accept-multiclient`, position, breakpoints, and
  checkpoints survive reconnects. The accept loop owns the checkpoint table.

Do not advertise `qXfer:memory-map`: GDB would treat it as the complete
readable set and reject stack/TEB reads. Do not implement LLDB-only
`qProcessInfo` without a verified client requirement.

## Tests

Unit tests live beside source. `tests/gdb_*.rs` uses TCP plus the mock backend;
`tests/target_*.rs`, `tests/gdb_ttd.rs`, and exception tests need recordings.
Trace tests are honestly ignored when `TTD_TRACE_PATH` or
`TTD_FAULT_TRACE_PATH` is absent.

```powershell
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
mise run test:rust       # includes corpus traces from [env]
mise run test            # real-client transcript diffs
```

[`regression/`](regression/README.md) is the source-level differential
suite: one comparison runner and flat `cases/<case>/` directories. Forward and
reverse scripts print state; the runner compares it directly with local,
ignored `.base` files captured from trusted code using `mise run baseline`.
Only known startup noise is excluded. Build and record are native mise tasks;
record contains only `ttd.exe` commands and requires administrator privileges.
Never refresh a baseline just to make a change pass.

## CLI

```bash
ttd-gdbserver <trace.run> --listen 127.0.0.1:1234
ttd-gdbserver <trace.run> --accept-multiclient
ttd-gdbserver <trace.run> -v     # -vv adds gdbstub logs
```

One session is served at a time. By default the server exits after it;
`--accept-multiclient` serves until Ctrl+C. The first Ctrl+C lets the current
session finish, and the second forces exit.

## Dependencies and style

- `ctrlc`, `thiserror`, `log`/`env_logger`, `clap`, `quick-xml`.
- `cxx`/`cxx-build`; no bindgen. Set `TTD_SDK_DIR` and put TTD runtime DLLs on
  `PATH`.
- `gdbstub`/`gdbstub_arch` come from the `ttd-dbg` branch of
  `github.com/lwintermelon/gdbstub`, which carries required protocol hooks.

Match the surrounding module structure and naming. Keep one primary type per
file and derive errors with `thiserror`. The crate is pedantic-clean, including
integration tests; use inline format args, checked tid conversions, and
`map_or` idioms. Public fallible/panicking items need the corresponding docs,
and pure accessors use `#[must_use]`. Comments explain current design, not
change history. If documented structure or behavior changes, update this file
and the relevant doc in the same change.
