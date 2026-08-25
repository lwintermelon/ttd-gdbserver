# Transcript regression tests

Real Delve (Go/cgo) and GDB (C/C++) clients run each case's `forward` and
`reverse` scripts against the same TTD recording. The printed program state is
the assertion. Tests compare it directly with a local baseline and print a
unified diff; only known client/server startup lines are excluded. Addresses,
positions, thread ids, and paths are not rewritten.

Baselines belong to one machine and one recording, so `forward.base` and
`reverse.base` are ignored. Generate them from trusted code before a change,
then use the same traces for verification. Never regenerate a baseline merely
to make a regression pass.

## Run

```powershell
cargo build
mise run build
mise run baseline            # trusted code, before making changes
mise run test                # forward and reverse, all cases

mise run baseline -- go-core
mise run test -- c-core c-threads
```

A missing baseline fails with instructions; tests never create one implicitly.
GDB writes its transcript itself, through `set logging`; Delve has no option for
its own output (`--redirect` targets the inferior), so its stdout is attached to
a file. Both land in `ttd-regression/` under the system temp directory, nothing
is written into the repository, and a failing case reports the transcript path
next to the diff.

The runner uses `target/debug/ttd-gdbserver.exe`, `gdb`, the TTD-enabled `dlv`,
and the TTD runtime. Point `DELVE_PATH` at that `dlv.exe`; its folder then stays
out of `PATH`, so a stock `dlv` on `PATH` cannot be picked by mistake. `TTD_DIR`
is a folder — the runtime DLLs are loaded by name — and belongs on `PATH`. See
`mise.local.toml.example`.

## Record

Two separate tasks:

```powershell
mise run clean      # delete traces, indexes and TTD .out files
mise run record     # record all seven programs
```

`record` needs administrator privileges; elevate it however you prefer: `sudo`,
`runas`, or an elevated terminal. One elevation covers all seven recordings.
It does not delete anything, so a failed elevation leaves existing traces alone.

Run `clean` before `record` when a trace must be re-recorded. A stale `.idx`
next to a fresh `.run` fails the next replay's index build, and recording over
an existing trace leaves a `<case>.run.old` backup. Neither happens when the old
files are gone first.

Each `cases/<case>/` directory contains the program, debugger scripts, and its
local `.exe`, `.run`, index, and baseline files. All generated files are
ignored; only sources and debugger scripts are versioned.
