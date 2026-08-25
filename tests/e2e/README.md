# Source-level e2e tests

These are end-to-end **tests**. They exercise the server through the real
clients (Delve for Go/cgo, GDB for C/C++) against recorded TTD traces.

One entry point runs everything versioned:

```bash
python tests/e2e/e2e.py <command>   # or via mise, see "Run" below
```

| Command | What it gates |
|---|---|
| `forward` | Replay vs `snapshots/live.snap` — the normalized transcript of one recorded live run. `--record` runs live + replay once, refuses to write if they disagree, and replaces the snapshot |
| `reverse` | Replay vs `snapshots/reverse.snap` — a recorded transcript of the SAME trace, so the diff is raw (addresses, positions, thread ids are **not** masked): any difference is a real regression |
| `build` / `record` | Compile the programs / record the traces (run from an elevated terminal) |

**Differential direction matters.** Forward compares two *different* worlds
(live process vs recorded trace), so its normalizer drops what legitimately
differs (hex addresses, pids/tids, positions). Reverse compares two runs of
the *same* trace, so program state is compared byte for byte. Both directions
normalize absolute file paths to `<PATH>/name`: they are properties of the
machine that recorded the trace, not of the program — but the basename stays
in the comparison, so a frame resolving to the wrong module is still a diff.

**The scripts dump state; they do not assert.** A script navigates and prints
(`p/x $pc`, `print x`, `info locals`, `bt`) at every interesting stop. The
full transcript is the gate, so a regression shows up as a diff line instead
of being swallowed by a hand-written check. Anything not printed cannot be
compared.

