# One worker's frame, pinned by its argument, so the stop is the same one on
# every replay. Thread enumeration, per-thread breakpoints and a cross-thread
# watchpoint are the reverse script's subject.
set pagination off
set confirm off
@FILE@
break main
@RUN@
break worker if arg == 0
continue
info args
bt 4
next
print id
next
bt 3
print arg
# finish crosses out of the worker into the pthread wrapper: a thread that is
# not the main thread calling into runtime code
finish
bt 3
# A global written by the workers, read from the main thread after the join:
# correct only if the replay accounts for writes made by threads other than
# the one being queried.
break threads.c:38
continue
print shared
quit
