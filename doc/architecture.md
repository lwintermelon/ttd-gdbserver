# Architecture

How ttd-gdbserver is put together, why, and how a request flows through it.
`AGENTS.md` documents the *structure* (which file holds what); this document
documents the *dynamics* (what happens when) and the reasoning behind the
load-bearing decisions. For the Delve client contract specifically — a table
of every server-side decision that exists because of it — see
`doc/delve-integration.md`. For what the TTD replay API itself provides (and
refuses to provide) and how each concept maps onto this codebase, see
`doc/ttd-internals.md`.

## Layers and the seam between them

```
GDB/LLDB/Delve client
        │  GDB Remote Serial Protocol (TCP)
        ▼
  src/gdb/            protocol frontend (gdbstub Target impl, GdbTarget<T>)
        │  DebugTarget trait  ── the only door between protocol and engine
        ▼
  src/target/         TtdProcess: replay control, cursor discipline, breakpoints
        │  ReplayEngine / ReplayCursor  (safe wrapper)
        ▼
  src/ttd/            SDK handle ownership, value vocabulary, ^C interrupt slot
        │  bindgen-generated `extern` declarations
        ▼
  src/ttd_sys/ ──csrc/ttd_sys.cpp──▶  TTDReplay.dll (C++ replay engine)
```

Three properties of this shape are deliberate:

1. **The protocol layer is generic**: `GdbTarget<T: DebugTarget>` knows
   nothing about TTD. The trait is the only API it consumes, which is what
   makes the whole frontend testable with a configurable in-memory mock
   (`tests/common/mock_target.rs`) and unit-testable with a recording stub
   (`src/gdb/target/test_support.rs`).
2. **The target layer is a domain model, not a generic debugger
   abstraction**. `DebugTarget`'s vocabulary is the TTD domain on purpose:
   `TtdPosition`, `TtdX64Regs`, `StopReason::TraceEnd`, reverse execution.
   The project scope is "time-travel debugging of a recorded x86_64 Windows
   trace", and the trait says so. A future non-TTD backend would adapt into
   this model rather than the other way around.
3. **The bindings are one thin layer, then a safe one**. `src/ttd_sys` is
   bindgen output for `csrc/ttd_sys.hpp`, a C++ surface that forwards to the
   SDK; `src/ttd` owns the handles and is the only API the target layer names.
   `#![deny(unsafe_code)]` holds everywhere else, so a raw pointer can only
   appear where the FFI contracts are written down — the bindings themselves
   and the type-erased cursor address in `ttd/interrupt.rs` that lets ^C abort
   a replay from another thread.

## Dynamics: the life of a `vCont;c`

The path a forward continue takes, end to end. Every subsystem appears here
exactly once, so this doubles as a guided tour.

1. **Wire**: gdbstub parses `vCont;c` and calls `MultiThreadResume` methods
   on `GdbTarget` (`gdb/target/base.rs`). The action is recorded in
   `ResumeState`; the trailing wildcard continue of a mixed `vCont` list is
   a deliberate no-op so it never downgrades a pending step (`resume.rs`).
2. **Dispatch**: `resume()` takes the pending `ReplayOp` and
   `spawn_replay`s it: a worker-thread job in `TargetRunner`
   (`gdb/target.rs::execute_op`). Per-thread ops set the backend's current
   thread first so stop replies and later queries observe it.
3. **Replay** (worker thread, holding the backend lock): `TtdProcess`
   positions the persistent cursor, installs the breakpoint-matching
   watchpoint callback, and calls `replay_forward_to` to the trace boundary
   (`target/process/execution.rs`). The callback matches hits against the
   logical breakpoint table (per-thread filters applied); speculative hits
   cannot fire because the limit *is* the boundary — there is no "future"
   to overreach into.
4. **Adopt**: where the cursor actually landed (TTD rounds positions) is
   read back — position *and* current thread — into the debug state
   (`adopt_cursor_position`). This keeps the lazy position caches honest;
   without it every later query would re-seek.
