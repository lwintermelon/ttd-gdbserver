# TTD internals, and how this project uses them

What the WinDbg Time Travel Debugging replay API provides, what it refuses to
provide, and how `ttd-gdbserver` turns it into a `DebugTarget`. Read this
before changing `src/ttd/` or `src/target/`; the other docs assume it.

Primary source: `TTD/IReplayEngine.h` from the
`Microsoft.TimeTravelDebugging.Apis` package (release 0.9.5). Quoted
signatures are from that header; behavioural notes marked *observed* were
measured against recordings.

## 1. What a trace contains

A recording (`ttd.exe myapp.exe` → `myapp.run`, plus `myapp.out` for the
program's debug output) holds:

- **The timeline** — every CPU instruction the recorded process executed, in
  order. This is the ground truth: replaying does not re-execute the program,
  it walks this list.
- **Memory** — an approximation of the process's memory, sampled at keyframes
  and recorded as deltas. It is *not* a full snapshot, which is why reads can
  be stale, short, or missing (§7).
- **Events** — exceptions, debug output, module load/unload, thread
  create/terminate. Each is attached to a timeline instruction.

Recording is `ttd.exe`'s job. This project only replays.

## 2. Position: the unit of time travel

```cpp
struct Position { SequenceId Sequence; StepCount Steps; };
```

A 128-bit, monotonically increasing, comparable value that identifies **one
executed instruction**. Our `TtdPosition` mirrors it and implements `Ord`.

Three consequences that shape the whole codebase:

1. A position is not an address and not a timestamp. Two threads at the same
   source line hold different positions; the same address appears at many
   positions.
2. Returning to a position re-executes nothing. Every query is a lookup at
   that position.
3. `SetPosition` may land elsewhere: TTD rounds to the nearest valid position.
   After a seek or replay, read the landing position back
   (`PositionedCursor::prepare`) instead of trusting the requested one.

## 3. Cursors

```cpp
ICursor* IReplayEngine::NewCursor();
class ICursor : public ICursorView { ... };
```

A cursor is the stateful handle: current position, event/exception masks,
watchpoints, and the concurrency context for queries. `ICursorView` is the
query surface; `ICursor` adds the owning/destructive operations.

**Ownership rule (SDK):** once the engine is destroyed, the only valid
operation on a cursor is `Destroy()`. `ttd`'s `ReplayCursor` owns its engine
handle, so a cursor keeps the engine alive by construction (see `AGENTS.md`,
"Engine ownership").

Navigation and execution:

| Call | Meaning |
|---|---|
| `SetPosition(Position)` | seek to a recorded position (rounded) |
| `ReplayForward(limit, steps)` / `ReplayBackward` | run until an event, the limit position, or the step budget |
| `StepForward()` / `StepBackward()` | exactly one instruction |
| `InterruptReplay()` | *callable from any thread*; stops an in-flight replay as soon as possible |

Replay stops for exactly one `EventType` and returns a `ReplayResult` whose
`StopReason` field says which.

## 4. Why replay stopped: `EventType`

| Event | Meaning | Maskable? |
|---|---|---|
| `MemoryWatchpoint` | a watched memory access (read/write/execute) happened | `EventMask` |
| `PositionWatchpoint` | execution entered a watched position range | `EventMask` |
| `Exception` | an exception matching `ExceptionMask` occurred | `EventMask` |
| `Gap` | execution that was **not recorded**; replay skips over it | `EventMask` |
| `Thread` | beginning or end of the *current thread* (direction-dependent) | `EventMask` |
| `StepCount` | step budget exhausted, no other event | never |
| `Position` | the limit position was reached, no other event | never |
| `Process` | beginning or end of the process (direction-dependent) | never |
| `Interrupted` | `InterruptReplay()` was called | never |
| `Error` | the replay engine failed | never |

`StopReason::try_from(&TtdReplayResult)` (`src/target/stop_reason.rs`) is the
single mapping point. Two rules matter:

- `Process` and `Thread` are **not** the same stop. `Process` is a real exit;
  `Thread` may just be one thread ending. The GDB layer builds rr's two-stage
  end sequence on that distinction (`ProcessExit` → `T09`, then `W00` on a
  further forward resume).
- `Error` and unknown values are errors, never a catch-all `StepComplete`:
  reporting success from a broken replay would make a client silently resume.

`Interrupted` is what a client's ^C produces. It has two sources that must
agree: the event loop's `on_interrupt` and this event from the backend.

## 5. Exception masks

```cpp
enum class ExceptionMask { Hardware, Software, CPlusPlus, DebugPrint, All };
```

Our policy (`src/target/process/execution.rs`):

- `EventMask = MemoryWatchpoint | Exception`
- `ExceptionMask = All`, with first-chance codes filtered afterwards

The mask alone cannot express what a debugger does, because TTD's
classification does not line up with it: an access violation is `Hardware`,
but a stack overflow and a MinGW C++ `throw` are both filed under `Software`.
Masking `Hardware` only would replay a fatal stack overflow to the trace end
unnoticed; masking `Hardware | Software` would stop on every `throw`. Every
class is therefore admitted, and a continue replays past the events a live
debugger treats as first-chance: the MSVC (`0xE06D7363`) and GCC/MinGW
(`0x20474343`, " GCC" reversed) C++ exception codes, both in TTD's
`CPlusPlus` group. A single step reports whatever it lands on; a client that
wants to see throws sets catchpoints, which resolve to ordinary breakpoints
on the runtime throw/catch functions (`__cxa_throw`, `__cxa_begin_catch`).

When a C++ exception is surfaced anyway, `signal_for_exception` maps both
codes to `SIGABRT`. `monitor ttd events` shows which one a trace contains.

The replay result does not carry the exception's code or address. The trace
records them as events, so `enrich_exception` looks up the last recorded
exception at or before the stop position (`TraceMetadata::exception_before`).

## 6. Gaps, and "the trace is the truth"

`Gap` means the recorder did not capture that stretch of execution; replay
jumps over it. The cursor is still at a valid position, so the frontend sees
an ordinary step completion.

The broader rule is more important than the event: **the trace is one
recorded execution, frozen**. Anything that depends on scheduling — which
worker picked up which job, a map's contents, an intermediate shared counter
— is fixed by the recording. A replay that disagrees with a *fresh live run*
is not necessarily wrong; a replay that disagrees with *another replay of the
same trace* always is. `tests/e2e` encodes exactly this split: the forward
gate diffs against a recorded live snapshot, the reverse gate diffs with no
masking at all.

## 7. Memory reads: policy and index

```cpp
MemoryBuffer QueryMemoryBuffer(GuestAddress, BufferView,
                               QueryMemoryPolicy = Default);
void SetDefaultMemoryPolicy(QueryMemoryPolicy);
```

| Policy | Behaviour |
|---|---|
| `Default` | engine picks; may only surface memory observed by the current thread |
| `ThreadLocal` | only what is cheap to find during thread-local execution |
| `GloballyConservative` | answers from the newest high-confidence snapshot it already has |
| `GloballyAggressive` | walks the index for the value at the queried position |
| `InFragmentAggressive` | looks into the current fragment, including the future |

Cursors are created with **`GloballyAggressive`** (`csrc/ttd_sys.cpp`).
`Default` is wrong for a debugger: it may hide writes observed only by other
threads. `GloballyConservative` is wrong in the other direction: it returned a
heap cell's image from a position *before* any of its writes (`&main.hits` at
`AF7:7A` read as 0 from the `0xA0B` range, while the 12 writes landed at
`0xA57`–`0xAE1`). A scan of every position of two traces found no range newer
than the queried landing position, so aggressive does not read the future
either.

