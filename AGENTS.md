# AGENTS.md — Project Architecture & Conventions

## Project Overview

TTD Time Travel Debugger — a Rust time-travel debugger backed by the WinDbg
TTD Replay API. Single binary with a GDB RSP frontend. x86_64 only, no symbol
support.

This file documents the *structure* (which file holds what) and the invariants
an agent must not break. The *dynamics* (what happens when a client continues,
queries, or hits ^C) live in [`doc/architecture.md`](doc/architecture.md); the
Delve client contract in `doc/delve-integration.md`; the replay API's own
model (positions, cursors, events, masks, memory policy, the absent breakpoints)
and how it maps onto `src/ttd`/`src/target` in
[`doc/ttd-internals.md`](doc/ttd-internals.md).

## Architecture

```
GDB client ←GDB RSP→ GDB frontend (gdbstub) ←DebugTarget trait→ TtdProcess → TTD DLL
```

The `DebugTarget` trait (`src/target.rs`) is the API between protocol frontend
and TTD backend: instruction-level operations only — execution control
(forward/reverse), memory reads, breakpoints, thread/register access, trace
position navigation. No source/line concepts, no expression evaluation, no
symbols. The target is read-only (no memory/register writes).

| Layer | Rust type | File |
|-------|-----------|------|
| Target API | `DebugTarget` trait | `src/target.rs` |
| Target impl | `TtdProcess` | `src/target/process.rs` |
| GDB target | `GdbTarget<T: DebugTarget>` | `src/gdb/target.rs` |

## Module Layout

| Module | Files | Responsibility |
|--------|-------|---------------|
| `ttd/` | `ttd.rs`, `ttd/{ffi,engine,cursor,callback,types,error,interrupt}.rs` | `ffi.rs` is the cxx bridge to `ttd/cpp/ttd_cxx.*`; `engine.rs`/`cursor.rs` own the SDK handles and expose positions, memory, threads, watchpoints and callbacks; `types.rs` is the value vocabulary (`TtdPosition`, `TtdReplayResult`, …) and the SDK constant masks; `callback.rs` escalates panics across the FFI boundary; `interrupt.rs` is the cross-thread ^C channel. This is where the crate's `unsafe` lives. |
| `target/` | `target.rs` (trait + re-exports), `target/{module_info,thread_extra_info,diagnostics,stats}.rs` (data types), `target/process.rs` + `target/process/{execution,target_impl}.rs`, `target/{cursors,breakpoint_state,threads,breakpoint,watchpoint,trace_metadata,error,stop_reason}.rs` | Debug target layer: `DebugTarget` + `TtdProcess`. One primary type per file. |
| `gdb/` | `gdb.rs`, `gdb/{regs,mapping,commands,resume,runner,session,packet,xml,pe,checkpoint,monitor}.rs`, `gdb/target/{base,breakpoints,extended,description,memory_map,thread_info,monitor_cmd,unknown}.rs` + `gdb/target/test_support.rs` (test-only) | GDB RSP frontend (on gdbstub, Delve-gdbserial-compatible) |
| `tests/common/` | `mod.rs`, `rsp.rs`, `harness.rs`, `mock_target.rs` | Shared integration-test support, pulled in by every test crate via `mod common;` |

### gdb/ file split

