#include "ttd_cxx.hpp"

#include <cstdlib>
#include <cstring>
#include <limits>
#include <string>
#include <vector>
#include <windows.h>

#include "ttd-gdbserver/src/ttd/ffi.rs.h"

namespace {

Position
to_position(TTD::Replay::Position const& p)
{
    return Position{static_cast<uint64_t>(p.Sequence), static_cast<uint64_t>(p.Steps)};
}

TTD::Replay::Position
to_sdk_position(Position const& p)
{
    TTD::Replay::Position result{};
    result.Sequence = static_cast<TTD::SequenceId>(p.Sequence);
    result.Steps = static_cast<TTD::Replay::StepCount>(p.Steps);
    return result;
}

PositionRange
to_range(TTD::Replay::PositionRange const& r)
{
    return PositionRange{to_position(r.Min), to_position(r.Max)};
}

ThreadInfo
to_thread_info(TTD::Replay::ThreadInfo const& t)
{
    return ThreadInfo{static_cast<uint32_t>(t.UniqueId), static_cast<uint32_t>(t.Id),
                      to_range(t.Lifetime), to_range(t.ActiveTime)};
}

void
push_u64(rust::Vec<uint8_t>& out, uint64_t value)
{
    for (unsigned shift = 0; shift < 64; shift += 8)
    {
        out.push_back(static_cast<uint8_t>((value >> shift) & 0xff));
    }
}

void
push_m128(rust::Vec<uint8_t>& out, M128BIT const& value)
{
    push_u64(out, value.Low);
    push_u64(out, static_cast<uint64_t>(value.High));
}

std::string
wide_to_utf8(wchar_t const* text, size_t length)
{
    if (text == nullptr || length == 0)
    {
        return {};
    }
    int bytes = WideCharToMultiByte(CP_UTF8, 0, text, static_cast<int>(length), nullptr, 0,
                                    nullptr, nullptr);
    if (bytes <= 0)
    {
        return {};
    }
    std::string result(static_cast<size_t>(bytes), '\0');
    WideCharToMultiByte(CP_UTF8, 0, text, static_cast<int>(length), result.data(), bytes,
                        nullptr, nullptr);
    return result;
}

struct BuildContext
{
    rust::Fn<void(size_t, size_t, size_t)> callback;
    size_t context;
};

void __stdcall
on_build_progress(void const* context, TTD::Replay::IndexBuildProgressType const* progress)
{
    auto const* state = static_cast<BuildContext const*>(context);
    state->callback(state->context, static_cast<size_t>(progress->KeyframesProcessed),
                    static_cast<size_t>(progress->KeyframeCount));
}

} // namespace

Engine::Engine()
{
    auto [engine, error] = TTD::Replay::MakeReplayEngine();
    if (error == 0 && engine)
    {
        engine_ = std::move(engine);
    }
}

bool
Engine::IsValid() const
{
    return engine_ != nullptr;
}

bool
Engine::Load(std::string const& path)
{
    if (!engine_ || path.empty())
    {
        return false;
    }
    int length = MultiByteToWideChar(CP_UTF8, 0, path.data(), static_cast<int>(path.size()),
                                     nullptr, 0);
    if (length <= 0)
    {
        return false;
    }
    std::wstring wide(static_cast<size_t>(length), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, path.data(), static_cast<int>(path.size()), wide.data(),
                        length);
    return engine_->Initialize(wide.c_str());
}

uint8_t
Engine::Architecture() const
{
    if (!engine_)
    {
        return 0xff;
    }
    return static_cast<uint8_t>(engine_->GetSystemInfo().System.ProcessorArchitecture);
}

PositionRange
Engine::Lifetime() const
{
    return to_range(engine_->GetLifetime());
}

uint8_t
Engine::IndexStatus() const
{
    switch (engine_->GetIndexStatus())
    {
    case TTD::Replay::IndexStatus::IndexFileLoaded: return 0;
    case TTD::Replay::IndexStatus::IndexFileNotPresent: return 1;
    case TTD::Replay::IndexStatus::IndexFileUnloadable: return 2;
    default: return 0xff;
    }
}

