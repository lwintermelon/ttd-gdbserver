# C++ exception policy on the forward path: the exception class is masked, so
# a continue passes a `throw` instead of stopping on it. Both threads throw
# and catch; the breakpoint after the main thread's catch proves execution got
# past it. Catchpoints (GDB implements those on its own side) are the reverse
# script's subject.
#
# Nothing here depends on which thread runs first: both throw/catch pairs are
# identical, and the breakpoint is on the main thread's post-catch statement.
set pagination off
set confirm off
@FILE@
break main
# line 37: the statement after the catch block, i.e. after both throws
break cpp_exceptions.cpp:39
@RUN@
continue
print value
next
next
quit