| File | Content |
|------|---------|
| `regs.rs` | Custom `Arch`: `gdbstub_arch`'s `X86_64CoreRegs` + `fs_base`/`gs_base`; target description XML embedded via `include_str!` (files next to it in `src/gdb/`) |
| `target.rs` | `GdbTarget` struct/ctor, worker-result mapping, extension discovery, resume-op dispatch (`execute_op`), and the rr-style end state (`end_state` + `spawn_replay`: a forward resume after a process-exit stop answers `W00`; a backward resume or restart clears it) |
| `target/base.rs` | registers/memory/threads + forward/reverse resume |
| `target/breakpoints.rs` | `Z`/`z` packets |
| `target/extended.rs` | `vRun`/`R` restart and process lifecycle |
| `target/description.rs` | exec-file, auxv, `qOffsets`, target XML |
| `target/memory_map.rs` | `qXfer:libraries` only (segment = runtime `.text` address); `qXfer:memory-map` deliberately not advertised (GDB treats it as the complete readable set — `mem inaccessible-by-default` defaults to on — and would refuse stack/TEB reads) |
| `target/thread_info.rs` | `qThreadExtraInfo` |
| `target/monitor_cmd.rs` | `monitor ttd …` dispatch |
| `target/unknown.rs` | packets gdbstub has no trait for (`qRRCmd`, `qGetTIBAddr`/`qGetTLSAddr`, `QPassSignals`, `vCtrlC`, handshake declines); client-neutral — per-client decisions live in `doc/delve-integration.md` |
| `mapping.rs` | Pure wire mapping: stop reason → T packet, `DataAccessMask` ↔ `WatchKind`, exception code → signal, stop-reply tid. Unit-tested |
| `commands.rs` | `qRRCmd` (`when`/checkpoints) and `vRun` restart forms (unit-tested via a `CommandTarget` view) |
| `resume.rs` | `ReplayOp` + `ResumeState`: vCont action priority, clear/consume semantics |
| `runner.rs` | `TargetRunner<T, R>`: the concurrency core — queries inline when stopped, resumes on a worker thread, ^C via the backend's `interrupt_handle`, `try_lock` never parks the event loop. Unit-tested without TCP |
| `session.rs` | TCP server + gdbstub event loop; `serve_sessions(…, persist)` = testable accept loop (one session by default, until shutdown when persisting); a protocol failure ends only the current session |
| `packet.rs` | Pure wire helpers: `parse_qrrcmd_args`, `parse_tid_from_qpacket`, hex codec. Unit-tested |
| `xml.rs` | qXfer payloads: `build_libraries_xml`, shared `write_xml_chunk`. Unit-tested |
| `pe.rs` | Minimal PE section-table reader for the `.text` RVA behind `qOffsets`/libraries segments (0x1000 fallback). Unit-tested |
| `checkpoint.rs` | rr-style `CheckpointTable` (a checkpoint is a saved `TtdPosition`); owned by the session layer so checkpoints persist across reconnects |
| `monitor.rs` | `monitor ttd …` rendering via the `TraceView` trait (unit-testable with a plain struct) |

Every read-only backend packet goes through `try_backend`; handlers with a
`TargetResult` path answer `E` on a busy backend, while fatal-only methods
answer neutrally (empty thread list, `target is running` extra-info, cached
`qOffsets` text-segment address, ignored `R`).

Rule of thumb: anything testable as a pure function or self-contained state
gets its own module with `#[cfg(test)]` unit tests; `gdb/target.rs` keeps only
what genuinely needs the stub.

### target/ file split

| File | Content |
|------|---------|
| `target.rs` | `DebugTarget` trait + re-exports |
| `module_info.rs` / `thread_extra_info.rs` / `diagnostics.rs` / `stats.rs` | The trait's data types, one primary type per file |
| `process.rs` | `TtdProcess`: construction (the two cursors keep the engine alive) + shared cursor-positioning plumbing (`at_position`/`with_cursor`/`adopt_cursor_position`/…) |
| `process/target_impl.rs` | The single `DebugTarget` impl block — the whole backend contract in one view (Execution / Breakpoints / Queries / Interrupt) |
| `process/execution.rs` | Replay internals: filtered continue (watchpoint callback + speculative-execution rules), position adoption, `classify_continue_stop` (pure decision core), single-step path, type-erased interrupt slot |
| `cursors.rs` | `PositionedCursor` (cursor + remembered position) and `CursorPair` (persistent + watchpoint-free step cursor) |
| `breakpoint_state.rs` | Logical breakpoint table + physical watchpoint registry: fallible add/rollback/retry, address-keyed removal, physical-op counters |
| `threads.rs` | `ThreadTable`: immutable thread catalog shared by query validation and breakpoint filters |
| `trace_metadata.rs` | `TraceMetadata`: one open-time snapshot of threads/modules/exceptions so query paths avoid repeated FFI calls |
| `watchpoint.rs` | Physical watchpoint registry: ordered diff, dedup, convergent retry. Unit-tested with fake closures |
| `error.rs` / `breakpoint.rs` / `stop_reason.rs` | `DebugError`; `Breakpoint`+`BreakpointKind`+`BreakpointManager` (id-ordered); `StopReason` + `TryFrom<&TtdReplayResult>` |

