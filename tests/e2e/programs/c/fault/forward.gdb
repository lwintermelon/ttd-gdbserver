# Hardware exception on the forward path: the access violation must stop the
# continue with the Windows code, the faulting instruction and both frames —
# a replay that ran to the trace end instead would hide every crash.
set pagination off
set confirm off
@FILE@
break main
@RUN@
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
quit
