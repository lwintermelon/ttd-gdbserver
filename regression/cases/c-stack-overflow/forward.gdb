# Stack exhaustion: a deep recursion whose exhaustion point is covered by the
# reverse script. Walking the deep recursion frame by frame is what exercises
# stack unwinding at that depth.
#
# The script never *runs* the recursion to exhaustion: the recorded stack depth
# is fixed, but a fresh `run` would need a stack as deep as the recording's.
# Unwinding here is therefore the read-only kind: `frame` and `bt`, plus single
# instructions. The reverse script walks the exhaustion point itself.
set pagination off
set confirm off
@FILE@
break main
@RUN@
next
next
# a shallow stop: frame navigation and single stepping, both cheap and free of
# side effects
break recurse if n == 3
continue
info args
bt 6
frame 2
info args
info locals
frame 0
stepi
p/x $pc
nexti
p/x $pc
# a deep stop: the same read-only navigation across hundreds of frames
break recurse if n == 200
continue
info args
bt 4
frame 1
info args
frame 0
quit
