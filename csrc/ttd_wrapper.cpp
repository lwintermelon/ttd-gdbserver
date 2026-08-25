#include "ttd_bindings.h"
#include "ttd_wrapper.h"

#include <TTD/IReplayEngineStl.h>
#include <TTD/IReplayEngineRegisters.h>
#include <TTD/ErrorReporting.h>
#include <cstring>

using namespace TTD;
using namespace Replay;

/* ═══════════════════════════════════════════════════════════════════
 *  Compile-time verification: C enum values match TTD SDK enum class.
 *
 *  If any of these fail, ttd_bindings.h is out of sync with the SDK.
 * ═══════════════════════════════════════════════════════════════════ */

static_assert(TTD_EVENT_MEMORY_WATCHPOINT   == static_cast<int>(EventType::MemoryWatchpoint),   "EventType::MemoryWatchpoint mismatch");
static_assert(TTD_EVENT_POSITION_WATCHPOINT == static_cast<int>(EventType::PositionWatchpoint), "EventType::PositionWatchpoint mismatch");
static_assert(TTD_EVENT_EXCEPTION           == static_cast<int>(EventType::Exception),           "EventType::Exception mismatch");
static_assert(TTD_EVENT_GAP                 == static_cast<int>(EventType::Gap),                 "EventType::Gap mismatch");
static_assert(TTD_EVENT_THREAD              == static_cast<int>(EventType::Thread),              "EventType::Thread mismatch");
static_assert(TTD_EVENT_STEP_COUNT          == static_cast<int>(EventType::StepCount),           "EventType::StepCount mismatch");
static_assert(TTD_EVENT_POSITION            == static_cast<int>(EventType::Position),            "EventType::Position mismatch");
static_assert(TTD_EVENT_PROCESS             == static_cast<int>(EventType::Process),             "EventType::Process mismatch");
static_assert(TTD_EVENT_INTERRUPTED         == static_cast<int>(EventType::Interrupted),         "EventType::Interrupted mismatch");
static_assert(TTD_EVENT_ERROR               == static_cast<int>(EventType::Error),               "EventType::Error mismatch");

static_assert(TTD_ACCESS_READ    == static_cast<int>(DataAccessMask::Read),    "DataAccessMask::Read mismatch");
static_assert(TTD_ACCESS_WRITE   == static_cast<int>(DataAccessMask::Write),   "DataAccessMask::Write mismatch");
static_assert(TTD_ACCESS_EXECUTE == static_cast<int>(DataAccessMask::Execute), "DataAccessMask::Execute mismatch");

static_assert(TTD_EVENT_MASK_MEMORY_WATCHPOINT   == static_cast<int>(EventMask::MemoryWatchpoint),   "EventMask::MemoryWatchpoint mismatch");
static_assert(TTD_EVENT_MASK_POSITION_WATCHPOINT == static_cast<int>(EventMask::PositionWatchpoint), "EventMask::PositionWatchpoint mismatch");
static_assert(TTD_EVENT_MASK_EXCEPTION           == static_cast<int>(EventMask::Exception),           "EventMask::Exception mismatch");
static_assert(TTD_EVENT_MASK_GAP                 == static_cast<int>(EventMask::Gap),                 "EventMask::Gap mismatch");
static_assert(TTD_EVENT_MASK_THREAD              == static_cast<int>(EventMask::Thread),              "EventMask::Thread mismatch");
static_assert(TTD_EVENT_MASK_ALL                 == static_cast<int>(EventMask::All),                 "EventMask::All mismatch");

static_assert(TTD_EXCEPTION_MASK_HARDWARE   == static_cast<int>(ExceptionMask::Hardware),   "ExceptionMask::Hardware mismatch");
static_assert(TTD_EXCEPTION_MASK_SOFTWARE   == static_cast<int>(ExceptionMask::Software),   "ExceptionMask::Software mismatch");
static_assert(TTD_EXCEPTION_MASK_CPLUSPLUS  == static_cast<int>(ExceptionMask::CPlusPlus),  "ExceptionMask::CPlusPlus mismatch");
static_assert(TTD_EXCEPTION_MASK_DEBUGPRINT == static_cast<int>(ExceptionMask::DebugPrint), "ExceptionMask::DebugPrint mismatch");
static_assert(TTD_EXCEPTION_MASK_ALL        == static_cast<int>(ExceptionMask::All),         "ExceptionMask::All mismatch");

