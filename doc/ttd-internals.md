# TTD internals

This is the replay model behind `src/ttd/` and `src/target/`. The primary API
reference is `TTD/IReplayEngine.h` in `Microsoft.TimeTravelDebugging.Apis`.
Recording is handled by `ttd.exe`; this project only replays.

## Positions, cursors, and events

A `TtdPosition` mirrors TTD's `{ Sequence, Steps }`: an ordered location in the
recorded instruction stream, not a clock, address, or source line. Replay walks
the recording without re-executing the program. `SetPosition` may round to a
valid location, so the backend always reads back where the cursor landed.

A cursor owns its position, event masks, watchpoints, and query context.
`ReplayForward`/`ReplayBackward` stop at an event, position limit, or step
budget; `StepForward`/`StepBackward` advance one instruction.
`InterruptReplay` may be called from another thread. A cursor must not outlive
its engine, which the Rust wrapper enforces through shared engine ownership.

Important `EventType` results:

| Event | Meaning |
|---|---|
| `MemoryWatchpoint` | Watched read, write, or execute access |
| `Exception` | Recorded exception accepted by the exception policy |
| `Gap` | Unrecorded execution; replay skips it |
| `Thread` | Current thread begins or ends |
| `StepCount` / `Position` | Step budget or requested limit reached |
| `Process` | Recorded process boundary |
| `Interrupted` | Replay was cancelled |
| `Error` | Replay failed; never report this as a successful step |

`src/target/stop_reason.rs` is the single result-mapping point. Process and
thread termination are different: the frontend makes process exit rewindable
with `T09`, returning `W00` only after another forward resume.

## Exception policy

TTD's hardware/software/C++ mask classes do not match debugger semantics. For
example, fatal stack overflow and a MinGW C++ `throw` can share a class. The
target therefore enables all exception classes and filters by exception code:
continue passes first-chance MSVC `0xE06D7363` and GCC/MinGW `0x20474343`
events, while a single step reports what it lands on.

Replay results do not contain the exception code and address.
`TraceMetadata::exception_before` enriches the stop from the immutable event
catalog captured when the trace opened. `monitor ttd events` exposes that
catalog for diagnosis.

## Memory is recovered, not a snapshot

`QueryMemoryBuffer` reconstructs bytes from keyframes and recorded data lines.
The cursor uses `GloballyAggressive`; policy controls how hard the engine
searches, not whether a stale value becomes exact. Without a complete index,
old writes may read as zero.

A read has two important properties:

- Its runs may come from several trace positions. `read_memory_ranges` and
  `monitor ttd mem` expose their fragment-granular sequence numbers.
- The newest available data line can become older as the cursor moves away
  from the last write. An old sequence is not proof of a wrong value: memory
  that has not changed should keep its old value.

There is no single cross-thread “now”. The cursor identifies one step of one
thread, so another thread's registers are exact at its own last step, but its
memory cannot be reinterpreted as a snapshot at the stopped thread's instant.

| Query | Guarantee |
|---|---|
| Registers, PC/SP, positions, instruction order | Exact |
| Stopped thread's recently touched stack/locals | Usually exact |
| Memory written only by other threads long ago | May be older than the cursor |
| A multi-field struct as one instant | Not guaranteed |

For a specific value's history, use a write watchpoint: it stops at the exact
recorded write, and reverse stepping restores the previous value.

### Index lifecycle

`TtdProcess::open` ensures `<trace>.idx` exists before queries. Keyframes are
needed both for old-memory recovery and efficient seeking. An interrupted
build can leave a truncated `.idx` that still reports `Loaded`; TTD's leftover
`.idxt` is the reliable evidence. `ensure_index` treats it as invalid, removes
both derived files, and rebuilds. `monitor ttd info` reports the resulting
index state.

## Threads and breakpoints

GDB uses OS thread ids. `ThreadTable` maps them to trace-stable
`UniqueThreadId`s where needed. The TEB is exposed as `gs_base`, allowing Delve
to locate Go runtime state without writing to the trace. TTD returns zeroed
context for unknown tids; `thread_state` rejects them instead of presenting a
thread stopped at RIP 0.

The SDK has no breakpoints. The target maps `Z0`/`Z1` to execute watchpoints
and `Z2`–`Z4` to data watchpoints over the requested range. The callback can
reject a hit and continue, enabling per-thread filters and many logical
breakpoints on one physical watchpoint.

Callback constraints matter:

- Only the supplied thread view may be used; cursor queries are forbidden.
- A hit identifies range and access type, not PC, so matching includes access
  bits and byte-range intersection.
- Registration can fail for an unobserved address. The target rolls back the
  logical breakpoint and the frontend returns `E16`.
- Callback context must be heap-owned. A reusable stack slot can leave TTD
  calling through garbage, which its SEH may silently turn into “continue”.

Position watchpoints exist in the SDK but are unused: clients express
breakpoints by address, not trace position.

## API limits

Replay cannot write guest state, inject calls, attach to a live process, set
native breakpoints/catchpoints, or resolve symbols. Module information is
available, but PDB/DWARF lookup belongs to the client. Correct
`qXfer:exec-file`, `qOffsets`, and `qXfer:libraries` responses are therefore
part of source-level debugging.

## Binding boundaries

`src/ttd/cpp/ttd_cxx.hpp` and `.cpp` are the only layer that calls the SDK's
COM-shaped interfaces. `src/ttd/ffi.rs` exposes ordinary typed methods through
`cxx`; safe Rust wrappers live in `src/ttd/`.

- SDK layouts do not cross cxx. Records are shared value structs, memory is a
  slice, and amd64 context is a fixed little-endian word stream decoded in Rust.
- Rust selects the register layout from the trace architecture and converts
  SDK access types to masks.
- Watchpoint and index callbacks trap panics and rethrow after C++ returns.
- `InterruptSlot` holds the replaying cursor's address only for the published
  replay scope, allowing another thread to cancel it safely.

Unsafe code is confined to `src/ttd/`; the rest of the crate has
`#![deny(unsafe_code)]`.

## Diagnosing a disagreement

1. Two runs of the same trace differ: treat it as a server regression. The
   baseline diffs in [`regression/`](../regression/README.md) target this.
2. Replay differs from a fresh live run: account for scheduling first. One
   recording is one fixed execution, not a template for another live run.
3. Memory looks stale or zero: check `monitor ttd info` and `monitor ttd mem`
   before changing memory policy.
4. A thread has RIP 0: check whether its tid exists in the trace catalog.
5. A breakpoint never fires: check `monitor ttd stats` and whether physical
   watchpoint registration was rejected.
