# Stack exhaustion on rewind. The trace fixes the exhaustion point, so this is
# the script that asserts it: the hardware exception code, the faulting
# instruction inside the compiler's stack probe, the very deep frame list, and
# a reverse walk back out of it.
set pagination off
set confirm off
@FILE@
@TARGET@
break main
continue
# run straight into the exhaustion: the recursion breakpoint is deliberately
# absent, so the first stop is the fault itself
continue
bt 8
info args
x/i $pc
p/x $pc
# rewind out of the fault, then step forward again onto it
reverse-stepi
p/x $pc
stepi
p/x $pc
# the frame above the fault still has its locals; unwinding reads them without
# executing anything
frame 1
info args
info locals
x/4gx $rsp
frame 0
reverse-continue
p/x $pc
info args
monitor ttd events
quit
