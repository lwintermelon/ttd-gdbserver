#pragma once
// Thin C++ surface over TTD::Replay for bindgen.
//
// The classes below hold the SDK's own RAII handles and forward each call to
// the SDK one-to-one; the structs and enums in the signatures are the SDK's,
// so bindgen gives Rust their real layout and the Rust wrapper decides what
// the bytes mean (register context, access masks, ...).
//
// ABI note: MSVC passes a hidden return-buffer pointer in RCX for a member
// function returning a large struct, while Rust's `extern "C"` expects it in
// RCX *after* `this`. Such operations are declared as static members at the
// bottom of `Cursor`: a static member is a free function for ABI purposes and
// still reaches the private handle.

#include <cstddef>
#include <cstdint>

#include "TTD/IReplayEngine.h"
#include "TTD/IReplayEngineRegisters.h"
#include "TTD/IReplayEngineStl.h"

namespace ttd_sys
{

/// A loaded trace.
class Engine
{
private:
    friend class Cursor;

    TTD::Replay::UniqueReplayEngine m_Engine;

public:
    Engine();

    ~Engine();

    Engine(Engine const&) = delete;

    Engine&
    operator=(Engine const&) = delete;

    bool
    IsValid() const;

    /// Load a trace; 0 on success.
    int32_t
    Load(wchar_t const* path);

    TTD::SystemInfo const&
    GetSystemInfo() const;

    TTD::Replay::PositionRange const&
    GetLifetime() const;

    TTD::Replay::IndexStatus
    GetIndexStatus() const;

    /// Build the trace index, reporting keyframe progress to `cb`.
    TTD::Replay::IndexStatus
    BuildIndex(TTD::Replay::IndexBuildProgressCallback cb, void const* context);

    size_t
    GetThreadCount() const;

    TTD::Replay::ThreadInfo const*
    GetThreadList() const;

    size_t
    GetModuleCount() const;

    TTD::Replay::Module const*
    GetModuleList() const;

    size_t
    GetExceptionEventCount() const;

    TTD::Replay::ExceptionEvent const*
    GetExceptionEventList() const;
};

/// A position in a trace.
class Cursor
{
private:
    TTD::Replay::UniqueCursor m_Cursor;

public:
    Cursor(Engine& engine);

    ~Cursor();

    bool
    IsValid() const;

    Cursor(Cursor const&) = delete;

    Cursor&
    operator=(Cursor const&) = delete;

    void
    SetPosition(TTD::Replay::Position const& position);

    TTD::Replay::Position const&
    GetPosition() const;

    TTD::Replay::ThreadInfo const&
    GetThreadInfo() const;

    uint64_t
    GetProgramCounter() const;

    uint64_t
    GetStackPointer() const;

    uint64_t
    GetTebAddress(TTD::ThreadId thread) const;

    size_t
    GetThreadCount() const;

    TTD::Replay::ActiveThreadInfo const*
    GetThreadList() const;

    bool
    AddMemoryWatchpoint(TTD::Replay::MemoryWatchpointData const& watchpoint);

    bool
    RemoveMemoryWatchpoint(TTD::Replay::MemoryWatchpointData const& watchpoint);

    void
    SetMemoryWatchpointCallback(TTD::Replay::ICursorView::MemoryWatchpointCallback* cb,
                                uintptr_t context);

    void
    SetEventMask(TTD::Replay::EventMask mask);

    void
    SetExceptionMask(TTD::Replay::ExceptionMask mask);

    /// Callable from any thread, including while a replay is in flight.
    void
    InterruptReplay() const;

    // Static members, not instance methods: MSVC passes a large by-value
    // result of an instance method through a hidden pointer in RCX *after*
    // `this`, which Rust's C convention cannot describe. A static member is a
    // free function for ABI purposes, so these are callable from Rust while
    // still reaching the private handle.

    /// Replay forward, at most `steps` steps, stopping at `limit`.
    static TTD::Replay::ICursorView::ReplayResult
    ReplayForward(Cursor& cursor, TTD::Replay::Position const& limit, uint64_t steps);

    /// Replay backward, at most `steps` steps, stopping at `limit`.
    static TTD::Replay::ICursorView::ReplayResult
    ReplayBackward(Cursor& cursor, TTD::Replay::Position const& limit, uint64_t steps);

    /// Fill `buffer`, returning the bytes read (0 when nothing is available).
    static size_t
    QueryMemoryBuffer(Cursor const& cursor, TTD::GuestAddress address, void* buffer, size_t size);

    /// Fill `buffer` and the runs it came from, returning the bytes read. The
    /// number of runs written is reported through `range_count`.
    static size_t
    QueryMemoryBufferWithRanges(Cursor const& cursor, TTD::GuestAddress address, void* buffer,
                                size_t size, TTD::Replay::MemoryRange* ranges, size_t max_ranges,
                                size_t* range_count);

    /// The register context of `thread`; the caller interprets the bytes
    /// according to the trace's processor architecture.
    static TTD::Replay::RegisterContext
    GetRegisterContext(Cursor const& cursor, TTD::ThreadId thread);

    /// The extended (AVX) register context of `thread`.
    static TTD::Replay::ExtendedRegisterContext
    GetExtendedRegisterContext(Cursor const& cursor, TTD::ThreadId thread);
};

/// The view of a thread that a replay callback is given.
///
/// Non-owning by design, and deliberately without a destructor: the SDK owns
/// the interface and lends it to the callback only for that call (its own
/// `~IThreadView` is `protected`, so deleting through the interface is not even
/// possible), and this class holds nothing but the pointer. A destructor here
/// would suggest ownership and would give the Rust side a `destruct()` that
/// destroys an object it does not own.
class ThreadView
{
private:
    TTD::Replay::IThreadView const* m_View;

public:
    explicit ThreadView(TTD::Replay::IThreadView const* view);

    TTD::Replay::ThreadInfo const&
    GetThreadInfo() const;
};

} // namespace ttd_sys
