# Hardware exception on rewind: the access violation must stop both directions
# with the recorded code, so a reverse continue has to land on the same
# instruction a forward one did.
set pagination off
set confirm off
@FILE@
@TARGET@
break main
continue
p/x $pc
next
next
next
p/x $pc
bt 4
info locals
x/i $pc
frame 1
info args
info locals
x/4gx $rsp
frame 0
# rewind out of the fault and step forward onto it again: the same instruction,
# the same frames
reverse-stepi
p/x $pc
stepi
p/x $pc
bt 3
# reverse-continue has to stop at the fault, not run back to the breakpoint
reverse-continue
p/x $pc
bt 3
monitor ttd events
quit
