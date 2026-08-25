# AGENTS.md — Project Architecture & Conventions

## Project Overview

TTD Time Travel Debugger — a Rust-based time-travel debugger backed by the WinDbg TTD Replay API.
Single binary with a GDB RSP frontend. x86_64 only, no symbol support.

## Architecture

```
GDB client ←GDB RSP→ GDB frontend (gdbstub) ←DebugTarget trait→ TtdProcess → TTD DLL
```

The API between the protocol frontend and the TTD debug target is the
`DebugTarget` trait (`src/target.rs`): instruction-level operations
only — execution control (forward/reverse), memory reads, breakpoints,
thread/register access, trace position navigation. No source/line concepts,
no expression evaluation, no symbol browsing. The target is read-only
(no memory/register writes).

| Layer | Rust type | File |
|-------|-----------|------|
| Target API | `DebugTarget` trait | `src/target.rs` |
| Target impl | `TtdProcess` | `src/target/process.rs` |
| GDB target | `GdbTarget<T: DebugTarget>` | `src/gdb/target.rs` |

## Module Layout

| Module | Files | Responsibility |
|--------|-------|---------------|
| `ttd/` | `ttd.rs`, `ttd/{engine,cursor,bindings,ffi,types,error}.rs` | FFI bindings to TTD C++ API. Two-pass bindgen: `bindings.rs` = SDK enums from `ttd_bindings.h` (C++ mode, `<TTD/IReplayEngine.h>`), `ffi.rs` = projection types + function declarations from `ttd_wrapper.h` (C mode). `types.rs` re-exports with clean names + `Ord` impl. |
| `target/` | `target.rs` (trait + `ModuleInfo`), `target/process.rs` + `target/process/replay.rs`, `target/{cursors,breakpoint_state,threads,breakpoint,watchpoint,error,stop_reason}.rs` | Debug target layer: `DebugTarget` trait + `TtdProcess`. Each file has one concern. |
| `gdb/` | `gdb.rs`, `gdb/{regs,mapping,commands,resume,runner,session,packet,xml,checkpoint,monitor}.rs`, `gdb/target/{base,breakpoints,inspect,unknown}.rs` + `gdb/target/test_util.rs` (test-only) | GDB RSP frontend (built on gdbstub, Delve-gdbserial-compatible) |
| `test-support/` | `src/lib.rs`, `src/rsp.rs`, `src/harness.rs`, `src/mock_target.rs` | Dev-only workspace package imported by all integration test targets (RSP client, server harness, mock target, trace bootstrap). |

### gdb/ file split

| File | Content |
|------|---------|
| `regs.rs` | Custom `Arch`: `TtdRegisters` wraps `gdbstub_arch`'s `X86_64CoreRegs` (GPRs + x87/SSE) + `fs_base`/`gs_base`; `TtdRegId` wraps `X86_64CoreRegId`; `read_register_value` serializes one register for `p`; target description embedded via `include_str!` (XML files live next to it in `src/gdb/`) |
| `target/` | `GdbTarget<T: DebugTarget>`, split by protocol area; protocol *mapping* only. `target.rs` owns the struct, constructor, worker-result mapping and the `Target` extension discovery. `base.rs` = registers/memory/threads + forward/reverse resume. `breakpoints.rs` = `Z`/`z` packets. `inspect.rs` = extended mode (`vRun`), exec-file, auxv, process info, libraries/memory-map XML, `qOffsets`, `qThreadExtraInfo`, `monitor ttd …`, target XML. `unknown.rs` = Delve-specific `qRRCmd` / `qGetTLSAddr` / signal ACK / `vCtrlC`. Every self-contained helper lives in its own top-level module (`regs.rs`, `mapping.rs`, ...). |
| `mapping.rs` | Pure wire mapping: stop reason → T packet, TTD `DataAccessMask` ↔ GDB `WatchKind` (both directions), Windows exception code → GDB signal, stop-reply thread selection. Unit-tested without a stub. |
| `commands.rs` | Delve-specific command handling: `qRRCmd` (`when`/checkpoints) and `vRun` restart forms. A `CommandTarget` view keeps it unit-testable with a plain stub. |
| `resume.rs` | `ReplayOp` + `ResumeState`: vCont action priority, clear/consume semantics, empty-range normalization. |
| `runner.rs` | `TargetRunner<T, R>`, the concurrency core between the event loop and the backend — queries inline, resumes on a worker thread, ^C abort via the backend's own `interrupt_handle`. Rejects a second concurrent `start`, reports worker panics through the result channel, and recovers the backend after a session-end interrupt. Unit-tested without TCP. |
| `session.rs` | TCP server + gdbstub `run_blocking` event loop (polls connection for ^C, worker for stop reasons); `serve_sessions(…, max_sessions)` makes the multi-session accept loop testable |
| `packet.rs` | Pure wire helpers: `parse_qrrcmd_args` (new/old qRRCmd styles), `parse_tid_from_qpacket`, `decode_hex`/`encode_hex`. Unit-tested. |
| `xml.rs` | qXfer payloads: `build_libraries_xml`, `build_memory_map_xml`, and the shared `write_xml_chunk` every qXfer read uses. Unit-tested (chunking + XML escaping). |
| `checkpoint.rs` | rr-style `CheckpointTable` (a checkpoint is just a saved `TtdPosition`) + `format_position`. Unit-tested. |
| `monitor.rs` | `monitor ttd …` rendering, driven by the `TraceView` trait (blanket-implemented for every `DebugTarget`) so it is unit-testable with a plain struct instead of a live backend;  |