uint8_t
Engine::BuildIndex(rust::Fn<void(size_t, size_t, size_t)> progress, size_t context)
{
    BuildContext state{progress, context};
    auto status = engine_->BuildIndex(&on_build_progress, &state,
                                      TTD::Replay::IndexBuildFlags::None);
    switch (status)
    {
    case TTD::Replay::IndexStatus::IndexFileLoaded: return 0;
    case TTD::Replay::IndexStatus::IndexFileNotPresent: return 1;
    case TTD::Replay::IndexStatus::IndexFileUnloadable: return 2;
    default: return 0xff;
    }
}

size_t
Engine::ThreadCount() const
{
    return engine_->GetThreadCount();
}

ThreadInfo
Engine::ThreadAt(size_t index) const
{
    return to_thread_info(engine_->GetThreadList()[index]);
}

size_t
Engine::ModuleCount() const
{
    return engine_->GetModuleCount();
}

ModuleInfo
Engine::ModuleAt(size_t index) const
{
    auto const& module = engine_->GetModuleList()[index];
    return ModuleInfo{static_cast<uint64_t>(module.Address), module.Size};
}

rust::String
Engine::ModuleName(size_t index) const
{
    auto const& module = engine_->GetModuleList()[index];
    return rust::String(wide_to_utf8(module.pName, module.NameLength));
}

size_t
Engine::ExceptionCount() const
{
    return engine_->GetExceptionEventCount();
}

ExceptionInfo
Engine::ExceptionAt(size_t index) const
{
    auto const& event = engine_->GetExceptionEventList()[index];
    return ExceptionInfo{to_position(event.Position), event.Code,
                         static_cast<uint64_t>(event.RecordAddress),
                         static_cast<uint64_t>(event.ProgramCounter)};
}

TTD::Replay::IReplayEngine&
Engine::Get() const
{
    return *const_cast<Engine*>(this)->engine_;
}

Cursor::Cursor(Engine const& engine)
{
    if (auto* raw = engine.Get().NewCursor())
    {
        cursor_ = TTD::Replay::UniqueCursor(raw);
        cursor_->SetDefaultMemoryPolicy(TTD::Replay::QueryMemoryPolicy::GloballyAggressive);
    }
}

bool
Cursor::IsValid() const
{
    return cursor_ != nullptr;
}

void
Cursor::SetPosition(Position position)
{
    cursor_->SetPosition(to_sdk_position(position));
}

Position
Cursor::GetPosition() const
{
    return to_position(cursor_->GetPosition());
}

ThreadInfo
Cursor::GetThreadInfo() const
{
    return to_thread_info(cursor_->GetThreadInfo());
}

uint64_t
Cursor::ProgramCounter() const
{
    return static_cast<uint64_t>(cursor_->GetProgramCounter());
}

uint64_t
Cursor::StackPointer() const
{
    return static_cast<uint64_t>(cursor_->GetStackPointer());
}

uint64_t
Cursor::TebAddress(uint32_t thread) const
{
    return static_cast<uint64_t>(cursor_->GetTebAddress(static_cast<TTD::ThreadId>(thread)));
}

size_t
Cursor::ActiveThreadCount() const
{
    return cursor_->GetThreadCount();
}

ActiveThread
Cursor::ActiveThreadAt(size_t index) const
{
    auto const& entry = cursor_->GetThreadList()[index];
    ThreadInfo info{};
    if (entry.pThread != nullptr)
    {
        info = to_thread_info(*entry.pThread);
    }
    return ActiveThread{info, to_position(entry.CurrentPosition)};
}

