"""Shared machinery for the e2e commands.

One definition per concern, so a command module only contains what is
specific to its gate:

- `common`    — paths, `Case`, case discovery/selection, child environment,
                subprocess helpers, artifact directories, diff rendering
- `clients`   — `Server` (the ttd-gdbserver process), `run_gdb`, `run_delve`,
                GDB script template rendering
- `normalize` — transcript normalization and client script-error detection
- `runner`    — parallel per-case execution with uniform PASS/FAIL/SKIP output
"""