Rule of thumb for `gdb/`: anything that can be tested as a pure function or as
self-contained state goes in its own module with `#[cfg(test)]` unit tests;
`gdb/target.rs` keeps only what genuinely needs the stub.

### target/ file split

| File | Content |
|------|---------|
| `target.rs` | `DebugTarget` trait (the API consumed by protocol frontends) + `ModuleInfo` |
| `process.rs` | `TtdProcess`: engine ownership, the public `DebugTarget` impl (Rust allows one trait impl per type), cursor-positioning helpers, `step_range` fast path, diagnostics and counters. Cursor pairing is delegated to `cursors.rs`; replay internals to `process/replay.rs`. |
| `process/replay.rs` | Filtered continue (TTD watchpoint callback + speculative-execution rules), position adoption after a stop, continue-stop resolution, and the single-step path. Helpers: `replay_watchpoint_filtered`, `adopt_replay_position`, `resolve_continue_stop`, `step_on_step_cursor`. |
| `cursors.rs` | `PositionedCursor` (cursor + remembered position, seek/read-back/invalidate) and `CursorPair` (persistent/query cursor + watchpoint-free step cursor), so lazy-position bookkeeping is one small reviewable type. |
| `breakpoint_state.rs` | `BreakpointState` + `WatchpointCursor`: logical breakpoint table, physical watchpoint registry, OS-tid→UniqueThreadId translation, fallible add/rollback/retry, address-keyed removal, atomic physical-operation counters. The cursor trait lets unit tests exercise failures with a fake sink. |
| `threads.rs` | `ThreadTable`: immutable trace lifetime thread catalog shared by query validation and breakpoint filters |
| `watchpoint.rs` | Physical TTD watchpoint registry: ordered diff, dedup of logical ids onto one `WatchpointKey`, and convergent retry of failed add/remove calls. Unit-tested with fake add/remove closures. |
| `error.rs` | `DebugError` enum (including registration failure and already-running replay) |
| `breakpoint.rs` | `Breakpoint`, `BreakpointKind`, `BreakpointManager` (id-ordered `BTreeMap`, so duplicate-address matching is deterministic) |
| `stop_reason.rs` | `StopReason` enum + fallible `try_from_replay_result()` (backend `Error`/unknown events are errors, never `StepComplete`) |

## Key Patterns

### Breakpoint Simulation
TTD has no native breakpoints. `BreakpointState` owns the logical `BreakpointManager` and the physical `WatchpointRegistry`; it applies/retries the latter on the persistent cursor during continue/step operations. A watchpoint callback matches hits against the logical table through the state.

