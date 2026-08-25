/* One realistic C program that exercises the whole generic debugging surface.
 *
 * A call chain gives frames to step into and out of; the loop's global keeps
 * a stable address for write/read/access watchpoints; the TLS variable makes
 * GDB resolve a thread-local through the TEB.
 *
 * Build: gcc -g -O0 -o core.exe core.c
 */
#include <stdio.h>

static int g_total = 0;
static __thread int tls_counter = 7;

static int add(int a, int b) {
    int sum = a + b;
    return sum;
}

static int bump(int v) {
    g_total += v;
    return g_total;
}

int main(void) {
    int x = 17;
    int y = 25;
    int z = add(x, y);
    tls_counter += 1;

    int acc = 0;
    for (int i = 1; i <= 4; i++) {
        acc += bump(i * 3);
    }

    printf("z=%d acc=%d tls=%d\n", z, acc, tls_counter);
    return z;
}