5. **Resolve**: the replay result becomes a `StopReason`
   (`classify_continue_stop`: breakpoint hit > trace boundary > exception
   enrichment, in that order). First-chance C++ exceptions are replayed past
   inside step 3, so a `throw` never reaches this decision.
6. **Map**: the worker result travels the result channel to the event loop,
   which maps `StopReason` → `MultiThreadStopReason` → `T05`/`T09`/… on the
   wire (`gdb/mapping.rs`). The worker publishes idle *before* sending, so
   the event loop can immediately service follow-up queries.

The event loop stays responsive through all of this because the replay runs
on the worker: it keeps polling the connection, which is how ^C reaches the
backend mid-replay (see below) and why fast text queries (`qRRCmd when`,
`monitor ttd …`) use a non-blocking `try_lock` with a protocol-specific
busy reply instead of parking.

## Dynamics: the life of a query

`g` (read all registers) is the archetype:

1. gdbstub asks `read_registers`; `GdbTarget` takes the backend with
   `TargetRunner::try_lock`. If a replay is in flight it answers `E`
   immediately instead of blocking: gdbstub processes packets that arrive
   while the target runs, and parking here would also stop ^C from being
   delivered. A handful of trait methods have no `TargetResult` (their only
   error path is fatal), so they answer with a neutral non-fatal value
   instead — an empty thread list, `target is running`, the cached
   text-segment address for `qOffsets`, or an ignored `R`.
2. `TtdProcess` narrows the wire tid to TTD's OS-tid space (`u32`); a tid
   that never appears in the trace's thread catalog answers `None` rather
   than serving TTD's zeroed context (RIP = 0) for a stale thread.
3. The persistent query cursor is repositioned **only if the debug position
   moved** (`with_cursor` → `at_position` → `PositionedCursor::prepare`),
   the read happens, and the `queries` counter ticks. Forty queries at one
   position cost zero extra seeks — this ratio is the health metric
   `monitor ttd stats` reports and `tests/target_threads.rs` pins.
4. Trace-global metadata (thread catalog, modules, exception events) is read
   once at open time into `TraceMetadata`; query and monitor paths use that
   snapshot instead of re-entering the engine for immutable data.
5. Thread-local memory (the OS stack and TEB) is readable through the same
   cursor once it is positioned at the stopped thread/position; the default
   memory policy returns those pages. Replays adopt the landing position
   before any query, so no snapshot/cache layer is needed.
6. `m` ignores the tid the client attaches to it and reads at the debug
   position. That is deliberate: the cursor's position is the only anchor the
   backend has, and re-anchoring per packet would make every read a seek
   (`SetPositionOnThread` moves the cursor, invalidating the position cache
   that step/continue depend on) while answering a question the recording
   cannot pose — "this address at the moment thread 2 stopped" has no common
   instant with the cursor's step. Registers (`g`/`p`) *are* per tid and
   exact; see [`ttd-internals.md` §7.2](ttd-internals.md) for what that means
   for a client reading another thread's stack.
7. `qXfer:libraries` reports each PE library at its runtime **`.text`**
   address (module base + section RVA), not the image base. GDB relocates the
   PE `.text` section to the reported segment address; the image base makes
   every DLL symbol 0x1000 low (the same misrelocation as `qOffsets`). The
   main executable is deliberately **not** in the list: GDB already has it
   from `qXfer:exec-file`, and a library entry would make it re-open the
   trace's recorded path instead of the client-supplied binary.
8. `qXfer:memory-map` is deliberately **not** advertised. GDB treats an
   advertised map as the complete set of readable regions; a module-only map
   makes it reject stack/TEB reads locally without ever sending `m`. The
   remote-read path is what makes C/C++ `locals` and `bt` work.

## The cursor discipline (the core invariant set)