**A forward script may only observe deterministic state.** The live side runs
on the real scheduler, so anything that depends on it (which worker took
which job, a shared counter's intermediate value, uninitialized stack slots)
must not be printed. Pin stops with conditions on values that identify a
single event. Reverse scripts are not bound by this — the trace fixes the
schedule — which is why they can watch shared counters.

Two client-grammar notes for `.dlv` scripts: reverse execution is `rev
<cmd>` (`rev continue`, `rev step-instruction`, …), and `clear` takes a
breakpoint id or name, not a `file:line` location. Prefer `clear <id>` over
`clearall`, whose output order follows Go map iteration.

## Layout

```text
e2e.py                the only entry point (subcommands above)
harness/              shared machinery, one definition per concern:
  common.py             paths, Case, case discovery/selection, child env
  clients.py            Server (ttd-gdbserver), run_gdb, run_delve
  normalize.py          transcript normalization + script-error detection
  runner.py             parallel per-case runner, uniform PASS/FAIL/SKIP
commands/             one module per subcommand (add_arguments + main)
programs/<lang>/<name>/   one directory per program:
  <source>              Go/C/C++ source
  build.toml            C/C++ compiler flags (optional)
  forward.dlv|gdb       this program's forward steps
  reverse.dlv|gdb       this program's reverse steps
  snapshots/live.snap     cached normalized live run (forward oracle)
  snapshots/reverse.snap  cached normalized reverse run (deterministic gate)
traces/               local .run/.idx/.out artifacts (ignored)
out/<case>/<gate>/    regenerated output, grouped per case per gate (ignored)
```

Everything under `out/` is regenerated output space, grouped `out/<case>/<gate>/` so
investigating one failing program means opening one directory. Suffixes are
the same everywhere: `.out` is the client transcript, `.stdout`/`.stderr`
the program's redirected streams, `.gdb`/`.dlv` the generated scripts.

**The debug steps live with the program, not in the harness.** The `forward`
and `reverse` commands discover directories that contain `forward.dlv` /
`forward.gdb` and contain no program names, breakpoints, or per-program
branches. The directory name determines the case name
(`programs/c/threads` → `c-threads`), the trace (`<case>.run`), and the
executable (`<directory>.exe`).

Delve scripts are passed to `dlv --init` verbatim. GDB scripts are run with
placeholders substituted: `@FILE@` (the executable), `@RUN@` (forward: live
`run` vs replay `target remote` + `continue`), `@TARGET@` (reverse: replay
`target remote`).

## The corpus: seven programs, each a feature combination

| Program | Client | What the scenario covers |
|---|---|---|
| `c-core` | GDB | A call chain and a loop with a global: source/instruction/range stepping, step-in/step-out, locals, stack memory, write/read/access watchpoints, then the recorded process exit. The reverse script adds `gs_base`, `info all-registers`, a hardware breakpoint, a rejected watchpoint (`E16`), the `qXfer`/`qRRCmd`/`qGetTIBAddr` annexes, `monitor ttd *`, and the wire-level `T09` → `W00` end sequence |
| `c-threads` | GDB | A per-thread breakpoint pinned by its argument, `finish` out of a worker; on rewind, thread enumeration and selection, and a shared address written by whichever worker holds the mutex (cross-thread watchpoint, retraced backwards) |
| `c-fault` | GDB | A hardware access violation: exception stop with code, faulting instruction, both frames, locals, and rewind out of the fault |
| `c-stack-overflow` | GDB | A deep recursion: frame navigation and single stepping at depth. The reverse script asserts the exhaustion itself — a second exception code (`0xC00000FD`) at a faulting stack probe after ~500 frames |
| `c-cpp-exceptions` | GDB (C++) | A plain continue passes a `throw` (first-chance events are filtered by code, not by mask class), while `catch throw`/`catch catch` still report both, in both directions |
| `go-core` | Delve | Goroutines and frame selection, deep recursion (goroutine stack copy), a line breakpoint, the nil dereference that ends the trace; on rewind, `qRRCmd when`, checkpoint create/info/delete, `vRun` restart by checkpoint and to the trace start, and reverse continue/step-instruction |
| `go-cgocrash` | Delve | Go → C → Go callback with mixed Go/C frames, then a fault in the C frames; on rewind, the full register file (including `Gs_base`) and a checkpoint round trip across the Go/C boundary |

Why this set: `c-core` covers the widest path (stepping, locals, stack
memory, watchpoints, the exit), `c-fault` adds the exception paths, and
`c-threads`/`c-stack-overflow` are kept as *scenarios* rather than extra
coverage — a mutex handoff between threads and ~500 frames of stack
exhaustion are failure modes not otherwise reachable in one trace.

`c-cpp-exceptions` exists for a *functional* property: a first-chance C++
exception must not stop a plain continue, while `catch throw`/`catch catch`
must still report it. The filter is by exception *code* (`0x20474343` on
MinGW, `0xE06D7363` on MSVC), and only a C++ trace contains such an event —
`monitor ttd events` shows it — so no C or Go program can stand in for it.

## Prerequisites

- `cargo build` (the tests start `target/debug/ttd-gdbserver.exe`)
- `dlv` (the patched `ttd`-capable Delve) on `PATH`, or `TTD_DLV`
- TTD runtime DLL directory on `PATH`, or `TTD_DIR`
- Delve directory on `PATH`, or `DELVE_DIR`
- `gdb`, `go`, `gcc`, `g++`

The traces and executables are deliberately **not committed** (large and
machine-specific). They live in `tests/e2e/traces/` and `programs/**`.

## Build and record

`mise.toml` has only tasks. Machine paths live in the untracked
`mise.local.toml`; copy `mise.local.toml.example` and fill in `TTD_DLV`,
`DELVE_DIR`, `TTD_DIR`, `TTD_EXE` (and `_.path`).

```bash
mise run build       # cached: skipped while the exes are newer than sources
# from an elevated terminal, when traces are missing/changed:
mise run record
mise run snapshots   # refresh both snapshot kinds (cached)
```

Recording needs an elevated token: run `mise run record` from an elevated
terminal.

## Run

```bash
# every program, both clients, both snapshot gates
mise run test              # = test:forward + test:reverse

# lower-level, when debugging one case
python tests/e2e/e2e.py forward --only go-core
python tests/e2e/e2e.py forward --record --only go-core
python tests/e2e/e2e.py forward --client delve
python tests/e2e/e2e.py reverse --client gdb --only c-core go-core
```

Environment overrides: `TTD_DLV`, `TTD_DIR`, `DELVE_DIR`.

Each program directory documents what it covers in its script comments; the
per-case output of a run lands in `out/<case>/<gate>/`.
