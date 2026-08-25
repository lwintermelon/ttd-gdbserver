#include "ttd_sys.hpp"

namespace ttd_sys
{

namespace
{
/// The SDK requires a non-null progress callback; this one ignores progress.
void __stdcall
ignore_progress(void const*, TTD::Replay::IndexBuildProgressType const*)
{
}
} // namespace

Engine::Engine()
{
    auto [engine, error] = TTD::Replay::MakeReplayEngine();
    if ( error == 0 && engine )
    {
        m_Engine = std::move(engine);
    }
}

Engine::~Engine() = default;

bool
Engine::IsValid() const
{
    return m_Engine != nullptr;
}

int32_t
Engine::Load(wchar_t const* path)
{
    if ( !m_Engine || !path )
    {
        return -1;
    }

    return m_Engine->Initialize(path) ? 0 : -2;
}

TTD::SystemInfo const&
Engine::GetSystemInfo() const
{
    return m_Engine->GetSystemInfo();
}

TTD::Replay::PositionRange const&
Engine::GetLifetime() const
{
    return m_Engine->GetLifetime();
}

TTD::Replay::IndexStatus
Engine::GetIndexStatus() const
{
    return m_Engine->GetIndexStatus();
}

TTD::Replay::IndexStatus
Engine::BuildIndex(TTD::Replay::IndexBuildProgressCallback cb, void const* context)
{
    return m_Engine->BuildIndex(cb ? cb : ignore_progress, context, TTD::Replay::IndexBuildFlags::None);
}

size_t
Engine::GetThreadCount() const
{
    return m_Engine->GetThreadCount();
}

TTD::Replay::ThreadInfo const*
Engine::GetThreadList() const
{
    return m_Engine->GetThreadList();
}

size_t
Engine::GetModuleCount() const
{
    return m_Engine->GetModuleCount();
}

TTD::Replay::Module const*
Engine::GetModuleList() const
{
    return m_Engine->GetModuleList();
}

size_t
Engine::GetExceptionEventCount() const
{
    return m_Engine->GetExceptionEventCount();
}

TTD::Replay::ExceptionEvent const*
Engine::GetExceptionEventList() const
{
    return m_Engine->GetExceptionEventList();
}

Cursor::Cursor(Engine& engine)
{
    if ( engine.m_Engine )
    {
        if ( auto* raw = engine.m_Engine->NewCursor() )
        {
            m_Cursor = TTD::Replay::UniqueCursor(raw);
        }
    }

    if ( m_Cursor )
    {
        // A debugger reads the whole address space at the cursor's position,
        // not just what the current thread's fragment makes cheap to find.
        m_Cursor->SetDefaultMemoryPolicy(TTD::Replay::QueryMemoryPolicy::GloballyAggressive);
    }
}

Cursor::~Cursor() = default;

bool
Cursor::IsValid() const
{
    return m_Cursor != nullptr;
}

void
Cursor::SetPosition(TTD::Replay::Position const& position)
{
    m_Cursor->SetPosition(position);
}

TTD::Replay::Position const&
Cursor::GetPosition() const
{
    return m_Cursor->GetPosition();
}

TTD::Replay::ThreadInfo const&
Cursor::GetThreadInfo() const
{
    return m_Cursor->GetThreadInfo();
}

uint64_t
Cursor::GetProgramCounter() const
{
    return static_cast<uint64_t>(m_Cursor->GetProgramCounter());
}

uint64_t
Cursor::GetStackPointer() const
{
    return static_cast<uint64_t>(m_Cursor->GetStackPointer());
}

uint64_t
Cursor::GetTebAddress(TTD::ThreadId thread) const
{
    return static_cast<uint64_t>(m_Cursor->GetTebAddress(thread));
}

size_t
Cursor::GetThreadCount() const
{
    return m_Cursor->GetThreadCount();
}

TTD::Replay::ActiveThreadInfo const*
Cursor::GetThreadList() const
{
    return m_Cursor->GetThreadList();
}

bool
Cursor::AddMemoryWatchpoint(TTD::Replay::MemoryWatchpointData const& watchpoint)
{
    return m_Cursor->AddMemoryWatchpoint(watchpoint);
}

bool
Cursor::RemoveMemoryWatchpoint(TTD::Replay::MemoryWatchpointData const& watchpoint)
{
    return m_Cursor->RemoveMemoryWatchpoint(watchpoint);
}

void
Cursor::SetMemoryWatchpointCallback(TTD::Replay::ICursorView::MemoryWatchpointCallback* cb,
                                    uintptr_t context)
{
    m_Cursor->SetMemoryWatchpointCallback(cb, context);
}

void
Cursor::SetEventMask(TTD::Replay::EventMask mask)
{
    m_Cursor->SetEventMask(mask);
}

void
Cursor::SetExceptionMask(TTD::Replay::ExceptionMask mask)
{
    m_Cursor->SetExceptionMask(mask);
}

void
Cursor::InterruptReplay() const
{
    m_Cursor->InterruptReplay();
}

TTD::Replay::ICursorView::ReplayResult
Cursor::ReplayForward(Cursor& cursor, TTD::Replay::Position const& limit, uint64_t steps)
{
    return cursor.m_Cursor->ReplayForward(limit, TTD::Replay::StepCount {steps});
}

TTD::Replay::ICursorView::ReplayResult
Cursor::ReplayBackward(Cursor& cursor, TTD::Replay::Position const& limit, uint64_t steps)
{
    return cursor.m_Cursor->ReplayBackward(limit, TTD::Replay::StepCount {steps});
}

size_t
Cursor::QueryMemoryBuffer(Cursor const& cursor, TTD::GuestAddress address, void* buffer, size_t size)
{
    const auto result =
        cursor.m_Cursor->QueryMemoryBuffer(address, TTD::BufferView {buffer, size});
    return result.Memory.IsValid() ? result.Memory.Size : 0;
}

size_t
Cursor::QueryMemoryBufferWithRanges(Cursor const& cursor, TTD::GuestAddress address, void* buffer,
                                    size_t size, TTD::Replay::MemoryRange* ranges,
                                    size_t max_ranges, size_t* range_count)
{
    const auto result = cursor.m_Cursor->QueryMemoryBufferWithRanges(
        address, TTD::BufferView {buffer, size}, max_ranges, ranges);

    if ( range_count )
    {
        *range_count = result.RangeCount < max_ranges ? result.RangeCount : max_ranges;
    }

    return result.Memory.IsValid() ? result.Memory.Size : 0;
}

TTD::Replay::RegisterContext
Cursor::GetRegisterContext(Cursor const& cursor, TTD::ThreadId thread)
{
    return cursor.m_Cursor->GetCrossPlatformContext(thread);
}

TTD::Replay::ExtendedRegisterContext
Cursor::GetExtendedRegisterContext(Cursor const& cursor, TTD::ThreadId thread)
{
    return cursor.m_Cursor->GetAvxExtendedContext(thread);
}

ThreadView::ThreadView(TTD::Replay::IThreadView const* view)
    : m_View {view}
{
}

TTD::Replay::ThreadInfo const&
ThreadView::GetThreadInfo() const
{
    return m_View->GetThreadInfo();
}

} // namespace ttd_sys
