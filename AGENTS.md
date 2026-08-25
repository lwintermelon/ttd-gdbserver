# AGENTS.md — Project Architecture & Conventions

## Project Overview

TTD Time Travel Debugger — a Rust-based time-travel debugger backed by the WinDbg TTD Replay API.
Single binary with a GDB RSP frontend. x86_64 only, no symbol support.

## Architecture

```
GDB client ←GDB RSP→ GDB frontend (gdbstub) ←DebugTarget trait→ TtdProcess → TTD DLL
```

The API between the protocol frontend and the TTD debug target is the
`DebugTarget` trait (`src/target/mod.rs`): instruction-level operations
only — execution control (forward/reverse), memory reads, breakpoints,
thread/register access, trace position navigation. No source/line concepts,
no expression evaluation, no symbol browsing. The target is read-only
(no memory/register writes).

| Layer | Rust type | File |
|-------|-----------|------|
| Target API | `DebugTarget` trait | `src/target/mod.rs` |
| Target impl | `TtdProcess` | `src/target/process.rs` |
| GDB target | `GdbTarget<T: DebugTarget>` | `src/gdb/target.rs` |

## Module Layout

| Module | Files | Responsibility |
|--------|-------|---------------|
| `ttd/` | `engine.rs`, `cursor.rs`, `bindings.rs`, `ffi.rs`, `types.rs`, `error.rs` | FFI bindings to TTD C++ API. Two-path bindgen: `bindings.rs` = POD types from `ttd_types.h`, `ffi.rs` = function declarations from `ttd_wrapper.h`. `types.rs` re-exports with clean names + `Ord` impl. |
| `target/` | `mod.rs` (trait + `ModuleInfo`), `process.rs`, `error.rs`, `breakpoint.rs`, `stop_reason.rs` | Debug target layer: `DebugTarget` trait + `TtdProcess`. Each file has one concern. |
| `gdb/` | `mod.rs`, `regs.rs`, `target.rs`, `session.rs`, `xml.rs` | GDB RSP frontend (built on gdbstub, Delve-gdbserial-compatible) |

### gdb/ file split