Breakpoint removal is **address-keyed end to end**
(`DebugTarget::remove_breakpoint_exact(addr, size, access)`): RSP `z` packets
identify breakpoints by address only, clients re-insert breakpoints after
restarts/reconnects (so one address can hold several backend ids), and a new
session has no memory of an earlier session's id map — so the backend itself
must clear every id at a triple. The GDB layer keeps no per-session breakpoint
bookkeeping at all; the persistent cursor's physical watchpoint set is
reconciled by `BreakpointState::sync` (which drives the ordered
`WatchpointRegistry`) on every add/remove.

Breakpoint **registration is fallible**: `DebugTarget::set_breakpoint` /
`set_data_breakpoint` return `Result<u64, DebugError>`; when the TTD cursor
rejects a watchpoint the entry is rolled out of the table again and the GDB
layer answers `Ok(false)`, which gdbstub maps to the RSP error reply `E16`
(`Error::NonFatalError(22)` — the code is written on the wire in hex, see
`gdbstub/src/stub/core_impl/breakpoints.rs`) instead of acknowledging a
breakpoint that would never fire.

Note: `QListThreadsInStopReply` stays deliberately unsupported. Implementing
it requires a fourth gdbstub fork patch (the hook doesn't exist upstream) and
only saves Delve one qfThreadInfo round-trip per stop — not worth the fork
maintenance.

### Target types
The target API uses the TTD projection types directly (`TtdPosition`,
`TtdX64Regs` from `ttd/types.rs`) plus its own `StopReason`, `DebugError`,
`BreakpointKind`, `ModuleInfo`. There is no separate protocol-type layer.

### Thread state
Threads are queried on demand at the current trace position:
`active_thread_ids()` enumerates live threads; `thread_state(tid)` returns
`(registers, teb)` for one thread. The TEB address is exposed as the
`gs_base` register so Delve can locate the goroutine `g`.

`thread_state` returns `None` for a tid that never appears in the trace's
thread table: TTD hands back a zeroed context for unknown threads, and
serving that (RIP = 0, RSP = 0) would make a stale tid look like a thread
stopped at address 0.

### Engine ownership (no lifetime parameters)
The SDK states that once the engine is destroyed, the only valid operation on a
cursor is `Destroy()`. Instead of modelling that as a borrow
(`TtdCursor<'engine>` + `PhantomData`, which would have to be erased to
`'static` for anything stored in a struct), the cursor **owns a strong
reference**: `TtdEngine` is a handle around `Arc<EngineInner>`, and
`TtdCursor::new(engine: Arc<EngineInner>)` stores a clone. The engine therefore
outlives every cursor by construction — no lifetime parameter, no
`PhantomData`, no drop-order invariant, no `unsafe` lifetime erasure.

`EngineInner` is `Send` but deliberately **not** `Sync` (the C++ engine is not
thread-safe), so the `Arc` can never be used to share the engine across
threads; `TtdCursor` is `Send` through an explicit `unsafe impl`, exactly the
guarantee the old design asserted by hand.

### Cursor `&self` vs `&mut self`
A cursor is a handle to C++ state: every operation mutates the C++ object
through a raw pointer, which is interior mutability, so **operations take
`&self`**. Only the callback slots (`set_watchpoint_callback`,
`with_watchpoint_callback`, `set_progress_callback`, `clear_*`) take
`&mut self`, because those store *Rust* closures whose address the C side
keeps. Consequence: `TtdProcess` holds plain `Box<TtdCursor>` — no `RefCell`.
(The boxes are not a soundness requirement: the C callback context is the
callback slot's inner heap box — stable across moves — never the cursor's
own address, so a cursor may be moved even with a callback installed.)

### Persistent cursor, step cursor & incremental watchpoints
`TtdProcess` owns TWO cursors for its whole lifetime.

- **Persistent cursor** — carries the client's watchpoints; serves every
  query (`with_cursor` → `at_position`).
- **Step cursor** — never carries watchpoints, so a step advances exactly one
  instruction even across a breakpoint (GDB semantics). Steps used to create a
  throwaway cursor each time; a reused one is ~150× faster (616 µs → 3.9 µs
  per step on the sample trace), which is what makes range stepping
  (`next`/`step` over a source line) usable.

Both are wrapped by `target/cursors.rs`: `CursorPair` owns the persistent
query cursor and the step cursor, and each `PositionedCursor` remembers where
it is. `SetPosition` runs only when the debug position has moved; a
`prepare` call reads the actual landing position back (TTD rounds to the
nearest valid position), and failed/partial operations call `invalidate`.
Only `goto`, replay and step move a cursor, and all of them update the cache. `tests/target/threads.rs` guards this with
cross-checks against a raw `TtdCursor` oracle
(`queries_after_steps_match_a_raw_cursor_at_the_same_position`) and a
step-after-goto invariant (`step_after_goto_continues_from_the_destination`);
both fail if a cache stops being updated. `DebugTarget::stats()` exposes the
`queries` / `cursor_seeks` counters plus the physical `watchpoint_adds` /
`watchpoint_removes` counters (served as `monitor ttd stats`), and
`tests/target/threads.rs::repeated_queries_do_not_reseek_the_cursor` asserts the ratio
directly — 40 queries at one position must cost 0 extra seeks. The watchpoint
counters make the dedup/registry contract observable from tests.

Client breakpoints are translated into TTD memory watchpoints
**incrementally**: each set/remove diffs the desired physical-watchpoint set
(`WatchpointKey`, deduplicated because Delve re-inserts ids after restart)
against what is registered on the persistent cursor. The registry keeps an
ordered `BTreeSet`, so add/remove calls are deterministic, duplicate logical
ids never cause duplicate physical calls, and failed calls stay tracked for
the next sync to retry. OS tids are translated to UniqueThreadIds through a
shared immutable `ThreadTable` built from the trace's lifetime thread list. When several breakpoints cover one address, a hit
reports the one restricted to the hitting thread (a per-thread breakpoint is
the more specific request); among equally specific matches the lowest id wins,
so the result is independent of hash order.

### Range stepping (`vCont;r` / Delve `next`)
`DebugTarget::step_range(thread, start, end)` is part of the target API, not
just a protocol loop. `TtdProcess` overrides the portable default to stay on
the watchpoint-free step cursor and read the PC with `pc_thread` from that same
cursor after each instruction. The default implementation would call
`thread_state` after every step, which routes through the persistent query
cursor and forces a `SetPosition` (possibly replaying from a keyframe) per
instruction — Delve's `next`/`stepout` are range steps, so that used to make
source-level stepping dramatically slower than it needed to be.
Speculative-execution note: `step_range` does not use the watchpoint callback
at all. It only consumes `StepCount` replay results and reads the PC from the
exact stopped cursor position, so TTD's speculative watchpoint hits can
never influence the range decision or its stop position. The continue path
keeps the original trace-boundary `limit` strategy and the existing
speculative-hit comment in `TtdProcess::watchpoint_replay` unchanged.

`tests/target/range.rs::range_step_reuses_step_cursor_without_query_seeks` pins the
contract (zero extra seeks and zero extra persistent-cursor queries after
priming); `tests/gdb_ttd.rs` exercises the packet over a real trace, and
`scratch/live-vs-replay.sh` with a `next`-based init script checks it against
live debugging.
### Multi-session persistence
`run_gdb_server` accepts sessions forever and threads the backend through
each one, so position/breakpoints survive reconnects.
`serve_sessions(listener, backend, Some(n), shutdown)` is the bounded variant
used by `tests/gdb/sessions.rs::test_engine_state_persists_across_sequential_sessions`
(the contract: position moved in session 1 is visible via `qRRCmd when` in
session 2, and a session-1 breakpoint is still removable there).

### Interrupt pipeline
Cancellation flows bottom-up with no manual wiring:
`TtdProcess::interrupt_handle()` (a `DebugTarget` factory method returning an
`Arc<dyn Fn()>` around a lock-free `AtomicPtr` slot published during replays)
→ `TargetRunner::new` picks it up once → the ^C path calls
`TargetRunner::request_interrupt`. Targets whose replays can't block return
`None` and pay nothing. Session teardown also fires the handle and waits
(bounded) for the worker to release the backend, so an abrupt client
disconnect during a long replay does not strand the backend in a detached
worker.

### Safe callback wrappers
Raw C callbacks never leak out of `src/ttd/`. `TtdCursor` exposes
`set_watchpoint_callback` / `with_watchpoint_callback` /
`set_progress_callback` (owned or scoped closures; the C side only ever sees
a thin pointer to a heap/stack slot the Rust side owns and frees after
clearing the C registration). `TtdEngine::build_index_with_progress` forwards
keyframe progress to a stack-local closure — `BuildIndex` is synchronous, so
no storage is needed. The C shim keeps an internal progress reporter because
the SDK crashes on a nullptr callback.

## Test Structure

| File | Type | TTD needed? |
|------|------|-------------|
| `src/**/*.rs` `#[cfg(test)]` | Unit: breakpoint manager, breakpoint-state rollback/retry, physical-watchpoint registry sync/dedup, thread catalog, stop-reason mapping, watch-kind/exception→signal encoding, TargetRunner concurrency/panic delivery, `qRRCmd`/`vRun` command handling, `ReplayOp`/`ResumeState`, qGetTLSAddr parsing, hex codec, qXfer chunking + XML, checkpoint table, `monitor ttd` rendering, `TtdPosition` ordering | No |
| `tests/gdb/suite.rs` + `tests/gdb/*.rs` | Integration: GDB RSP over TCP (configurable mock backend, replays Delve handshake, cross-session persistence). Split by protocol area: `handshake`, `breakpoints`, `reverse`, `delve`, `introspection`, `resume`, `monitor`, `sessions`. | No |
| `test-support/` | Shared integration-test support, imported by every test target: `rsp.rs` (wire client + `qRRCmd`/`monitor`/`vRun` helpers), `harness.rs` (server startup + Delve handshake), `mock_target.rs` (configurable `DebugTarget`), `lib.rs` (`create_engine`). | No |
| `tests/target/suite.rs` + `tests/target/*.rs` | Integration: TtdProcess via DebugTarget trait, split into `execution`, `threads`, `position`, `breakpoints`, `range` modules. | Yes (reported `ignored` without trace) |
| `tests/gdb_ttd.rs` | Integration: GDB RSP frontend with real trace | Yes (reported `ignored` without trace) |
| `tests/ttd.rs` | Integration: TtdEngine + TtdCursor FFI level (incl. safe callback wrappers) | Yes (reported `ignored` without trace) |

```powershell
# All non-TTD tests
cargo test --lib --test gdb

# TTD integration (needs a .run trace; TTDReplay DLLs on PATH)
$env:TTD_TRACE_PATH = "path.run"
cargo test --test target --test gdb_ttd --test ttd -- --nocapture
```

Every test that needs a recorded trace is tagged
`#[cfg_attr(not(has_ttd_trace), ignore)]`: when `TTD_TRACE_PATH` is unset,
build.rs does not emit the `has_ttd_trace` cfg and libtest reports the tests
as **ignored** (honest skip, not a silent pass) — plain `cargo test` therefore
works in CI without a trace.

## CLI

```bash
# GDB RSP frontend (Delve / plain GDB) over TCP
ttd-gdbserver <trace.run> --listen 127.0.0.1:1234

# Verbosity: -v = this crate's debug logs (RSP packets, breakpoint sync,
# replays); -vv additionally enables gdbstub's internal logs.
ttd-gdbserver <trace.run> -v

# Server accepts one session at a time; engine state persists across sessions.
# Ctrl+C shuts down gracefully (finish current work, release trace files, exit 0).
```

## Dependencies

- `ctrlc` for graceful ^C shutdown (binary-level handler -> shutdown flag -> accept loop)
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
- Enum constants from `bindings`, re-exported under their bindgen names:
  `TTD_Replay_EventType_*`, `TTD_Replay_DataAccessMask_*`,
  `TTD_Replay_EventMask_*`, `TTD_Replay_ExceptionMask_*`
  (there are deliberately no short aliases — callers use these names directly)
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
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

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
  `T00` trace-start, `T02` ^C interrupt. `T02` has two sources that must stay
  in sync: the event loop's `on_interrupt` (protocol ^C) and
  `StopReason::Interrupted` (the TTD `Interrupted` event surfacing through
  `map_stop_reason`) — the backend-aborted twin, so a backend that aborts
  itself reports identically on the wire.
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
- `ttd/types.rs` re-exports from `bindings`/`ffi` under their bindgen names + `Ord` impl for `TtdPosition`
- Error types use `thiserror::Error` derive