static_assert(TTD_MEM_POLICY_DEFAULT                == static_cast<int>(QueryMemoryPolicy::Default),                "QueryMemoryPolicy::Default mismatch");
static_assert(TTD_MEM_POLICY_THREAD_LOCAL           == static_cast<int>(QueryMemoryPolicy::ThreadLocal),           "QueryMemoryPolicy::ThreadLocal mismatch");
static_assert(TTD_MEM_POLICY_GLOBALLY_CONSERVATIVE  == static_cast<int>(QueryMemoryPolicy::GloballyConservative),  "QueryMemoryPolicy::GloballyConservative mismatch");
static_assert(TTD_MEM_POLICY_GLOBALLY_AGGRESSIVE    == static_cast<int>(QueryMemoryPolicy::GloballyAggressive),    "QueryMemoryPolicy::GloballyAggressive mismatch");
static_assert(TTD_MEM_POLICY_IN_FRAGMENT_AGGRESSIVE == static_cast<int>(QueryMemoryPolicy::InFragmentAggressive), "QueryMemoryPolicy::InFragmentAggressive mismatch");

/* ---- Internal handle structures ---- */

struct TtdEngine {
    UniqueReplayEngine engine;
};

struct TtdCursor {
    UniqueCursor cursor;
    /* Callback storage */
    TtdWatchpointCb wp_cb  = nullptr;
    void*           wp_ctx = nullptr;
    TtdProgressCb   prog_cb  = nullptr;
    void*           prog_ctx = nullptr;
};

/* ---- Helper: Position conversion ---- */
static Position to_cpp_pos(TtdPosition p) {
    Position cpp;
    cpp.Sequence = SequenceId(p.sequence);
    cpp.Steps    = StepCount(p.steps);
    return cpp;
}

static TtdPosition from_cpp_pos(Position p) {
    return TtdPosition{
        static_cast<uint64_t>(p.Sequence),
        static_cast<uint64_t>(p.Steps)
    };
}

static TtdPositionRange from_cpp_range(PositionRange r) {
    return TtdPositionRange{ from_cpp_pos(r.Min), from_cpp_pos(r.Max) };
}

/* ---- Helper: ReplayResult conversion ---- */
static void fill_result(TtdReplayResult* out, ICursorView::ReplayResult const& rr) {
    if (!out) return;
    out->stop_reason          = static_cast<uint8_t>(rr.StopReason);
    out->steps_executed       = static_cast<uint64_t>(rr.StepsExecuted);
    out->instructions_executed = static_cast<uint64_t>(rr.InstructionsExecuted);
    if (rr.StopReason == EventType::MemoryWatchpoint) {
        out->wp_address     = static_cast<uint64_t>(rr.MemoryWatchpoint.Address);
        out->wp_size        = rr.MemoryWatchpoint.Size;
        out->wp_access_type = static_cast<uint8_t>(rr.MemoryWatchpoint.AccessType);
    } else {
        out->wp_address     = 0;
        out->wp_size        = 0;
        out->wp_access_type = 0;
    }
}

/* ---- Callback shims ---- */

static bool __fastcall WatchpointCbShim(
    uintptr_t context,
    TTD::Replay::ICursorView::MemoryWatchpointResult const& result,
    TTD::Replay::IThreadView const*
) {
    auto* cur = reinterpret_cast<TtdCursor*>(context);
    if (cur->wp_cb) {
        return cur->wp_cb(
            cur->wp_ctx,
            static_cast<uint64_t>(result.Address),
            result.Size,
            static_cast<uint8_t>(result.AccessType)
        );
    }
    return true; /* stop by default */
}

static void __stdcall ProgressCbShim(uintptr_t context, Position const& position) {
    auto* cur = reinterpret_cast<TtdCursor*>(context);
    if (cur->prog_cb) {
        cur->prog_cb(cur->prog_ctx, from_cpp_pos(position));
    }
}

/* ---- Engine ---- */

TtdEngine* ttd_engine_create(void) {
    auto [engine, err] = MakeReplayEngine();
    if (err != 0 || !engine) {
        return nullptr;
    }
    auto* e = new TtdEngine{ std::move(engine) };
    return e;
}

void ttd_engine_destroy(TtdEngine* eng) {
    if (eng) delete eng;
}

int ttd_engine_load(TtdEngine* eng, const uint16_t* path) {
    if (!eng || !path) return -1;
    if (!eng->engine->Initialize(reinterpret_cast<wchar_t const*>(path))) {
        return -2;
    }
    return 0;
}

