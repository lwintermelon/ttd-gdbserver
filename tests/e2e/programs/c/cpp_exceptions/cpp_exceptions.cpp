// C++ first-chance exceptions, on the main thread and on a worker.
//
// The server masks the C++ exception group, so a plain continue must pass a
// `throw` without stopping, while `catch throw`/`catch catch` still report
// it. MinGW records the exception code 0x20474343; MSVC would record
// 0xE06D7363. Both must map to SIGABRT when surfaced.
//
// Build: g++ -g -O0 -pthread -o cpp_exceptions.exe cpp_exceptions.cpp
#include <cstdio>
#include <stdexcept>
#include <thread>

static int thrower(int x) {
    if (x < 0) {
        throw std::runtime_error("negative");
    }
    return x * 2;
}

static void worker() {
    try {
        thrower(-1);
    } catch (const std::exception &e) {
        std::printf("worker caught: %s\n", e.what());
    }
}

int main() {
    std::thread t(worker);
    t.join();

    int value = 0;
    try {
        value = thrower(-1);
    } catch (const std::exception &e) {
        std::printf("main caught: %s\n", e.what());
        value = -1;
    }
    std::printf("value: %d\n", value);
    return value;
}
