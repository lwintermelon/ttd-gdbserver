/* An access violation after a real call chain.
 *
 * The stop must carry the Windows exception code, the faulting instruction,
 * and both frames.
 *
 * Build: gcc -g -O0 -o fault.exe fault.c
 */
#include <stdio.h>

static volatile int *g_bad = (volatile int *)0x10;

static int fault(void) {
    return *g_bad;
}

static int wrapper(int n) {
    int value = n + 1;
    return fault() + value;
}

int main(void) {
    printf("before fault\n");
    fflush(stdout);
    return wrapper(3);
}