static void __stdcall IndexProgressCbShim(void const*, TTD::Replay::IndexBuildProgressType const* p) {
    if (p && p->KeyframeCount > 0) {
        fprintf(stderr, "[ttd] index build %u/%u keyframes\n",
                p->KeyframesProcessed, p->KeyframeCount);
    }
}

int ttd_engine_build_index(TtdEngine* eng) {
    if (!eng) return -1;
    // The progress callback must be non-null: passing nullptr crashes the
    // engine in this SDK version (0.9.5).
    auto status = eng->engine->BuildIndex(IndexProgressCbShim, nullptr);
    return (status == TTD::Replay::IndexStatus::IndexFileLoaded) ? 0 : -2;
}

TtdPosition ttd_engine_first_pos(TtdEngine* eng) {
    if (!eng) return {0, 0};
    return from_cpp_pos(eng->engine->GetFirstPosition());
}

TtdPosition ttd_engine_last_pos(TtdEngine* eng) {
    if (!eng) return {0, 0};
    return from_cpp_pos(eng->engine->GetLastPosition());
}

uint32_t ttd_engine_thread_count(TtdEngine* eng) {
    if (!eng) return 0;
    return static_cast<uint32_t>(eng->engine->GetThreadCount());
}

TtdThreadInfo ttd_engine_thread_info(TtdEngine* eng, uint32_t idx) {
    TtdThreadInfo info = {};
    if (!eng) return info;
    auto count = eng->engine->GetThreadCount();
    if (idx >= count) return info;
    auto* list = eng->engine->GetThreadList();
    if (!list) return info;
    auto& ti = list[idx];
    info.unique_id     = static_cast<uint32_t>(ti.UniqueId);
    info.os_thread_id  = static_cast<uint32_t>(ti.Id);
    info.lifetime      = from_cpp_range(ti.Lifetime);
    info.active_time   = from_cpp_range(ti.ActiveTime);
    return info;
}

/* ---- Cursor ---- */

TtdCursor* ttd_cursor_create(TtdEngine* eng) {
    if (!eng) return nullptr;
    ICursor* raw = eng->engine->NewCursor();
    if (!raw) return nullptr;
    auto* c = new TtdCursor{ UniqueCursor(raw) };
    /* Set initial position to trace start */
    c->cursor->SetPosition(Position::Min);
    /* The Default memory policy may only surface memory observed by the
     * current thread. Debugger reads want the whole address space, so use a
     * global policy. Conservative only returns high-confidence memory (it may
     * short-read instead of guessing). Accurate recovery of old writes (e.g.
     * the Go runtime's allgs array) additionally requires the trace index,
     * built by ttd_engine_build_index. */
    c->cursor->SetDefaultMemoryPolicy(QueryMemoryPolicy::GloballyConservative);
    return c;
}

void ttd_cursor_destroy(TtdCursor* cur) {
    if (cur) delete cur;
}

TtdPosition ttd_cursor_get_pos(TtdCursor* cur) {
    if (!cur) return {0, 0};
    return from_cpp_pos(cur->cursor->GetPosition());
}

void ttd_cursor_set_pos(TtdCursor* cur, TtdPosition pos) {
    if (!cur) return;
    cur->cursor->SetPosition(to_cpp_pos(pos));
}

int ttd_cursor_step_forward(TtdCursor* cur, uint64_t steps, TtdReplayResult* out) {
    if (!cur) return -1;
    auto rr = cur->cursor->ReplayForward(StepCount(steps));
    fill_result(out, rr);
    return 0;
}

int ttd_cursor_step_backward(TtdCursor* cur, uint64_t steps, TtdReplayResult* out) {
    if (!cur) return -1;
    auto rr = cur->cursor->ReplayBackward(StepCount(steps));
    fill_result(out, rr);
    return 0;
}

int ttd_cursor_replay_forward_to(TtdCursor* cur, TtdPosition limit, TtdReplayResult* out) {
    if (!cur) return -1;
    auto rr = cur->cursor->ReplayForward(to_cpp_pos(limit));
    fill_result(out, rr);
    return 0;
}

int ttd_cursor_replay_backward_to(TtdCursor* cur, TtdPosition limit, TtdReplayResult* out) {
    if (!cur) return -1;
    auto rr = cur->cursor->ReplayBackward(to_cpp_pos(limit));
    fill_result(out, rr);
    return 0;
}