The policy changes *how hard the engine looks*, not whether its answer is
right. Measured on one trace, one address, ten positions, all five policies
per position: the four global policies returned byte-identical results
everywhere, including where the value was stale, and only
`GloballyConservative` ever differed (one position where it served an older
data line). `ThreadLocal` is the one policy that answers "no data" (`size=0`)
rather than reconstructing — which is why it is what callbacks get.

### 7.1 A read is not a snapshot

`MemoryBuffer` is documented as "possibly collected from **multiple
positions** in the trace"; `QueryMemoryBufferWithRanges` returns the runs,
each with the `Sequence` of the position it came from. A 192-byte read of
`runtime.g` was assembled from four runs at three sequences (1964, 1982,
1983). Because a fragment is TTD's atomic unit, the sequence is
fragment-granular: it carries no `Steps`.

The newest recoverable data line for an address also ages as the query moves
past the last write that produced one. On the same trace, `m.curg`:

| queried position | data line used | value |
|---|---|---|
| 2700 | 2693 | correct (the write at 2693) |
| 2800 … 4200 | 2676 | older |
| 4400, 4425 | 1982 | oldest |

So "how old is the newest data line" is a property of position, not of the
address being "local" or "global". Registers and PC never go through this
path (they come from the execution stream and are exact); memory does.

