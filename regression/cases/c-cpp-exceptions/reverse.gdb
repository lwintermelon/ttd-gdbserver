# C++ exceptions on rewind: catchpoints are how a client asks to see throws,
# and the first-chance events a plain continue skips must still be reachable
# going backwards.
#
# The catchpoints live in this file (not the forward one) because they are a
# *replay* behavior: the trace has one fixed throw, and rewinding to it is the
# interesting direction. The forward script proves the skip.
set pagination off
set confirm off
@FILE@
@TARGET@
break main
continue
catch throw
continue
bt 4
info args
p/x $pc
catch catch
continue
bt 4
p/x $pc
# reverse-continue from the handler lands back on the throw
reverse-continue
p/x $pc
bt 4
reverse-stepi
p/x $pc
stepi
p/x $pc
# with every catchpoint gone the masked exception must not stop a continue
delete
continue
continue
quit