## Key Patterns

### Breakpoint simulation
TTD has no native breakpoints. `BreakpointState` owns the logical
`BreakpointManager` and the physical `WatchpointRegistry`; a watchpoint
callback matches hits against the logical table.

- Removal is **address-keyed end to end** (`remove_breakpoint_exact`): RSP `z`
  identifies breakpoints by address only, clients re-insert after
  restarts/reconnects (one address, several backend ids), and a new session
  has no memory of an old id map. The GDB layer keeps no per-session
  bookkeeping; `BreakpointState::sync` reconciles the persistent cursor's
  physical set on every add/remove.
- Registration is **fallible** (`Result<u64, DebugError>`): a TTD rejection
  rolls the entry back out and the GDB layer answers `Ok(false)` → RSP `E16`,
  instead of acknowledging a breakpoint that would never fire.
- `QListThreadsInStopReply` is **declined** (explicit empty reply) because
  gdbstub has no hook for it; Delve falls back to `qfThreadInfo`.

### Target types
The target API uses the TTD projection types directly (`TtdPosition`,
`TtdX64Regs`) plus its own `StopReason`, `DebugError`, `BreakpointKind`,
`ModuleInfo`. No separate protocol-type layer.

### Thread state
Threads are queried on demand at the current position: `active_thread_ids()`
enumerates, `thread_state(tid)` returns `(registers, teb)`; the TEB is exposed
as `gs_base` so Delve can locate the goroutine `g`. `thread_state` returns
`None` for tids absent from the trace's thread table — TTD hands back a zeroed
context for unknown threads, and serving RIP = 0 would make a stale tid look
like a thread stopped at address 0.

### Thread-local stack/TEB memory
`ICursorView::QueryMemoryBuffer` is position/thread-sensitive: once the
persistent cursor is adopted at the stopped thread/position, the cursor's
`GloballyAggressive` policy (set in `Cursor`'s constructor, `src/ttd/cpp/ttd_cxx.cpp`)
returns OS stack and TEB pages — no snapshot cache needed. GDB's memory map is deliberately not
advertised (see `target/memory_map.rs` above).

### Engine ownership
A cursor must not outlive its engine. `ReplayCursor` holds an
`Arc<EngineInner>` (the SDK `Engine` and its `Drop`), so `ReplayEngine::cursor()`
returns a cursor with no lifetime parameter. `TtdProcess` therefore stores no
engine field at all — creating both long-lived cursors keeps the engine alive,
and the local `ReplayEngine` can be dropped when `TtdProcess::open` returns.

### Cursor `&mut self`
`ReplayCursor`'s navigation entry points (`set_position`, `replay_*`,
`watchpoint`, `set_event_mask`, the callback slots) take `&mut ReplayCursor`,
while the `DebugTarget` query methods take `&self`. `PositionedCursor` wraps the
cursor in a `RefCell` and exposes short `cursor()` / `cursor_mut()` borrows; the
target is reached only through `TargetRunner`'s mutex, so no borrow overlaps.
Moving a cursor with a callback installed is fine: the SDK's callback context is
the address of a heap `WatchpointSlot`, never the cursor's own address. It has to
be a heap slot — a stack slot can be reused by the compiler once the install
call returns, and the replay then reads garbage (which TTD's SEH swallows,
silently turning every hit into "keep going").

### Persistent cursor, step cursor & incremental watchpoints
`TtdProcess` owns TWO cursors for its whole lifetime, both wrapped by
`target/cursors.rs` (`PositionedCursor` remembers its position; `SetPosition`
runs only when the position moved; failures `invalidate`):

- **Persistent cursor** — carries the client's watchpoints; serves every query.
- **Step cursor** — never carries watchpoints, so a step advances exactly one
  instruction even across a breakpoint. A long-lived step cursor is ~150×
  faster per step than a throwaway one (3.9 µs vs 616 µs), which is what makes
  range stepping usable.