Two consequences worth internalising:

- an old `Sequence` is **not** evidence of a wrong value — a location nothing
  wrote since is correctly served from an old data line (a 3000-frame
  recursion read every `pad[n%2048]` correctly, including a byte written
  millions of instructions earlier);
- a buffer assembled from several positions is **not** an anomaly — it is the
  documented behaviour. It does mean a struct read is not a single-instant
  snapshot, which a client must not assume.

`DebugTarget::read_memory_ranges` and `monitor ttd mem <addr> [len]` surface
the runs and their sequences, so a suspicious read can be triaged instead of
guessed about.

**Writes are not what refreshes a query.** A watchpoint sweep over one address
with `Write|Overwrite|DataMismatch|NewData|RedundantData` showed 6 writes and
4 received data blocks on disjoint positions, and the receiving blocks came
from four different threads. The replay *model* tracks the writes (every one
had a matching `Overwrite` carrying the old value); the *index* only serves the
data lines it holds.

### 7.2 What a client should assume

A live debugger stops every thread, so "read this address now" has one answer
for the whole process. A trace has no such instant: the cursor is on one step
of one thread, and every query is answered relative to that step. That is not
a limitation to work around — it is what the recording contains.

| Query | Accuracy | Why |
|---|---|---|
| registers, PC/SP | **exact** | execution stream, never a memory query |
| instructions / breakpoints / positions | **exact** | simulator executes the recorded stream |
| order between sequence events | **exact** | `SequenceId` ordering is faithful |
| order *inside* a fragment | approximated | fragments are atomic by definition |
| the stopped thread's stack and locals | **usually exact** | it wrote them, so a data line covers the position |
| an address only other threads wrote, long ago | **may be older than the position** | nothing refreshed the index near the cursor |
| memory as a whole-struct snapshot | **not guaranteed** | one read may span positions (§7.1) |

Two practical rules follow:

- Reading thread 2's registers after `thread 2` is exact; reading its *stack*
  is exact only where thread 2 itself wrote it recently. There is no "same
  moment as thread 1" to appeal to.
- A write watchpoint is the reliable way to get a specific value: it stops at
  the instruction that performed the write (execution stream, exact), and
  stepping back restores the previous value — see
  `tests/target_watchpoint_hits.rs`. This is also why watchpoints are a strong
  point of this backend rather than a compromise: no debug-register limit,
  both directions, and real read watchpoints.