bool
Cursor::AddMemoryWatchpoint(uint64_t address, uint64_t size, uint8_t access_mask,
                            uint32_t thread)
{
    TTD::Replay::MemoryWatchpointData data{};
    data.Address = static_cast<TTD::GuestAddress>(address);
    data.Size = size;
    data.AccessMask = static_cast<TTD::Replay::DataAccessMask>(access_mask);
    data.ThreadId = static_cast<TTD::Replay::UniqueThreadId>(thread);
    return cursor_->AddMemoryWatchpoint(data);
}

bool
Cursor::RemoveMemoryWatchpoint(uint64_t address, uint64_t size, uint8_t access_mask,
                               uint32_t thread)
{
    TTD::Replay::MemoryWatchpointData data{};
    data.Address = static_cast<TTD::GuestAddress>(address);
    data.Size = size;
    data.AccessMask = static_cast<TTD::Replay::DataAccessMask>(access_mask);
    data.ThreadId = static_cast<TTD::Replay::UniqueThreadId>(thread);
    return cursor_->RemoveMemoryWatchpoint(data);
}

void
Cursor::SetWatchpointCallback(
    rust::Fn<bool(size_t, uint64_t, uint64_t, uint8_t, uint32_t)> callback, size_t context)
{
    watchpoint_callback_ = callback;
    watchpoint_context_ = context;
    cursor_->SetMemoryWatchpointCallback(&Cursor::OnWatchpoint,
                                         reinterpret_cast<uintptr_t>(this));
}

void
Cursor::ClearWatchpointCallback()
{
    cursor_->SetMemoryWatchpointCallback(nullptr, 0);
    watchpoint_callback_.reset();
    watchpoint_context_ = 0;
}

bool __fastcall
Cursor::OnWatchpoint(uintptr_t context,
                     TTD::Replay::ICursorView::MemoryWatchpointResult const& result,
                     TTD::Replay::IThreadView const* thread)
{
    auto* self = reinterpret_cast<Cursor*>(context);
    if (!self->watchpoint_callback_)
    {
        return true;
    }
    uint32_t thread_id = 0;
    if (thread != nullptr)
    {
        thread_id = static_cast<uint32_t>(thread->GetThreadInfo().Id);
    }
    return (*self->watchpoint_callback_)(self->watchpoint_context_,
                                         static_cast<uint64_t>(result.Address), result.Size,
                                         static_cast<uint8_t>(result.AccessType), thread_id);
}

void
Cursor::SetEventMask(uint32_t mask)
{
    cursor_->SetEventMask(static_cast<TTD::Replay::EventMask>(mask));
}

void
Cursor::SetExceptionMask(uint32_t mask)
{
    cursor_->SetExceptionMask(static_cast<TTD::Replay::ExceptionMask>(mask));
}

void
Cursor::InterruptReplay() const
{
    cursor_->InterruptReplay();
}

ReplayOutcome
Cursor::ReplayForward(Position limit, uint64_t steps)
{
    auto result = cursor_->ReplayForward(to_sdk_position(limit),
                                         TTD::Replay::StepCount{steps});
    ReplayOutcome outcome{};
    outcome.stop_reason = static_cast<uint8_t>(result.StopReason);
    outcome.steps = static_cast<uint64_t>(result.StepsExecuted);
    outcome.instructions = static_cast<uint64_t>(result.InstructionsExecuted);
    if (result.StopReason == TTD::Replay::EventType::MemoryWatchpoint)
    {
        outcome.wp_address = static_cast<uint64_t>(result.MemoryWatchpoint.Address);
        outcome.wp_size = result.MemoryWatchpoint.Size;
        outcome.wp_access = static_cast<uint8_t>(result.MemoryWatchpoint.AccessType);
    }
    return outcome;
}