Client breakpoints become TTD watchpoints **incrementally**: each set/remove
diffs the desired physical set (`WatchpointKey`, deduplicated) against the
registered one; the ordered registry keeps calls deterministic and retries
failed ones at the next sync. OS tids map to UniqueThreadIds through the
shared `ThreadTable`. When several breakpoints cover one address, a hit
reports the per-thread one if any, else the lowest id.

### Range stepping (`vCont;r` / Delve `next`)
`DebugTarget::step_range(thread, start, end)` is target API, not a protocol
loop. `TtdProcess` stays on the step cursor and reads the PC from it after
each instruction — the default impl would route a `thread_state` query through
the persistent cursor per instruction (a `SetPosition`, possibly a keyframe
replay, each time), and Delve's `next`/`stepout` are range steps. `step_range`
ignores the watchpoint callback entirely (StepCount results + exact stopped
position only), so TTD's speculative watchpoint hits cannot influence it.
Pinned by `tests/target_range.rs::range_step_reuses_step_cursor_without_query_seeks`.

### Multi-session persistence
By default the server exits when the first session ends; with
`--accept-multiclient` it accepts sessions until ^C and threads the backend
(and the `CheckpointTable`, handed to each session by value) through each
one, so position/breakpoints/checkpoints survive reconnects. Pinned by
`tests/gdb_sessions.rs`.

### Interrupt pipeline
Cancellation flows bottom-up with no manual wiring:
`TtdProcess::interrupt_handle()` (an `Arc<dyn Fn()>` around a lock-free slot
published during replays) → `TargetRunner::new` picks it up → ^C calls
`request_interrupt`. Session teardown fires the handle and waits (bounded) for
the worker to release the backend.

A blocking replay is aborted by poking the published cursor. A range step is
not: it issues one short `StepForward` per instruction, so there is no single
SDK call to abort. It polls `TtdProcess::interrupt_epoch` instead, which the
handle bumps and `DebugTarget::begin_resume` resets before every resume
operation (called by `TargetRunner::start`). Both halves are load-bearing:
without the reset a ^C that arrived while stopped would abort the next range
step, and without the poll the loop runs to the trace boundary.

The publish/poke pair lives in `ttd/interrupt.rs::InterruptSlot`. Publishing
stores the cursor's address in an atomic; the guard clears it on drop —
including on the panic path. The replay path holds `&mut ReplayCursor` for
the whole published scope, so the pointer cannot dangle. That is what lets
`src/lib.rs` deny `unsafe_code` everywhere except `ttd/`: no other module can
hold or dereference a cursor pointer.

### Safe callback wrappers
The `ttd` crate owns the C callback shim: a user callback never unwinds into
TTD, the panic payload is stashed, and the replay call that was running
re-throws it afterwards. `cursors.rs` installs a borrowed closure for the
duration of one filtered replay (`install_borrowed_callback`) and clears it
through a drop guard, so a panic still uninstalls it. `TtdProcess` catches the
re-thrown panic to clear the active-replay slot and invalidate the cursor
cache before propagating.

## Test Structure

Unit tests live beside the code (`#[cfg(test)] mod tests`); shared unit-test
helpers live in `src/gdb/target/test_support.rs`. Integration tests are one
Cargo target per `tests/*.rs` file (no `Cargo.toml` declaration needed), each
starting with `mod common;`.

| File | Type | TTD needed? |
|------|------|-------------|
| `src/**/*.rs` `#[cfg(test)]` | Unit tests for every pure module named above | No |
| `tests/gdb_*.rs` | GDB RSP over TCP with the mock backend, split by protocol area: `handshake`, `breakpoints`, `reverse`, `delve`, `introspection`, `resume`, `monitor`, `sessions` | No |
| `tests/target_*.rs` | `TtdProcess` via `DebugTarget`: `execution`, `threads`, `position`, `breakpoints`, `range`, `properties` (step round trip, `goto` path independence, abortable range step), `watchpoint_hits` (a write stop shows the new value; reversing restores the old one) | Yes (ignored without trace) |
| `tests/gdb_ttd.rs` | GDB frontend with a real trace | Yes (ignored without trace) |
| `tests/target_exceptions.rs` | Exception stops against the corpus's fault recording (`TTD_FAULT_TRACE_PATH`): the recorded code/address reach the stop reason, a second continue passes the fault | Yes (ignored without trace) |