The **index** (`IReplayEngineView::BuildIndex`) writes `<trace>.idx`, a
keyframe table. It is required for accurate recovery of old writes — without
it, memory written long before the current position reads back as zero (the
Go runtime's `allgs` array was the first symptom). Keyframes are also what
makes seeking cheap: reaching an arbitrary position means replaying forward
from the nearest keyframe, so `SetPosition` is far from free — which is why
`PositionedCursor` avoids it whenever the position has not moved.

**A half-built index is worse than no index, and reports itself as healthy.**
The SDK has no incremental build, and a build killed part-way leaves a
truncated `.idx` that `GetIndexStatus` can still answer as `Loaded`; every
memory query then comes from a partial keyframe table. An index interrupted
this way produced a replay reading `main.hits == 0` where the trace recorded
54, and goroutine listings resolving to the wrong thread (`:0 ???` in Delve).
TTD writes the index under construction to `<trace>.idxt` and deletes it only
on completion, so `ensure_index` (`target/process.rs`) treats a leftover
`.idxt` as proof
that the sibling `.idx` cannot be trusted: it deletes both derived files and
rebuilds. Otherwise a `NotPresent`/`Unloadable`/`Unknown` status triggers the
same rebuild, and `Loaded` is reused as-is.

`monitor ttd info` reports the state as `index:`, so a suspicious memory read
can be triaged before anything else; the value is the status measured at open
time, i.e. after `ensure_index` ran.

## 8. Threads

- `UniqueThreadId` — stable for the trace's lifetime.
- OS `ThreadId` — may be reused by the OS.
- `GetThreadList()` — threads active at the cursor's position.
- `GetThreadInfo()`, `GetProgramCounter/StackPointer/FramePointer(ThreadId)`,
  `GetCrossPlatformContext(ThreadId)`, `GetAvxExtendedContext(ThreadId)`.
- `GetTebAddress(ThreadId)` — the Windows TEB.

Mapping in this project: the OS tid is what GDB speaks, so `ThreadTable` maps
OS tid → `UniqueThreadId` for watchpoint thread filters. The TEB is exposed as
the `gs_base` register so Delve can find the goroutine `g` without writing
memory (TTD is read-only, so Delve's usual injected-`MOV` trick cannot work).

**Unknown threads are not zero-state.** TTD returns a zeroed context for a
thread it has no state for; serving that would make a stale tid look like a
thread stopped at address 0, so `thread_state` returns `None` instead.

## 9. There are no breakpoints

The SDK has no breakpoint API. This project implements them as **memory
watchpoints**:

| GDB packet | Watchpoint registered |
|---|---|
| `Z0`/`Z1` (software/hardware execute) | execute access at the instruction address |
| `Z2`/`Z3`/`Z4` (write/read/access) | read and/or write access over the byte range |

```cpp
bool AddMemoryWatchpoint(MemoryWatchpointData const&);   // false = cannot watch
void SetMemoryWatchpointCallback(MemoryWatchpointCallback*, uintptr_t ctx);
typedef bool MemoryWatchpointCallback(uintptr_t, MemoryWatchpointResult const&,
                                      IThreadView const*);
```

The callback **accepts or rejects** a hit: returning false makes replay
continue. That is the mechanism behind per-thread breakpoints, deduplication
of logical breakpoints onto one physical watchpoint, and GDB's "a step must
not re-trigger the breakpoint at the PC" rule (the step cursor carries no
watchpoints).

Two constraints from the header:

- Inside the callback only `IThreadView` may be used; `ICursorView` is
  forbidden. Our callback touches only an atomic breakpoint table.
- A hit identifies the **watched range and access type**, not the PC. The
  matcher therefore treats `(address, size, access)` as the identity and
  requires the access bits to intersect, so an execute watchpoint never
  swallows a data hit at the same address.
- `AddMemoryWatchpoint` can fail (an address the trace never observed). That
  is why breakpoint registration returns `Result` and the RSP layer answers
  `E16` instead of acknowledging a breakpoint that would never fire.

`AddPositionWatchpoint` exists but is not used: positions are an internal
navigation concept, while clients express breakpoints by address.

## 10. What the replay API cannot do

- Write guest memory or registers.
- Set breakpoints (see §9) or catchpoints.
- Call into the process (no call injection, no expression evaluation).
- Attach to a live process — replay is the only mode.
- Resolve symbols: it exposes modules (`ModuleInstance`) only, not PDB/DWARF.
  Symbol lookup is the client's job; that is why `qXfer:exec-file`,
  `qOffsets`, and `qXfer:libraries` have to be right.

## 11. Mapping: TTD concept → this project → GDB RSP

| TTD | This project | On the wire |
|---|---|---|
| `Position` | `TtdPosition` | `qRRCmd when`, `vRun;;<pos>` |
| `EventType` | `StopReason` | `T` stop replies, `W00` |
| `MemoryWatchpoint` | `Breakpoint` / `Watchpoint` | `Z`/`z`; a hit is a `T05` with `watch:`/`rwatch:`/`awatch:` |
| `ExceptionMask` | policy in `process/execution.rs` | a `T` stop whose signal comes from the exception code |
| `InterruptReplay` | `DebugTarget::interrupt_handle` | `vCtrlC`/^C → `T02` |
| `BuildIndex` | `TtdProcess::open` | — |
| `GetModuleList` | `ModuleInfo` (+ `main_module`) | `qXfer:libraries`, `qOffsets`, auxv |
| `GetTebAddress` | `gs_base` register | `qGetTIBAddr`, `g` |
| `GetThreadList` | `active_thread_ids` | `qfThreadInfo` |
| `GetThreadContext` | `thread_state` | `g`/`p` |

Engine and cursor lifecycle in this project:

- One `ReplayEngine` per trace, opened once in `TtdProcess::open` (load trace
  → build index → snapshot immutable metadata); the two cursors created there
  keep it alive, so it is dropped when `open` returns.
- **Two cursors per process, for its whole lifetime.** The *persistent*
  cursor carries the client's watchpoints and serves every query; the *step*
  cursor carries none, so a step advances exactly one instruction even across
  a breakpoint. Reusing them (rather than creating one per operation) is what
  makes range stepping usable — see `AGENTS.md`.
- Only `goto`, a replay and a step move a cursor; each updates the
  remembered position, and any failed/partial operation invalidates it.

## 12. Debugging a disagreement

When a replay looks wrong, decide which layer is lying before changing code:

1. **Two replays of the same trace differ** → a server bug (determinism is
   guaranteed by the trace). `tests/e2e/e2e.py reverse` targets this.
2. **Replay differs from a fresh live run** → check trace-scheduling first
   (§6). Re-record the live snapshot only if the difference is legitimate.
3. **A memory read looks stale or zero** → check `monitor ttd info`'s
   `index:` line; anything but `loaded` means `ensure_index` rebuilt from a
   trace whose index was missing or partial, and a stale `.idxt` is deleted
   before the rebuild (§7). If the index is `loaded`, confirm the policy is
   `GloballyAggressive`.
4. **A thread reports RIP=0** → the tid is not in the trace's thread table;
   that is `thread_state`'s `None`, not a real context (§8).
5. **A breakpoint never fires** → TTD may have rejected the watchpoint
   (unobserved address). `monitor ttd stats` shows the physical
   add/remove counters.

## 13. The bindings this project uses

`csrc/ttd_sys.hpp` is a thin C++ surface over `TTD::Replay`: two classes
(`Engine`, `Cursor`) that hold the SDK's own `UniqueReplayEngine`/`UniqueCursor`
and forward one call each, plus free functions for the operations whose
by-value result a member function cannot return to Rust (see below). bindgen
reads that header, so every struct and enum it mentions is the SDK's own type
with its real layout; `src/ttd_sys/` is the generated half and `src/ttd/` is
the safe wrapper.

Surface this project needs beyond plain replay:

| Need | `ttd` wrapper |
|---|---|
| memory watchpoint callback (breakpoints, §9) | `install_borrowed_callback`; panics are parked and re-thrown after the replay call |
| `InterruptReplay` (^C) | `interrupt_replay(&self)`, callable from another thread |
| memory policy | `Cursor`'s constructor sets `GloballyAggressive` |
| `QueryMemoryBufferWithRanges` (§7.1) | `read_memory_ranges` |
| index state (§7) | `index_state`, `build_index_with_progress` |
| per-thread context | `amd64_context(tid)`, `teb_address_of(tid)`, `active_thread(idx)` |
| event/exception masks | `set_event_mask` / `set_exception_mask` |

Three constraints shaped that shim:

- **SDK interfaces are not exposed.** TTD's API is COM-shaped (pure virtual
  classes), and bindgen renders those as an opaque vtable rather than callable
  methods, so nothing in Rust can call them directly. The classes are the
  thinnest layer that can.
