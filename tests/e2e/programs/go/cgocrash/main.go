// Go -> C -> Go callback, then a fault in the C frames: the stop must show
// mixed Go and C frames.
//
// Build: CGO_ENABLED=1 go build -gcflags="all=-N -l" -o cgocrash.exe .
package main

/*
#include <stdint.h>
extern void goMarker(int32_t v);
static void crashNow(void) {
	volatile int *p = (volatile int *)0;
	*p = 42;
}
static int tramp(int32_t v) {
	goMarker(v);
	crashNow();
	return 0;
}
*/
import "C"

import "fmt"

//export goMarker
func goMarker(v C.int32_t) {
	fmt.Println("marker", v)
}

func main() {
	fmt.Println("before")
	C.tramp(7)
	fmt.Println("after")
}