/* ---- Memory ---- */

uint32_t ttd_cursor_read_mem(TtdCursor* cur, uint64_t addr, uint8_t* buf, uint32_t len) {
    if (!cur || !buf || len == 0) return 0;
    BufferView bv(buf, static_cast<size_t>(len));
    auto mb = cur->cursor->QueryMemoryBuffer(GuestAddress(addr), bv);
    return static_cast<uint32_t>(mb.Memory.Size);
}

/* ---- Registers ---- */

TtdX64Regs ttd_cursor_read_regs(TtdCursor* cur) {
    TtdX64Regs regs = {};
    if (!cur) return regs;
    auto rc = cur->cursor->GetCrossPlatformContext();
    CROSS_PLATFORM_CONTEXT cpc = rc;
    auto& amd = cpc.Amd64Context;
    regs.rax = amd.Rax;  regs.rcx = amd.Rcx;  regs.rdx = amd.Rdx;  regs.rbx = amd.Rbx;
    regs.rsp = amd.Rsp;  regs.rbp = amd.Rbp;  regs.rsi = amd.Rsi;  regs.rdi = amd.Rdi;
    regs.r8  = amd.R8;   regs.r9  = amd.R9;   regs.r10 = amd.R10;  regs.r11 = amd.R11;
    regs.r12 = amd.R12;  regs.r13 = amd.R13;  regs.r14 = amd.R14;  regs.r15 = amd.R15;
    regs.rip = amd.Rip;
    regs.eflags = amd.EFlags;
    regs.cs = amd.SegCs; regs.ss = amd.SegSs;
    regs.ds = amd.SegDs; regs.es = amd.SegEs;
    regs.fs = amd.SegFs; regs.gs = amd.SegGs;
    regs.fctrl = amd.FltSave.ControlWord;
    regs.fstat = amd.FltSave.StatusWord;
    regs.ftag  = amd.FltSave.TagWord;
    regs.fop   = amd.FltSave.ErrorOpcode;
    regs.fioff = amd.FltSave.ErrorOffset;
    regs.fiseg = amd.FltSave.ErrorSelector;
    regs.fooff = amd.FltSave.DataOffset;
    regs.foseg = amd.FltSave.DataSelector;
    regs.mxcsr = amd.FltSave.MxCsr;
    for (int i = 0; i < 8; i++) {
        memcpy(regs.st + i * 10, &amd.FltSave.FloatRegisters[i], 10);
    }
    for (int i = 0; i < 16; i++) {
        memcpy(regs.xmm + i * 16, &amd.FltSave.XmmRegisters[i], 16);
    }
    return regs;
}

uint64_t ttd_cursor_get_pc(TtdCursor* cur) {
    if (!cur) return 0;
    return static_cast<uint64_t>(cur->cursor->GetProgramCounter());
}

uint64_t ttd_cursor_get_sp(TtdCursor* cur) {
    if (!cur) return 0;
    return static_cast<uint64_t>(cur->cursor->GetStackPointer());
}

uint32_t ttd_cursor_get_current_tid(TtdCursor* cur) {
    if (!cur) return 0;
    auto& ti = cur->cursor->GetThreadInfo();
    return static_cast<uint32_t>(ti.Id);
}

/* ---- Thread info at cursor ---- */

uint32_t ttd_cursor_thread_count(TtdCursor* cur) {
    if (!cur) return 0;
    return static_cast<uint32_t>(cur->cursor->GetThreadCount());
}

TtdActiveThreadInfo ttd_cursor_active_thread_info(TtdCursor* cur, uint32_t thread_idx) {
    TtdActiveThreadInfo result = {};
    if (!cur) return result;
    auto* list = cur->cursor->GetThreadList();
    auto count = cur->cursor->GetThreadCount();
    if (!list || thread_idx >= count) return result;
    auto& ati = list[thread_idx];
    if (ati.pThread) {
        result.thread.unique_id    = static_cast<uint32_t>(ati.pThread->UniqueId);
        result.thread.os_thread_id = static_cast<uint32_t>(ati.pThread->Id);
        result.thread.lifetime     = from_cpp_range(ati.pThread->Lifetime);
        result.thread.active_time  = from_cpp_range(ati.pThread->ActiveTime);
    }
    result.current_position = from_cpp_pos(ati.CurrentPosition);
    return result;
}

