/* Deliberate stack overflow: the trace ends in a stack-exhaustion exception
 * after thousands of frames, which is a different exception code from an
 * access violation.
 *
 * Build: gcc -g -O0 -o stack_overflow.exe stack_overflow.c
 */
#include <stdio.h>
#include <string.h>

static int recurse(int n) {
    volatile char pad[4096];
    memset((void *)pad, n & 0xff, sizeof(pad));
    int v = pad[0];
    return recurse(n + 1) + v;
}

int main(void) {
    printf("starting recursion\n");
    fflush(stdout);
    return recurse(0);
}