- **Large by-value results are free functions.** MSVC passes a member
  function's hidden return buffer in RCX after `this`; Rust's `extern "C"`
  expects it in RCX first. A member function returning a large struct is
  therefore uncallable from Rust (it crashes on entry, `this` being garbage),
  while the same function declared free works. That is why
  `ReplayForward`/`ReplayBackward`, the memory queries and the register-context
  getters are namespace-level functions.
- **Interpretation stays in Rust.** The shim hands back the SDK's
  architecture-agnostic `RegisterContext`; the wrapper picks the amd64 layout
  from the trace's `ProcessorArchitecture`, and turns the SDK's
  `DataAccessType` enum into an access mask.

`ReplayCursor` owns an `Arc` of the engine handle, so there is no lifetime
parameter and a cursor cannot outlive the object it drives. The one thing this
crate still does itself is the cross-thread handoff: `InterruptSlot`
(`src/ttd/interrupt.rs`) stores the cursor's address in an atomic for the
duration of a replay, because the interrupting thread has no borrow of the
replaying thread's cursor.

The watchpoint callback's context has to be a **heap** slot
(`WatchpointSlot`). A stack slot looks fine — the address is passed and used
synchronously, in the same call — but the compiler is free to reuse that stack
memory once the install call returns, and the replay then calls through
garbage. TTD catches the resulting access violation in its own SEH, so the
symptom is not a crash: every hit silently reads as "keep going" and the
continue runs to the trace end.
