# Stack exhaustion: a deep recursion whose exhaustion point is covered by the
# reverse script. Walking the deep recursion frame by frame is what exercises
# stack unwinding at that depth.
#
# The script never *runs* the recursion to exhaustion: how many frames fit
# depends on where the loader placed the stack, so a live run and a replay
# overflow one or two frames apart (and a few instructions further into the
# compiler's stack probe). Every `finish`/`next` past the recursive call would
# inherit that difference, so unwinding here is the read-only kind: `frame` and
# `bt`, plus single instructions. The trace fixes the exhaustion point, so the
# reverse script asserts that one.
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
