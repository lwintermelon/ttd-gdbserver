# The same program walked backwards: retrace each stop, reverse source and
# instruction stepping, watchpoint hits in reverse order, the wire-level end
# sequence, and the monitor surface (replay-only: GDB needs a remote target
# for `monitor`).
set pagination off
set confirm off
@FILE@
@TARGET@
break main
continue
p/x $pc
next
p/x $pc
next
p/x $pc
break add
continue
p/x $pc
info args
info locals
bt 3
x/8gx $rsp
reverse-next
p/x $pc
stepi
p/x $pc
reverse-stepi
p/x $pc
reverse-continue
p/x $pc
info locals
# The TEB: `gs_base` is the register Delve resolves a goroutine's `g`
# through, and it is replay-only (the native x86-64 GDB has no such register).
p/x $gs_base
x/2gx $gs_base
# watchpoint hits, retraced in reverse
watch g_total
continue
print g_total
continue
print g_total
reverse-continue
print g_total
reverse-continue
print g_total
delete 3
# Every register, including the SSE file and fs_base/gs_base: this is the `p`
# packet for each register in the target description.
info all-registers
# A hardware breakpoint (Z1), which a plain `break` does not exercise.
hbreak bump
continue
info args
delete 4
# A registration TTD itself refuses (a watchpoint of size 0) must answer E16
# instead of acknowledging a breakpoint that can never fire. An in-range
# address is not the test: TTD accepts watchpoints anywhere, so the server has
# no mapping to validate against.
maintenance packet Z2,171143d000,0
# qXfer annexes a plain GDB session never asks for, but Delve does: auxv is
# how Delve computes the DWARF static base, and the error path for an unknown
# feature file must answer an error rather than drop the session.
maintenance packet qXfer:auxv:read::0,ff
maintenance packet qXfer:exec-file:read::0,ff
maintenance packet qXfer:features:read:nosuch.xml:0,10
# The old hex-encoded qRRCmd form, which pre-5.8 Delve clients still speak.
maintenance packet qRRCmd:7768656e
# Windows thread information block, the register Delve uses for goroutine `g`.
maintenance packet qGetTIBAddr:p1.686c
maintenance packet qGetTLSAddr:p1.686c,0,0
# rr vocabulary over plain GDB: Delve's qRRCmd commands are ordinary packets,
# so GDB can send them with `maintenance packet` (replies are hex-encoded text).
maintenance packet qRRCmd:when:-1
maintenance packet qRRCmd:checkpoint:-1:
maintenance packet qRRCmd:info checkpoints:-1
maintenance packet qRRCmd:delete checkpoint:-1:31
# deleting a checkpoint that does not exist answers text, not silence
maintenance packet qRRCmd:delete checkpoint:-1:37
maintenance packet qRRCmd:info checkpoints:-1
# Rejected restart forms: a malformed position and an unknown checkpoint id
# must answer an error packet instead of moving the cursor.
maintenance packet vRun;;7a7a7a
maintenance packet vRun;;63393939
maintenance packet qRRCmd:when:-1
# vRun restarts to the trace start; the resume that follows stays there.
maintenance packet vRun;;
maintenance packet qRRCmd:when:-1
# `R` (the GDB restart form) does the same and answers nothing, as the
# protocol specifies for R XX.
maintenance packet R00
maintenance packet qRRCmd:when:-1
# monitor surface: every subcommand
monitor ttd info
monitor ttd threads
monitor ttd stats
monitor ttd events
monitor ttd module 0xffffffffffffffff
# The rr end sequence, at the wire level: the resume after a restart stops at
# the restart position, the next one replays to the recorded process exit and
# reports the synthetic SIGKILL (`T09`, rewindable, Delve's "almost exited"),
# and a further forward resume answers the real exit (`W00`).
maintenance packet vCont;c
maintenance packet vCont;c
maintenance packet vCont;c