```powershell
mise run test:rust                # full suite; [env] supplies the corpus traces
cargo test                        # without traces: trace tests are ignored
cargo test --test gdb_handshake   # one area
```

Tests needing a trace are tagged
`#[cfg_attr(not(has_ttd_trace), ignore = "requires TTD_TRACE_PATH")]` (and the
exception tests use `has_ttd_fault_trace` / `TTD_FAULT_TRACE_PATH`): without
the variable build.rs emits no cfg and libtest reports them as **ignored**
(honest skip) — plain `cargo test` works in CI.

Both variables come from the environment, never from a path baked into the
code or a second config file: `mise.toml`'s `[env]` block supplies the corpus
recordings (`{{config_root}}/tests/e2e/traces/…`) for this checkout, so
`mise run test:rust` exercises the real engine on a known recording, while CI
either sets the variables or gets the honest `ignored` result.

Source-level end-to-end testing (Delve/GDB + real traces) lives in
`tests/e2e/` — one entry point (`e2e.py <command>`), shared machinery in
`harness/`, one directory per program under `programs/`. The two gates:
`forward` diffs a replay against one recorded live run, `reverse` diffs
reverse runs of the same trace with no masking. Scripts navigate and print
only — the transcript is the assertion. See `tests/e2e/README.md`.

## CLI

```bash
ttd-gdbserver <trace.run> --listen 127.0.0.1:1234   # serve GDB RSP over TCP
ttd-gdbserver <trace.run> --accept-multiclient      # keep serving until ^C
ttd-gdbserver <trace.run> -v                        # -v: crate debug logs; -vv: + gdbstub
# One session at a time; by default the server exits when the session ends
# (dlv's --accept-multiclient polarity). With the flag it keeps listening and
# engine state persists across reconnects.
# Ctrl+C stops accepting and lets the current session finish (again to force).
```

## Dependencies

- `ctrlc`, `thiserror`, `log`/`env_logger`, `clap`, `quick-xml` (qXfer XML)
- `cxx` + `cxx-build` — the typed bridge to `src/ttd/cpp/ttd_cxx.cpp`; no bindgen.
  `build.rs` compiles the C++ wrapper and generates the bridge glue.
- TTD SDK: `Microsoft.TimeTravelDebugging.Apis` NuGet via `TTD_SDK_DIR`;
  runtime DLLs (`TTDReplay.dll`) on `PATH`
- `gdbstub` + `gdbstub_arch` — a git dependency on branch `ttd-dbg` of
  `github.com/lwintermelon/gdbstub`; the branch carries the protocol
  extensions this server needs (an `UnknownPacket` hook, 2-digit stop
  signals, kill without a reply, and unqualified `vCont;s`/`vCont;r` stepping
  the selected thread), which is why it is not on crates.io. `qProcessInfo` is
  deliberately not implemented: it is an LLDB-only extension that no verified
  client sends (Delve and GDB both fall back before reaching it, and rr does
  not implement it either).

## Bindings

`src/ttd/cpp/ttd_cxx.hpp` declares the typed C++ surface: two classes (`Engine`,
`Cursor`) holding the SDK's `UniqueReplayEngine`/`UniqueCursor`, plus value
conversions. `src/ttd/ffi.rs` is the cxx bridge; `build.rs` compiles the
wrapper and generates the bridge glue. Deliberate limits:

- **SDK interfaces are not exposed.** TTD's API is COM-shaped (pure virtual),
  so the C++ wrapper is the only layer that calls it; cxx sees ordinary
  methods.
- **No SDK layout crosses cxx.** Position/thread/module/exception records are
  cxx shared structs; memory is `&mut [u8]`; amd64 register state is serialized
  into a fixed little-endian word stream (`81` words) and decoded in
  `ttd/types.rs`. The C++ ABI stays in `ttd_cxx.cpp`.
- **Interpretation stays in Rust.** `ttd/cursor.rs` picks the amd64 layout from
  the trace's `ProcessorArchitecture`; access *types* become masks in Rust.
