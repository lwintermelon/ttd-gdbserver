# Threads on rewind: enumerate and select threads, retrace a cross-thread
# watchpoint in both directions, and walk one worker backwards. Replay-only:
# live thread ids and the order in which the workers write `shared` are
# scheduling-dependent; the trace fixes both.
set pagination off
set confirm off
@FILE@
@TARGET@
break main
continue
info threads
# `shared` is written by whichever worker holds the lock, so each hit is a
# real cross-thread write. Breakpoints are numbered from 1 (main).
watch shared
continue
print shared
bt 4
continue
print shared
bt 4
reverse-continue
print shared
# select the worker explicitly, then step inside it in both directions
info threads
thread 2
bt 3
info locals
stepi
p/x $pc
reverse-stepi
p/x $pc
# cross-thread read: the main thread sees the value the workers wrote
thread 1
bt 3
print shared
quit
