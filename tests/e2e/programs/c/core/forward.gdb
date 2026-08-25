# A full forward session on one program: source stepping, a call boundary,
# instruction and range stepping, locals, stack, memory, the TEB (gs_base),
# TLS, all three watchpoint kinds, and the process exit at the end.
#
# Everything printed here must be the same in a live run and in the replay:
# no thread scheduling, no addresses of anything the loader may place
# differently (the harness normalizes hex), and only values after the
# statement that assigns them.
set pagination off
set confirm off
@FILE@
break main
@RUN@
next
next
print x
print y
# the callee breakpoint goes in before the call executes
break add
continue
info args
bt 3
x/8gx $rsp
next
print sum
# `finish` crosses the call boundary and reports the return value
finish
next
print z
# instruction-level stepping: both flavors, at the same statement
stepi
p/x $pc
nexti
p/x $pc
# range stepping (GDB sends vCont;r)
next
p/x $pc
# The TEB (`gs_base`, the register Delve resolves a goroutine's `g` through)
# is replay-only: the native x86-64 GDB has no `$gs_base`, so it belongs to
# the reverse script, which always has a remote target.
# data breakpoints: write, read, access on the loop's global. Breakpoint
# numbers are assigned in creation order (1 = main, 2 = add), so the three
# watchpoints are 3, 4 and 5 and are retired by id as the run progresses.
watch g_total
continue
print g_total
bt 3
delete 3
rwatch g_total
continue
print g_total
delete 4
awatch g_total
continue
print g_total
info locals
bt 3
# retire the last watchpoint so the continues below reach the end of the run
delete 5
# the recorded process exit. Live GDB reports "exited with code 42" here and
# has nothing to resume; the replay reports the synthetic SIGKILL stop, which
# the forward gate treats as the end of the transcript. The rr-style end
# sequence itself (`T09` then `W00`) is replay-only and lives in the reverse
# script.
continue
quit