| File | Content |
|------|---------|
| `regs.rs` | Custom `Arch`: `TtdRegisters` wraps `gdbstub_arch`'s `X86_64CoreRegs` (GPRs + x87/SSE) + `fs_base`/`gs_base`; `TtdRegId` wraps `X86_64CoreRegId`; target description embedded via `include_str!` (XML files live next to it in `src/gdb/`) |
| `target.rs` | `GdbTarget<T: DebugTarget>`: full gdbstub `Target` impl (regs/memory/threads/Z-z breakpoints/forward+reverse resume/extended-mode/exec-file/auxv/process-info/**UnknownPacket** for `qRRCmd`/`qGetTLSAddr`/signal-ACK/**MonitorCmd** for `monitor ttd …`, plus `Libraries`/`MemoryMap`/`ThreadExtraInfo`/`SectionOffsets`); replays run on worker threads |
| `session.rs` | TCP server + gdbstub `run_blocking` event loop (polls connection for ^C, worker for stop reasons); engine state persists across sessions |
| `xml.rs` | Minimal XML escaping helpers for the `qXfer:*:read` blobs (library list, memory map) |

### target/ file split

| File | Content |
|------|---------|
| `mod.rs` | `DebugTarget` trait (the API consumed by protocol frontends) + `ModuleInfo` |
| `process.rs` | `TtdProcess` struct + `impl DebugTarget`, `ContinueContext`, watchpoint callback |
| `error.rs` | `DebugError` enum |
| `breakpoint.rs` | `Breakpoint`, `BreakpointKind`, `BreakpointManager` |
| `stop_reason.rs` | `StopReason` enum + `from_replay_result()` |

## Key Patterns

### Breakpoint Simulation
TTD has no native breakpoints. `BreakpointManager` tracks logical breakpoints and applies them as TTD memory watchpoints during continue/step operations. A watchpoint callback matches hits against the breakpoint table.

Note: clients (Delve, GDB) re-insert all breakpoints after `vRun` (restart),
so the stub may temporarily hold several ids for one address. The `z`
handlers therefore remove **all** ids at an address/key, not just one —
popping a single id would leave stale watchpoints firing after the client
deleted the breakpoint (observed as an infinite continue loop).

### Target types
The target API uses the TTD projection types directly (`TtdPosition`,
`TtdX64Regs` from `ttd/types.rs`) plus its own `StopReason`, `DebugError`,
`BreakpointKind`, `ModuleInfo`. There is no separate protocol-type layer.

### Thread state
Threads are queried on demand at the current trace position:
`active_thread_ids()` enumerates live threads; `thread_state(tid)` returns
`(registers, teb)` for one thread. The TEB address is exposed as the
`gs_base` register so Delve can locate the goroutine `g`.

### Lifetimes
`TtdCursor<'engine>` borrows `TtdEngine` — cursors cannot outlive their engine. This is enforced via `PhantomData`.

## Test Structure

| File | Tests | Type | TTD needed? |
|------|-------|------|-------------|
| `src/target/*.rs` `#[cfg(test)]` | 16 | Unit: breakpoint manager + stop-reason mapping | No |
| `tests/gdb.rs` | 9 | Integration: GDB RSP over TCP (mock backend, replays Delve handshake) | No |
| `tests/target.rs` | 14 | Integration: TtdProcess via DebugTarget trait | Yes (auto-skip) |
| `tests/gdb_ttd.rs` | 4 | Integration: GDB RSP frontend with real trace | Yes (auto-skip) |
| `tests/ttd.rs` | 12 | Integration: TtdEngine + TtdCursor FFI level | Yes (`TTD_TRACE_PATH`) |
| **Total** | **77** | | 46 no TTD, 31 need TTD |

```powershell
# All non-TTD tests
cargo test --lib --test gdb

# TTD integration (needs a .run trace; TTDReplay DLLs on PATH)
$env:TTD_TRACE_PATH = "path.run"
cargo test --test target --test gdb_ttd --test ttd -- --nocapture
```

## CLI

```bash
# GDB RSP frontend (Delve / plain GDB) over TCP
ttd-gdbserver <trace.run> --listen 127.0.0.1:1234 [--exe <executable>]

# Server accepts one session at a time; engine state persists across sessions.
```

## Dependencies

- `thiserror` for error types
- `log`/`env_logger` for logging
- `clap` for CLI
- `cc` (build-dep) for compiling the C++ shim
- `bindgen` (build-dep) for auto-generating Rust FFI bindings from C header
- TTD SDK: the `Microsoft.TimeTravelDebugging.Apis` NuGet package, located via
  the `TTD_SDK_DIR` env var (see README.md); runtime DLLs (`TTDReplay.dll`)
  come from the TTD install dir on `PATH`.
- `gdbstub` (git dep on the `ttd-dbg` branch of
  `github.com/lwintermelon/gdbstub`) — the GDB RSP engine; the branch carries
  three small patches (UnknownPacket hook, 2-digit stop signals, kill without
  a reply), each also submitted upstream as a separate PR. No patch files are
  maintained in this repo.
- `gdbstub_arch` (git dep, same branch) — the x86-64 register file
  (`X86_64CoreRegs`: GPRs + x87/SSE in GDB's standard layout), extended with
  `fs_base`/`gs_base` in `src/gdb/regs.rs`.

## Bindgen & FFI Architecture

### Design principle
Rust code interacts with TTD through two auto-generated sources:
1. **SDK enum bindings** (`ttd_bindings.h`) — directly includes `<TTD/IReplayEngine.h>`. bindgen parses it in C++ mode and extracts `enum class` values (`EventType`, `DataAccessMask`, `EventMask`, `ExceptionMask`). Fully automated — no hand-written enum values.
2. **Wrapper projections** (`ttd_wrapper.h`) — types that need manual C-to-C++ conversion (flattened structs, simplified registers, opaque handles, callbacks, C enums for ABI compatibility) + all `extern "C"` function declarations.

The C++ shim (`ttd_wrapper.cpp`) includes TTD SDK headers directly and converts between SDK types and our projection types. C enums in `ttd_wrapper.h` are verified against the SDK at C++ compile time via `static_assert`.

Top-level Rust API is idiomatic — no low-level details leak.

### Two-pass bindgen (current state)
```
Pass 1 (SDK enums, C++ mode):
  ttd_bindings.h (#include <TTD/IReplayEngine.h>)
    → bindgen -x c++ -fms-compatibility -std=c++20
    → ttd_sdk_bindings.rs → src/ttd/bindings.rs
  (auto-generated: TTD::Replay::EventType, EventMask, DataAccessMask, ExceptionMask)

Pass 2 (wrapper API, C mode):
  ttd_wrapper.h
    → bindgen (C mode)
    → ttd_wrapper_bindings.rs → src/ttd/ffi.rs
  (projection types + functions + callbacks + opaque handles)
```

**Key detail:** In C++ mode, `allowlist_type`/`allowlist_var` patterns must match the
C++ qualified name (e.g. `TTD::Replay::EventType`), not the flattened Rust name
(`TTD_Replay_EventType`).  The flattened names appear in the generated output but
bindgen matches against the original C++ names during allowlist filtering.

### What bindgen generates

**Pass 1 — `bindings.rs`:**
- **Enum types**: `TTD_Replay_EventType` (u8), `TTD_Replay_EventMask` (u32), `TTD_Replay_DataAccessMask` (u8), `TTD_Replay_ExceptionMask` (u32)
- **Enum constants**: `TTD_Replay_EventType_MemoryWatchpoint`, etc.

**Pass 2 — `ffi.rs`:**
- **Projection structs**: `TtdPosition`, `TtdPositionRange`, `TtdThreadInfo`, `TtdX64Regs`, `TtdReplayResult`, `TtdActiveThreadInfo`, `TtdExceptionEvent`
- **Opaque types**: `TtdEngine`, `TtdCursor`
- **Callback typedefs**: `TtdWatchpointCb`, `TtdProgressCb`
- **All `extern "C"` function declarations**: `ttd_engine_*`, `ttd_cursor_*`
- **Layout assertions**: compile-time size/alignment/offset checks

### Clean re-exports in `types.rs`
- SDK enum types from `bindings` (with `as` aliases): `TtdEventType`, `TtdDataAccessMask`, `TtdEventMask`, `TtdExceptionMask`
- Projection types from `ffi`: `TtdPosition`, `TtdPositionRange`, `TtdThreadInfo`, etc.
- Enum constants from `bindings` (with short names):
  - `EVENT_MEMORY_WATCHPOINT`, `EVENT_STEP_COUNT`, etc.
  - `ACCESS_READ`, `ACCESS_WRITE`, `ACCESS_EXECUTE`, `ACCESS_NONE`
  - `EVENT_MASK_MEMORY_WATCHPOINT`, `EVENT_MASK_ALL`, etc.
  - `EXCEPTION_MASK_HARDWARE`, `EXCEPTION_MASK_ALL`, etc.
- Plus `PartialOrd`/`Ord` impl for `TtdPosition`

### Design for extensibility
- Currently x86_64 only (`TtdX64Regs`); designed for ARM64 via conditional compilation
- Structs are self-contained (no pointers to TTD-managed memory)
- New SDK enum values → `cargo build` picks them up automatically via Pass 1
- Add new projection types to `ttd_wrapper.h` → bindgen picks them up via Pass 2
- Add new wrapper functions to `ttd_wrapper.h` → bindgen picks them up via Pass 2
- C enums in `ttd_wrapper.h` are verified against SDK at compile time via `static_assert`

**When any C header changed**, just run `cargo build` — both bindgen passes regenerate automatically.

## Code style

Run before committing:

```bash
cargo fmt
cargo clippy --lib --tests    # zero warnings expected
cargo test                    # 77 tests, all green
```

`cargo fmt` is enforced: the project keeps a clean `cargo fmt --check`.

## GDB Frontend & Delve Integration

The GDB RSP frontend (`src/gdb/`) is built on **gdbstub** (the `ttd-dbg`
branch of `github.com/lwintermelon/gdbstub`, git dependency) and satisfies
Delve's `pkg/proc/gdbserial` client — the same backend Delve uses for `rr` on
Linux. The goal: ttd-gdbserver is a reversible Delve backend on Windows.

GDB RSP is a general mechanism (like rr): GDB, LLDB and Delve all speak it
and all support reverse debugging, so the frontend is not Delve-specific —
Delve is just the first verified client (Go debugging works end-to-end).

- **Protocol choice**: GDB RSP because Delve already speaks it and its
  reverse semantics (`bc`/`bs`, `vRun` restart) are built in.
  See `doc/delve-integration.md` (the Delve-side changes live on the
  `ttd-dbg` branch of `github.com/lwintermelon/delve`, not in this repo).
- **Reversible mapping**: `vCont;c`→continue_forward, `vCont;s`→step,
  `bc`→continue_backward, `bs`→step_back, `vRun;;<hex>`→goto,
  `qRRCmd when`→position.
- **Stop replies**: `T05` breakpoint/step, `T09` trace-end (Delve "almost exited"),
  `T00` trace-start, `T02` ^C interrupt.
- **gdbstub patches** (no patch files maintained in this repo; the patches
  live on the fork's `ttd-dbg` branch, each also submitted upstream as a
  separate PR): `UnknownPacket` target hook (Delve's `qRRCmd`), 2-digit hex
  signals in stop replies (Delve parses `resp[1:3]`), and no response flushed
  to a `k` kill (Delve expects EOF).
- **ttd-gdbserver-side adaptation** (not patches): `use_rle() = false`
  (Delve's binary decoder doesn't handle run-length), `use_no_ack_mode() =
  true`, custom `Arch` with 59 registers (GPRs + x87/SSE + `gs_base`),
  target description embedded at compile time (`include_str!` from
  `src/gdb/`, GDB's `gdb/features/i386/` files, the same ones rr ships) and
  served via `TargetDescriptionXmlOverride`.
- **gs_base = TEB**: `ttd_cursor_get_teb_thread()` (TTD `GetTebAddress`) is exposed
  as a `gs_base` register so Delve can locate the goroutine `g` without writing
  memory (TTD is read-only; Delve's usual inject-a-MOV trick can't work).
- **Memory query policy**: cursors are created with
  `QueryMemoryPolicy::GloballyConservative` (set in `ttd_cursor_create`). The
  `Default` policy may only surface memory observed by the current thread.
  Accurate recovery of old writes (e.g. the Go runtime's `allgs` array) also
  requires the trace index: `TtdEngine::open` calls `ttd_engine_build_index`
  (`.idx` next to the trace), without which goroutine enumeration reads back
  zeros. `BuildIndex`'s progress callback must be non-null (SDK 0.9.5 crashes
  on nullptr).
- **Event loop**: replay runs on a worker thread; the gdbstub `run_blocking`
  event loop polls the connection for ^C and the worker for stop reasons
  (`src/gdb/session.rs`). `k` answers with EOF, `D` with `OK`.

## File Conventions

- One primary type per file (e.g., `cursor.rs` only has `TtdCursor`, `error.rs` only has `DebugError`)
- `ttd/bindings.rs` auto-generated by bindgen from `csrc/ttd_wrapper.h` — structs, enums, callbacks, functions, `#[repr(C)]` with layout assertions
- `ttd/types.rs` re-exports from `bindings` with clean short names (`EVENT_*`, `ACCESS_*`, `EVENT_MASK_*`, `EXCEPTION_MASK_*`) + `Ord` impl for `TtdPosition`
- Error types use `thiserror::Error` derive
- Test files use inline `mod` blocks to group related tests logically