- `src/ttd/types.rs` — value types (`TtdPosition`, `TtdThreadInfo`,
  `TtdX64Regs`, …). Simple 1:1 records are re-exported cxx shared structs;
  semantic records stay separate. `EventType` and `DataAccessType` carry
  `Unknown(u8)`; `DataAccessMask`, `EventMask` and `ExceptionMask` are
  `bitflags`.
- `#![deny(unsafe_code)]` in `src/lib.rs`; only `src/ttd` opts back in.
- x86_64 only for now; designed for ARM64 via conditional compilation.

## Code style

Run before committing: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.

`Cargo.toml` sets `clippy::pedantic = warn`; the crate is pedantic-clean
(integration tests included), with local `#[allow]`s carrying their reason.
Conventions the lints encode: inline format args (`format!("{x}")`); GDB
(`u64`) ↔ Windows (`u32`) tid conversions via `u32::try_from`/`u64::from`;
`map_or`/`map_or_else` over `map(..).unwrap_or(..)`; `# Errors`/`# Panics`
docs on public items; `#[must_use]` on pure accessors.

`src/lib.rs` sets `#![deny(unsafe_code)]`; `src/ttd/` opts back in with
`#[allow(unsafe_code)]` on the module declaration. A new raw pointer therefore
has to be introduced where the FFI contracts already live; anywhere else it is
a compile error rather than a review comment.

Comments explain the current design to a reader — never narrate what a change
did or what earlier code looked like.

## GDB Frontend & Delve Integration

The frontend satisfies Delve's `pkg/proc/gdbserial` client (Delve's rr
backend), making ttd-gdbserver a reversible Delve backend on Windows. GDB RSP
is a general mechanism — GDB/LLDB speak it too; Delve is just the first
verified client. The full client contract, packet by packet, is
`doc/delve-integration.md`; the Delve-side changes live on the `ttd-dbg`
branch of `github.com/lwintermelon/delve`.

Invariants that are easy to break and expensive to debug:

- **Stop replies**: `T05` breakpoint/step, `T09` trace-end *and* recorded
  process exit (Delve's "almost exited"), `T00` trace-start, `T02` ^C, and
  `W00` only when the client resumes forward again from a process-exit stop
  (`end_state` in `gdb/target.rs`; idempotent, unlike rr's FATAL).
- **Memory query policy**: a complete index (`TtdProcess::open` →
  `ensure_index` in `target/process.rs`).
  The policy picks *how hard the engine looks*, not whether the answer is
  right: with a complete index the four global policies return identical bytes
  at every sampled position (only `GloballyConservative` sometimes serves an
  older data line), so switching policies cannot fix a stale value. Without an
  index old writes read back as zeros (Go's `allgs`). A build killed part-way
  leaves a truncated `.idx` that the engine still reports as `Loaded`, so a
  leftover `.idxt` forces a rebuild; `monitor ttd info` reports the state as
  `index:`. `BuildIndex`'s progress callback must be non-null (the SDK crashes
  on nullptr).
- **Memory is not a snapshot**: one `QueryMemoryBuffer` call can be assembled
  from several trace positions (`MemoryBuffer`: "possibly collected from
  multiple positions"), and the newest recoverable data line for an address
  gets older the further the query moves past the last write that produced
  one. `DebugTarget::read_memory_ranges` + `monitor ttd mem` expose the
  per-run position (`MemoryRange::Sequence`, fragment-granular). Never treat
  an old `sequence` as proof of a wrong value, nor a multi-position buffer as
  impossible: both are documented engine behaviour.
- **^C during a range step**: the step loop polls `TtdProcess`'s interrupt
  epoch, which `TargetRunner::start` resets through `DebugTarget::begin_resume`
  before each operation. Without the reset a ^C that arrived while stopped
  aborts the next range step; without the poll the loop cannot be stopped at
  all (each iteration is a short `StepForward`, which the SDK's cursor
  interrupt has nothing to abort).
- **Query scheduling**: handlers take `TargetRunner::try_lock` and never park
  the event loop (see `runner.rs`); pinned by
  `tests/gdb_resume.rs::test_backend_packets_do_not_block_while_replay_runs`.

## File Conventions

- One primary type per file (e.g., `cursors.rs` holds `PositionedCursor` +
  `CursorPair`)
- Error types use `thiserror::Error` derive