ReplayOutcome
Cursor::ReplayBackward(Position limit, uint64_t steps)
{
    auto result = cursor_->ReplayBackward(to_sdk_position(limit),
                                         TTD::Replay::StepCount{steps});
    ReplayOutcome outcome{};
    outcome.stop_reason = static_cast<uint8_t>(result.StopReason);
    outcome.steps = static_cast<uint64_t>(result.StepsExecuted);
    outcome.instructions = static_cast<uint64_t>(result.InstructionsExecuted);
    if (result.StopReason == TTD::Replay::EventType::MemoryWatchpoint)
    {
        outcome.wp_address = static_cast<uint64_t>(result.MemoryWatchpoint.Address);
        outcome.wp_size = result.MemoryWatchpoint.Size;
        outcome.wp_access = static_cast<uint8_t>(result.MemoryWatchpoint.AccessType);
    }
    return outcome;
}

size_t
Cursor::QueryMemory(uint64_t address, rust::Slice<uint8_t> buffer) const
{
    auto result = cursor_->QueryMemoryBuffer(static_cast<TTD::GuestAddress>(address),
                                             TTD::BufferView{buffer.data(), buffer.size()});
    return result.Memory.IsValid() ? result.Memory.Size : 0;
}

MemoryResult
Cursor::QueryMemoryWithRanges(uint64_t address, rust::Slice<uint8_t> buffer,
                              rust::Slice<MemoryRange> ranges) const
{
    std::vector<TTD::Replay::MemoryRange> raw(ranges.size());
    auto result = cursor_->QueryMemoryBufferWithRanges(
        static_cast<TTD::GuestAddress>(address), TTD::BufferView{buffer.data(), buffer.size()},
        raw.size(), raw.data());
    size_t count = result.RangeCount < raw.size() ? result.RangeCount : raw.size();
    for (size_t i = 0; i < count; ++i)
    {
        ranges[i] = MemoryRange{static_cast<uint64_t>(raw[i].Address),
                                raw[i].Memory.IsValid() ? raw[i].Memory.Size : 0,
                                static_cast<uint64_t>(raw[i].Sequence)};
    }
    return MemoryResult{result.Memory.IsValid() ? result.Memory.Size : 0, count};
}

rust::Vec<uint8_t>
Cursor::GetAmd64Context(uint32_t thread) const
{
    auto context =
        cursor_->GetCrossPlatformContext(static_cast<TTD::ThreadId>(thread));
    auto const* regs = reinterpret_cast<AMD64_CONTEXT const*>(context.Data);
    rust::Vec<uint8_t> out;
    out.reserve(81 * 8);
    for (auto value : {regs->Rax, regs->Rbx, regs->Rcx, regs->Rdx, regs->Rsi, regs->Rdi,
                       regs->Rbp, regs->Rsp, regs->R8, regs->R9, regs->R10, regs->R11,
                       regs->R12, regs->R13, regs->R14, regs->R15, regs->Rip})
    {
        push_u64(out, value);
    }
    push_u64(out, regs->EFlags);
    for (auto value : {regs->SegCs, regs->SegDs, regs->SegEs, regs->SegFs, regs->SegGs,
                       regs->SegSs})
    {
        push_u64(out, value);
    }
    for (auto const& value : regs->FltSave.FloatRegisters)
    {
        push_m128(out, value);
    }
    for (auto const& value : regs->FltSave.XmmRegisters)
    {
        push_m128(out, value);
    }
    push_u64(out, regs->FltSave.ControlWord);
    push_u64(out, regs->FltSave.StatusWord);
    push_u64(out, regs->FltSave.TagWord);
    push_u64(out, regs->FltSave.ErrorOpcode);
    push_u64(out, regs->FltSave.ErrorOffset);
    push_u64(out, regs->FltSave.ErrorSelector);
    push_u64(out, regs->FltSave.DataOffset);
    push_u64(out, regs->FltSave.DataSelector);
    push_u64(out, regs->FltSave.MxCsr);
    return out;
}

std::unique_ptr<Engine>
NewEngine()
{
    return std::make_unique<Engine>();
}

std::unique_ptr<Cursor>
NewCursor(Engine const& engine)
{
    return std::make_unique<Cursor>(engine);
}