TTD cursors are expensive (~150× a step to create vs. reuse), so
`TtdProcess` owns exactly two for its whole life, both lazily positioned
(`target/cursors.rs`):

- **Persistent cursor** — carries the client's watchpoints; serves every
  query.
- **Step cursor** — watchpoint-free, so a step is always exactly one
  instruction even across a breakpoint (GDB semantics); reused across
  steps, which is what makes range stepping (`next`/`stepi` over a line —
  Delve's hot path) usable.

Invariants that hold across every operation, and the tests that pin them:

| Invariant | Guard |
|---|---|
| Only `goto`/replay/step move a cursor, and all of them update the cache | `tests/target_threads.rs` (raw-cursor oracle, step-after-goto) |
| Failed/partial operations invalidate the cache (next access re-seeks) | `execution.rs` error paths, `PositionedCursor::invalidate` |
| Queries at one position never re-seek | `repeated_queries_do_not_reseek_the_cursor` (40 queries, 0 seeks) |
| Range steps never touch the query cursor | `range_step_reuses_step_cursor_without_query_seeks` |

## Dynamics: breakpoint sync

TTD has no breakpoints; every client breakpoint is a TTD memory watchpoint
(`target/breakpoint_state.rs`):

- A **logical table** (`BreakpointManager`, id-ordered for deterministic
  duplicate-address matching) holds what the client asked for; a **physical
  registry** (`WatchpointRegistry`, ordered + deduplicated) holds what is
  actually registered on the persistent cursor.
- A hit matches a logical breakpoint only when the access masks overlap, so
  an execution breakpoint and a data watchpoint at the same address remain
  distinguishable (the callback carries TTD's real `DataAccessMask`).
  Data watchpoints additionally match by byte-range intersection.
- Every add/remove diffs desired vs. actual and applies the delta
  incrementally. Registration is fallible: a rejected watchpoint rolls back
  the logical entry and answers `E16` rather than acknowledging a
  breakpoint that would never fire. Failed physical calls stay tracked and
  converge on the next sync.
- Removal is **address-keyed end to end**: RSP `z` packets identify
  breakpoints by address only, clients re-insert ids after restarts, and a
  new session has no memory of an old session's ids — so the backend itself
  clears every id at a triple, and the GDB layer keeps zero per-session
  breakpoint bookkeeping.
- OS tids become `UniqueThreadId`s through the immutable `ThreadTable`;
  the watchpoint callback receives OS tids back, so per-thread filters are
  applied in the matching callback, not in the TTD filter (which would
  silently degrade unknown tids to "any thread").

## Dynamics: ^C and the interrupt pipeline

Cancellation flows bottom-up with no manual wiring
(`DebugTarget::interrupt_handle` → `TargetRunner` → ^C path):

1. While a replay is in flight, the worker publishes its cursor as a
   type-erased handle (`AtomicUsize`); the handle is an `Arc<dyn Fn()>` the
   runner picks up once.
2. ^C (protocol interrupt or `vCtrlC`) fires the handle →
   `InterruptReplay` inside TTD → the replay returns `Interrupted`.
3. Either path surfaces as `T02` on the wire — the protocol interrupt and
   the backend-aborted twin report identically by design.

The store/clear pair around the replay makes the racing-handle story sound:
the slot is non-null exactly while the cursor is alive, and `Drop` clears
it before the cursors die. Session teardown fires the handle and waits
(bounded) so an abrupt disconnect never strands the backend in a detached
worker.

## Panics across the FFI boundary

User callbacks (watchpoint filter, index progress) run on C++ stacks, so an
unwinding panic would be UB. Every shim funnels through
`ttd/callback.rs::call_guarded`: the payload is stashed in a thread-local,
the shim returns a safe default (stop the replay), and the
`CallbackPanicGuard` held by the FFI wrapper re-throws with
`resume_unwind` once the C stack is gone. `TtdProcess::watchpoint_replay`
catches that re-thrown panic at its own boundary to clear the published
interrupt slot and invalidate the cursor cache before propagating, so a
callback panic cannot leave the target pointing at a dead replay or a stale
position. A panicking backend replay degrades to a SIGTRAP stop, never a
wedged event loop.

## Design decisions and their rejected alternatives

- **Two cursors + lazy positioning**, not one cursor or fresh cursors per
  call: measured 616 µs → 3.9 µs per step on the sample trace.
- **Trace metadata snapshot at open**, not per-query engine calls: the thread
  catalog, module list, and exception list are immutable for a loaded trace,
  and `monitor ttd`/exception enrichment otherwise re-read them repeatedly.
  The exception index is a `BTreeMap<TtdPosition, …>`, reusing the position
  ordering the target already depends on.
- **Logical/physical breakpoint split with convergent retry**, not direct
  pass-through: client ids and physical watchpoints have m:m relationships
  (dedup, restarts, retries) that one table cannot model honestly.
- **Worker-thread replay + channel**, not async or shared-state locks: the
  event loop must poll the connection for ^C, and a plain channel gives
  testable, backpressure-free handoff. The fork of gdbstub carries three
  small patches (UnknownPacket hook, 2-digit stop signals, kill-without-
  reply) — each also submitted upstream; no patch files live in this repo.
- **Command errors are replies, not logs**: `vRun`/`R` restart propagate a
  failed `goto` (and reject unknown checkpoints/malformed positions) instead
  of logging and acknowledging. A client that is told a restart succeeded
  but stayed at the old position will interpret every later stop wrong.
- **Kill-EOF / empty-reply declines / `use_rle = false`**: protocol
  behavior Delve's client parses literally; all pinned by wire tests
  (`tests/gdb_sessions.rs`, `tests/gdb_handshake.rs`).
- **Exception filtering is by code, not by mask class**: TTD files an access
  violation under `Hardware`, but a stack overflow and a MinGW C++ `throw`
  both under `Software`, so no mask bit selection matches a real debugger's
  behavior. All classes are admitted and the first-chance events a live
  debugger passes through (`0xE06D7363`, `0x20474343`) are replayed past by
  code; a single step reports whatever it lands on.
- **Checkpoints owned by the accept loop** (`serve_sessions`), not by the
  per-session `GdbTarget`: like position and breakpoints they are
  trace-global state, so `restart c1` keeps working after a reconnect.
- **Rejected: cargo workspace split.** Three crates would make the
  layering physical, but at this size (one binary, a few thousand lines of
  Rust) it buys only manifest churn and `pub`-surface inflation; the module
  system plus the private `ttd` facade already enforce the boundaries.
- **Rejected: replacing gdbstub with a hand-rolled RSP engine.** The fork
  patches are small and upstream-tracked; the protocol machinery
  (checksums, no-ack mode, packet buffering) is not where this project's
  value lives.
- **Rejected: `QListThreadsInStopReply` support.** gdbstub has no hook; a
  fourth fork patch would save one round-trip per stop. Not worth the fork
  drift.

## Testing strategy in one paragraph

Unit tests pin every pure decision (`classify_continue_stop`, watchpoint
sync against a fake cursor, checkpoint table, wire mappings, XML chunking,
command parsing, resume priority, panic escalation). The `gdb_*` suites
drive the real protocol over TCP against the mock backend and replay the
Delve handshake, including the fork-patch behaviors. The `target_*`, `ttd`,
and `gdb_ttd` suites need a recorded trace (`TTD_TRACE_PATH`) and are
honestly `#[ignore]`d without one, so CI stays green everywhere while the
real-trace invariants (cursor discipline, register layout, breakpoint
firing) stay pinned on any machine that has a trace. Above those sits
`tests/e2e/`: the same server driven by real clients over real traces, gated
on transcript diffs (see `tests/e2e/README.md`).
