# GDB RSP reference

The server is multi-process with one pid and uses all-stop semantics. Replay is
read-only: `M`, `P`, and `X` return errors, and signal-delivery requests are
ignored because the recorded event stream is authoritative.

## Packet surface

- **Registers, memory, threads:** `g`/`G`, `p`/`P`, `m`/`M`, `H`,
  `qfThreadInfo`/`qsThreadInfo`, `qThreadExtraInfo`.
- **Execution:** `vCont;c`, `vCont;s`, `vCont;r` (range step), `c`/`s`,
  `bc`/`bs`, `vRun`, `R`, and `D`.
- **Breakpoints:** `Z0`/`Z1` execute breakpoints, simulated with TTD execute
  watchpoints; `Z2`–`Z4` write/read/access watchpoints.
- **Introspection:** `qXfer:features:read`, `qXfer:exec-file:read`,
  `qXfer:auxv:read`, `qXfer:libraries:read`, `qOffsets`, `qGetTLSAddr`,
  `qSupported`, `qAttached`, and `qRcmd`.
- **rr navigation:** `qRRCmd when` and checkpoint commands; `vRun` restart by
  position or checkpoint.

Stop replies use `T05` for breakpoint/step, `T09` for trace end and the initial
process-exit stop, `T00` for trace start, and `T02` for interruption. A further
forward resume after process exit returns `W00`; backward resume can still
rewind from the exit stop.

`qXfer:memory-map` is deliberately absent: GDB treats an advertised map as the
complete readable set and would reject stack/TEB accesses. `QNonStop`,
fork/vfork events, launch configuration, and `QListThreadsInStopReply` are also
unsupported. Unknown thread ids return an error rather than a zeroed context.

## Monitor commands

| Command | Output |
|---|---|
| `monitor ttd info` | Trace lifetime, current position, index status, module/thread/exception counts |
| `monitor ttd threads` | Every recorded thread: UTID, OS tid, active range |
| `monitor ttd module <addr>` | Module containing an address and its range |
| `monitor ttd mem <addr> [len]` | Bytes and the trace position of each recovered run; default 16, maximum 256 |
| `monitor ttd events [n]` | Recorded exceptions with code, address, PC, and position; default 20 |
| `monitor ttd stats` | Query, seek, step, replay, and physical-watchpoint counters |

`monitor ttd mem` is the first diagnostic for a suspicious read. An old
position is not itself an error: unchanged memory can legitimately come from
an old data line. The memory model is explained in
[`ttd-internals.md`](ttd-internals.md).
