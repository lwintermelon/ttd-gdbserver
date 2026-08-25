#pragma once
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ═══════════════════════════════════════════════════════════════════
 *  TTD Replay API — C ABI Wrapper
 *
 *  Projection types, opaque handles, callbacks, and function declarations.
 *  The C++ shim (ttd_wrapper.cpp) implements these against the TTD SDK.
 *
 *  C enum values mirror TTD SDK's C++ enum class definitions.
 *  Verified at compile time via static_assert in ttd_wrapper.cpp.
 * ═══════════════════════════════════════════════════════════════════ */

/* ---- Opaque Handles ---- */
typedef struct TtdEngine TtdEngine;
typedef struct TtdCursor TtdCursor;

/* ---- Enums (mirror TTD SDK C++ enum class values) ---- */

typedef enum TtdEventType {
    TTD_EVENT_MEMORY_WATCHPOINT   = 0,
    TTD_EVENT_POSITION_WATCHPOINT = 1,
    TTD_EVENT_EXCEPTION           = 2,
    TTD_EVENT_GAP                 = 3,
    TTD_EVENT_THREAD              = 4,
    TTD_EVENT_STEP_COUNT          = 5,
    TTD_EVENT_POSITION            = 6,
    TTD_EVENT_PROCESS             = 7,
    TTD_EVENT_INTERRUPTED         = 8,
    TTD_EVENT_ERROR               = 9,
} TtdEventType;

typedef enum TtdDataAccessMask {
    TTD_ACCESS_NONE    = 0,
    TTD_ACCESS_READ    = 1 << 0,
    TTD_ACCESS_WRITE   = 1 << 1,
    TTD_ACCESS_EXECUTE = 1 << 2,
} TtdDataAccessMask;

typedef enum TtdEventMask {
    TTD_EVENT_MASK_MEMORY_WATCHPOINT   = 1 << 0,
    TTD_EVENT_MASK_POSITION_WATCHPOINT = 1 << 1,
    TTD_EVENT_MASK_EXCEPTION           = 1 << 2,
    TTD_EVENT_MASK_GAP                 = 1 << 3,
    TTD_EVENT_MASK_THREAD              = 1 << 4,
    TTD_EVENT_MASK_ALL                 = 0x1F,
} TtdEventMask;

typedef enum TtdExceptionMask {
    TTD_EXCEPTION_MASK_HARDWARE   = 1 << 0,
    TTD_EXCEPTION_MASK_SOFTWARE   = 1 << 1,
    TTD_EXCEPTION_MASK_CPLUSPLUS  = 1 << 2,
    TTD_EXCEPTION_MASK_DEBUGPRINT = 1 << 3,
    TTD_EXCEPTION_MASK_ALL        = 0x0F,
} TtdExceptionMask;

/* Mirrors the TTD SDK QueryMemoryPolicy. With the Default policy the engine
 * may return memory observed only by the current thread, reading zeros for
 * memory written by other threads (e.g. the Go runtime's allgs array, which
 * is populated across threads). Debugger-style reads should use a global
 * policy instead. */
typedef enum TtdQueryMemoryPolicy {
    TTD_MEM_POLICY_DEFAULT                = 0,
    TTD_MEM_POLICY_THREAD_LOCAL           = 1,
    TTD_MEM_POLICY_GLOBALLY_CONSERVATIVE  = 2,
    TTD_MEM_POLICY_GLOBALLY_AGGRESSIVE    = 3,
    TTD_MEM_POLICY_IN_FRAGMENT_AGGRESSIVE = 4,
} TtdQueryMemoryPolicy;

/* ---- Projection Types (C++ → C ABI) ---- */

typedef struct TtdPosition {
    uint64_t sequence;
    uint64_t steps;
} TtdPosition;

typedef struct TtdPositionRange {
    TtdPosition min;
    TtdPosition max;
} TtdPositionRange;

typedef struct TtdThreadInfo {
    uint32_t unique_id;      /* UniqueThreadId */
    uint32_t os_thread_id;   /* OS ThreadId */
    TtdPositionRange lifetime;
    TtdPositionRange active_time;
} TtdThreadInfo;

/* Flattened active thread info — no pointers to TTD-managed memory */
typedef struct TtdActiveThreadInfo {
    TtdThreadInfo thread;
    TtdPosition   current_position;
} TtdActiveThreadInfo;

/* x86_64 registers extracted from AMD64_CONTEXT (GPRs + x87/SSE from FltSave) */
typedef struct TtdX64Regs {
    uint64_t rax, rcx, rdx, rbx;
    uint64_t rsp, rbp, rsi, rdi;
    uint64_t r8,  r9,  r10, r11;
    uint64_t r12, r13, r14, r15;
    uint64_t rip;
    uint32_t eflags;
    uint16_t cs, ss, ds, es, fs, gs;
    /* x87 FPU state (from AMD64_CONTEXT.FltSave) */
    uint16_t fctrl, fstat;
    uint8_t  ftag;
    uint16_t fop;
    uint32_t fioff, fooff;
    uint16_t fiseg, foseg;
    uint8_t  st[80];    /* 8 x 10-byte x87 registers (st0..st7) */
    /* SSE state (from AMD64_CONTEXT.FltSave) */
    uint32_t mxcsr;
    uint8_t  xmm[256];  /* 16 x 16-byte XMM registers (xmm0..xmm15) */
} TtdX64Regs;

/* ReplayResult simplified */
typedef struct TtdReplayResult {
    uint8_t  stop_reason;         /* EventType enum value */
    uint64_t steps_executed;
    uint64_t instructions_executed;
    /* Valid when stop_reason == TTD_EVENT_MEMORY_WATCHPOINT */
    uint64_t wp_address;
    uint64_t wp_size;
    uint8_t  wp_access_type;      /* DataAccessMask bits */
} TtdReplayResult;