TtdPosition ttd_cursor_get_thread_pos(TtdCursor* cur, uint32_t thread_idx) {
    if (!cur) return {0, 0};
    auto* list = cur->cursor->GetThreadList();
    auto count = cur->cursor->GetThreadCount();
    if (!list || thread_idx >= count) return {0, 0};
    return from_cpp_pos(list[thread_idx].CurrentPosition);
}

/* ---- Per-thread queries ---- */

static ThreadId to_thread_id(uint32_t tid) {
    return tid == 0 ? ThreadId::Invalid : static_cast<ThreadId>(tid);
}

TtdX64Regs ttd_cursor_read_regs_thread(TtdCursor* cur, uint32_t thread_id) {
    TtdX64Regs regs = {};
    if (!cur) return regs;
    auto rc = cur->cursor->GetCrossPlatformContext(to_thread_id(thread_id));
    CROSS_PLATFORM_CONTEXT cpc = rc;
    auto& amd = cpc.Amd64Context;
    regs.rax = amd.Rax;  regs.rcx = amd.Rcx;  regs.rdx = amd.Rdx;  regs.rbx = amd.Rbx;
    regs.rsp = amd.Rsp;  regs.rbp = amd.Rbp;  regs.rsi = amd.Rsi;  regs.rdi = amd.Rdi;
    regs.r8  = amd.R8;   regs.r9  = amd.R9;   regs.r10 = amd.R10;  regs.r11 = amd.R11;
    regs.r12 = amd.R12;  regs.r13 = amd.R13;  regs.r14 = amd.R14;  regs.r15 = amd.R15;
    regs.rip = amd.Rip;
    regs.eflags = amd.EFlags;
    regs.cs = amd.SegCs; regs.ss = amd.SegSs;
    regs.ds = amd.SegDs; regs.es = amd.SegEs;
    regs.fs = amd.SegFs; regs.gs = amd.SegGs;
    regs.fctrl = amd.FltSave.ControlWord;
    regs.fstat = amd.FltSave.StatusWord;
    regs.ftag  = amd.FltSave.TagWord;
    regs.fop   = amd.FltSave.ErrorOpcode;
    regs.fioff = amd.FltSave.ErrorOffset;
    regs.fiseg = amd.FltSave.ErrorSelector;
    regs.fooff = amd.FltSave.DataOffset;
    regs.foseg = amd.FltSave.DataSelector;
    regs.mxcsr = amd.FltSave.MxCsr;
    for (int i = 0; i < 8; i++) {
        memcpy(regs.st + i * 10, &amd.FltSave.FloatRegisters[i], 10);
    }
    for (int i = 0; i < 16; i++) {
        memcpy(regs.xmm + i * 16, &amd.FltSave.XmmRegisters[i], 16);
    }
    return regs;
}

uint64_t ttd_cursor_get_pc_thread(TtdCursor* cur, uint32_t thread_id) {
    if (!cur) return 0;
    return static_cast<uint64_t>(cur->cursor->GetProgramCounter(to_thread_id(thread_id)));
}

uint64_t ttd_cursor_get_sp_thread(TtdCursor* cur, uint32_t thread_id) {
    if (!cur) return 0;
    return static_cast<uint64_t>(cur->cursor->GetStackPointer(to_thread_id(thread_id)));
}

TtdPosition ttd_cursor_get_pos_thread(TtdCursor* cur, uint32_t thread_id) {
    if (!cur) return {0, 0};
    auto& pos = cur->cursor->GetPosition(to_thread_id(thread_id));
    return from_cpp_pos(pos);
}

uint64_t ttd_cursor_get_teb_thread(TtdCursor* cur, uint32_t thread_id) {
    if (!cur) return 0;
    return static_cast<uint64_t>(cur->cursor->GetTebAddress(to_thread_id(thread_id)));
}

/* ---- Exception events from engine ---- */

uint32_t ttd_engine_exception_count(TtdEngine* eng) {
    if (!eng) return 0;
    return static_cast<uint32_t>(eng->engine->GetExceptionEventCount());
}

TtdExceptionEvent ttd_engine_exception_event(TtdEngine* eng, uint32_t idx) {
    TtdExceptionEvent ev = {};
    if (!eng) return ev;
    auto* list = eng->engine->GetExceptionEventList();
    auto count = eng->engine->GetExceptionEventCount();
    if (!list || idx >= count) return ev;
    ev.code              = list[idx].Code;
    ev.flags             = list[idx].Flags;
    ev.exception_address = static_cast<uint64_t>(list[idx].RecordAddress);
    ev.program_counter   = static_cast<uint64_t>(list[idx].ProgramCounter);
    ev.position          = from_cpp_pos(list[idx].Position);
    return ev;
}

