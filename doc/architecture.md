# Architecture

```text
GDB / LLDB / Delve
        │ GDB RSP over TCP
        ▼
src/gdb/       protocol frontend; GdbTarget<T: DebugTarget>
        │ DebugTarget: the only protocol/backend boundary
        ▼
src/target/    TtdProcess: replay, cursors, threads, breakpoints
        │ safe ReplayEngine / ReplayCursor wrappers
        ▼
src/ttd/       SDK ownership, values, callbacks, interrupts
        │ cxx
        ▼
src/ttd/cpp/   TTDReplay.dll
```

`GdbTarget` knows no TTD specifics and is tested with a mock `DebugTarget`.
The trait deliberately uses replay concepts such as `TtdPosition`, reverse
execution, and `StopReason::TraceEnd`; it is not a generic debugger
abstraction. Only `src/ttd/` permits `unsafe` code.

## Continue and stop replies

A forward `vCont;c` flows through these stages:

1. gdbstub parses the action into `ResumeState`. Mixed `vCont` actions retain
   their priority; a trailing wildcard must not downgrade a pending step.
2. `GdbTarget::execute_op` starts a worker through `TargetRunner`. Per-thread
   operations select the backend thread before resuming.
3. `TtdProcess` positions the persistent cursor, installs its watchpoint
   callback, and replays to the trace boundary.
4. The backend reads back the actual landing position and thread because TTD
   may round a requested position.
5. `classify_continue_stop` resolves breakpoint, boundary, and exception
   results. The frontend maps the resulting `StopReason` to an RSP stop reply.

The worker publishes the backend as idle before returning its result, so
follow-up queries can run immediately. Position, breakpoints, and checkpoints
belong to the trace, not a connection, and persist across reconnects with
`--accept-multiclient`.

## Queries and scheduling

Read-only packets take `TargetRunner::try_lock`; they never wait for a running
replay. Handlers with a nonfatal error path reply `E`; fatal-only methods use a
neutral response such as an empty thread list or cached `qOffsets` data. This
keeps the event loop polling the connection so ^C remains deliverable.

The persistent cursor is repositioned only when the debug position changes.
Repeated register, memory, and monitor queries at one position therefore do
not repeatedly seek. Immutable threads, modules, and exceptions are snapshotted
into `TraceMetadata` when the trace opens.

Registers are queried for a specific recorded thread. Memory reads instead use
the current debug position: selecting another thread's tid does not create a
cross-thread instant that the recording contains. Library XML reports each
PE's runtime `.text` address; the main executable is omitted because the
client already received it through `qXfer:exec-file`.

## Cursor discipline

`TtdProcess` owns two cursors for its lifetime:

- The **persistent cursor** carries client watchpoints and serves queries and
  filtered continue operations.
- The **step cursor** has no watchpoints, so stepping advances exactly one
  instruction even on a breakpoint. Reusing it makes range stepping practical.

Every navigation updates the remembered position; failed or partial operations
invalidate it. `step_range` stays on the step cursor and reads its PC directly,
without seeking the query cursor after every instruction.

A cursor owns a shared engine handle, so it cannot outlive the engine. Both
cursors are created during `TtdProcess::open`, which keeps the engine alive
without a separate engine field in the process.

## Breakpoints and watchpoints

TTD has no breakpoint API. `BreakpointState` separates the logical breakpoints
requested by clients from the deduplicated physical watchpoints registered on
the persistent cursor.

- Adds and removals diff desired against registered watchpoints and retry
  failed physical operations at the next sync.
- Registration is fallible. A rejected watchpoint rolls back its logical entry
  and produces RSP `E16` instead of acknowledging a breakpoint that cannot fire.
- Removal is address-keyed end to end, matching RSP and surviving reconnects.
- Hits match address, access mask, byte range, and thread filter. When several
  logical breakpoints match, the per-thread one wins, then the lowest id.

The step cursor never carries these watchpoints, which prevents a step from
re-triggering a breakpoint at its current PC.

## Interrupts and FFI callbacks

During a blocking replay, `InterruptSlot` publishes the active cursor address.
^C or `vCtrlC` invokes the backend interrupt handle, calls TTD's
`InterruptReplay`, and produces `T02`. The slot is cleared on every exit path.
Session teardown interrupts and joins the worker with a bounded wait.

A range step consists of short SDK calls, so there is no long replay to poke.
Its loop instead polls an interrupt epoch. `DebugTarget::begin_resume` resets
that epoch before every operation, preventing an old ^C from cancelling the
next resume.

Rust callbacks never unwind into C++. The shim stores a panic, returns a safe
default, and rethrows after the C++ call returns. The target then clears the
interrupt slot and invalidates cursor state before propagating the panic.

## Tests

Pure mapping and state machines have unit tests. `tests/gdb_*.rs` drives the
real RSP frontend over TCP with a mock backend; `tests/target_*.rs` and
`tests/gdb_ttd.rs` exercise a real recording when `TTD_TRACE_PATH` is set.
Without a recording, trace-backed tests report as ignored.

[`regression/`](../regression/README.md) adds source-level tests using real
Delve/GDB clients. Forward and reverse transcripts are compared directly with
local baselines captured from trusted code on the same trace.