/* Exception event (self-contained, no pointers) */
typedef struct TtdExceptionEvent {
    uint32_t    code;
    uint32_t    flags;
    uint64_t    exception_address;
    uint64_t    program_counter;
    TtdPosition position;
} TtdExceptionEvent;

/* Callback types */
typedef bool (*TtdWatchpointCb)(void* ctx, uint64_t addr, uint64_t size, uint8_t access_type);
typedef void (*TtdProgressCb)(void* ctx, TtdPosition pos);

/* ---- Engine ---- */
TtdEngine* ttd_engine_create(void);
void       ttd_engine_destroy(TtdEngine* eng);
int        ttd_engine_load(TtdEngine* eng, const uint16_t* path); /* wchar_t* (null-terminated) */
TtdPosition ttd_engine_first_pos(TtdEngine* eng);
TtdPosition ttd_engine_last_pos(TtdEngine* eng);
uint32_t    ttd_engine_thread_count(TtdEngine* eng);
TtdThreadInfo ttd_engine_thread_info(TtdEngine* eng, uint32_t idx);

/* ---- Cursor ---- */
TtdCursor*  ttd_cursor_create(TtdEngine* eng);
void        ttd_cursor_destroy(TtdCursor* cur);

/* Position */
TtdPosition ttd_cursor_get_pos(TtdCursor* cur);
void        ttd_cursor_set_pos(TtdCursor* cur, TtdPosition pos);

/* Replay: returns 0 on success, -1 on error */
int ttd_cursor_step_forward(TtdCursor* cur, uint64_t steps, TtdReplayResult* out);
int ttd_cursor_step_backward(TtdCursor* cur, uint64_t steps, TtdReplayResult* out);
int ttd_cursor_replay_forward_to(TtdCursor* cur, TtdPosition limit, TtdReplayResult* out);
int ttd_cursor_replay_backward_to(TtdCursor* cur, TtdPosition limit, TtdReplayResult* out);

/* Memory — returns bytes actually read */
uint32_t ttd_cursor_read_mem(TtdCursor* cur, uint64_t addr, uint8_t* buf, uint32_t len);

/* Registers */
TtdX64Regs ttd_cursor_read_regs(TtdCursor* cur);
uint64_t   ttd_cursor_get_pc(TtdCursor* cur);
uint64_t   ttd_cursor_get_sp(TtdCursor* cur);
/* OS thread id of the cursor's current thread (0 if none). */
uint32_t   ttd_cursor_get_current_tid(TtdCursor* cur);

/* Thread info at current cursor position */
uint32_t    ttd_cursor_thread_count(TtdCursor* cur);
TtdActiveThreadInfo ttd_cursor_active_thread_info(TtdCursor* cur, uint32_t thread_idx);
TtdPosition ttd_cursor_get_thread_pos(TtdCursor* cur, uint32_t thread_idx);

/* Per-thread queries (pass thread_id=0 for current thread) */
TtdX64Regs ttd_cursor_read_regs_thread(TtdCursor* cur, uint32_t thread_id);
uint64_t   ttd_cursor_get_pc_thread(TtdCursor* cur, uint32_t thread_id);
uint64_t   ttd_cursor_get_sp_thread(TtdCursor* cur, uint32_t thread_id);
TtdPosition ttd_cursor_get_pos_thread(TtdCursor* cur, uint32_t thread_id);
/* Thread environment block address (GS base on Windows x64).
 * This is what Delve needs to locate the goroutine 'g' without writing
 * memory. Pass thread_id=0 for the current thread. */
uint64_t   ttd_cursor_get_teb_thread(TtdCursor* cur, uint32_t thread_id);

/* Exception events from engine */
uint32_t ttd_engine_exception_count(TtdEngine* eng);
TtdExceptionEvent ttd_engine_exception_event(TtdEngine* eng, uint32_t idx);

/* Module info */
uint32_t ttd_engine_module_count(TtdEngine* eng);
void     ttd_engine_module_info(TtdEngine* eng, uint32_t idx,
                                uint64_t* out_addr, uint64_t* out_size,
                                uint16_t* out_name_buf, uint32_t name_buf_len);

/* Watchpoints */
int  ttd_cursor_add_watchpoint(TtdCursor* cur, uint64_t addr, uint64_t size, uint8_t access_mask, uint32_t thread_id);
int  ttd_cursor_remove_watchpoint(TtdCursor* cur, uint64_t addr, uint64_t size, uint8_t access_mask, uint32_t thread_id);

/* Callbacks */
void ttd_cursor_set_watchpoint_cb(TtdCursor* cur, TtdWatchpointCb cb, void* ctx);
void ttd_cursor_set_progress_cb(TtdCursor* cur, TtdProgressCb cb, void* ctx);

/* Interrupt replay (callable from any thread) */
void ttd_cursor_interrupt(TtdCursor* cur);

/* Event masks */
void ttd_cursor_set_event_mask(TtdCursor* cur, uint32_t mask);
void ttd_cursor_set_exception_mask(TtdCursor* cur, uint32_t mask);

/* Memory query policy (see TtdQueryMemoryPolicy) */
void ttd_cursor_set_memory_policy(TtdCursor* cur, uint32_t policy);

/* Index: build the global memory index for accurate memory recovery.
 * Returns 0 on success, non-zero on failure. */
int ttd_engine_build_index(TtdEngine* eng);

#ifdef __cplusplus
}
#endif