/* ---- Module info ---- */

uint32_t ttd_engine_module_count(TtdEngine* eng) {
    if (!eng) return 0;
    return static_cast<uint32_t>(eng->engine->GetModuleCount());
}

void ttd_engine_module_info(TtdEngine* eng, uint32_t idx,
                            uint64_t* out_addr, uint64_t* out_size,
                            uint16_t* out_name_buf, uint32_t name_buf_len)
{
    if (!eng || !out_addr || !out_size) return;
    *out_addr = 0;
    *out_size = 0;
    auto* list = eng->engine->GetModuleList();
    auto count = eng->engine->GetModuleCount();
    if (!list || idx >= count) return;
    auto& mod = list[idx];
    *out_addr = static_cast<uint64_t>(mod.Address);
    *out_size = mod.Size;
    if (out_name_buf && name_buf_len > 0 && mod.pName) {
        uint32_t copy_len = (mod.NameLength < name_buf_len) ? static_cast<uint32_t>(mod.NameLength) : name_buf_len - 1;
        memcpy(out_name_buf, mod.pName, copy_len * sizeof(uint16_t));
        out_name_buf[copy_len] = 0;
    }
}

/* ---- Watchpoints ---- */

int ttd_cursor_add_watchpoint(TtdCursor* cur, uint64_t addr, uint64_t size, uint8_t access_mask, uint32_t thread_id) {
    if (!cur) return -1;
    MemoryWatchpointData wp;
    wp.Address   = GuestAddress(addr);
    wp.Size      = size;
    wp.AccessMask = static_cast<DataAccessMask>(access_mask);
    wp.ThreadId  = (thread_id == 0) ? UniqueThreadId::Invalid : static_cast<UniqueThreadId>(thread_id);
    return cur->cursor->AddMemoryWatchpoint(wp) ? 0 : -2;
}

int ttd_cursor_remove_watchpoint(TtdCursor* cur, uint64_t addr, uint64_t size, uint8_t access_mask, uint32_t thread_id) {
    if (!cur) return -1;
    MemoryWatchpointData wp;
    wp.Address   = GuestAddress(addr);
    wp.Size      = size;
    wp.AccessMask = static_cast<DataAccessMask>(access_mask);
    wp.ThreadId  = (thread_id == 0) ? UniqueThreadId::Invalid : static_cast<UniqueThreadId>(thread_id);
    return cur->cursor->RemoveMemoryWatchpoint(wp) ? 0 : -2;
}

/* ---- Callbacks ---- */

void ttd_cursor_set_watchpoint_cb(TtdCursor* cur, TtdWatchpointCb cb, void* ctx) {
    if (!cur) return;
    cur->wp_cb  = cb;
    cur->wp_ctx = ctx;
    if (cb) {
        cur->cursor->SetMemoryWatchpointCallback(&WatchpointCbShim, reinterpret_cast<uintptr_t>(cur));
    } else {
        cur->cursor->SetMemoryWatchpointCallback(nullptr, 0);
    }
}

void ttd_cursor_set_progress_cb(TtdCursor* cur, TtdProgressCb cb, void* ctx) {
    if (!cur) return;
    cur->prog_cb  = cb;
    cur->prog_ctx = ctx;
    if (cb) {
        cur->cursor->SetReplayProgressCallback(&ProgressCbShim, reinterpret_cast<uintptr_t>(cur));
    } else {
        cur->cursor->SetReplayProgressCallback(nullptr, 0);
    }
}

/* ---- Interrupt ---- */

void ttd_cursor_interrupt(TtdCursor* cur) {
    if (cur) cur->cursor->InterruptReplay();
}

/* ---- Event masks ---- */

void ttd_cursor_set_event_mask(TtdCursor* cur, uint32_t mask) {
    if (cur) cur->cursor->SetEventMask(static_cast<EventMask>(mask));
}

void ttd_cursor_set_exception_mask(TtdCursor* cur, uint32_t mask) {
    if (cur) cur->cursor->SetExceptionMask(static_cast<ExceptionMask>(mask));
}

void ttd_cursor_set_memory_policy(TtdCursor* cur, uint32_t policy) {
    if (cur) cur->cursor->SetDefaultMemoryPolicy(static_cast<QueryMemoryPolicy>(policy));
}
