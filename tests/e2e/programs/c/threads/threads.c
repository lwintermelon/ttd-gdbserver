/* Two worker threads sharing one counter under a mutex.
 *
 * Gives the server several threads to enumerate and select between, and one
 * shared address written by whichever thread holds the lock.
 *
 * Build: gcc -g -O0 -pthread -o threads.exe threads.c
 */
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static int shared = 0;

static void *worker(void *arg) {
    int id = (int)(intptr_t)arg;
    for (int i = 0; i < 6; i++) {
        pthread_mutex_lock(&lock);
        shared += id + 1;
        int seen = shared;
        pthread_mutex_unlock(&lock);
        if (seen % 5 == 0) {
            printf("worker %d seen %d\n", id, seen);
        }
    }
    return NULL;
}

int main(void) {
    pthread_t threads[2];
    for (int i = 0; i < 2; i++) {
        pthread_create(&threads[i], NULL, worker, (void *)(intptr_t)i);
    }
    for (int i = 0; i < 2; i++) {
        pthread_join(threads[i], NULL);
    }
    printf("shared %d\n", shared);
    return shared;
}
