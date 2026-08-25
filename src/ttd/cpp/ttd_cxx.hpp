#pragma once
// cxx bridge surface over TTD::Replay.
//
// The classes below own the SDK's RAII handles and convert its COM-shaped
// results into the plain structs the bridge can carry. Register state is
// serialized into a fixed little-endian word stream and decoded by the Rust
// side, so no SDK struct layout crosses the cxx boundary.

#include <cstddef>
#include <cstdint>
#include <memory>
#include <optional>
#include <string>

#include "TTD/IReplayEngine.h"
#include "TTD/IReplayEngineRegisters.h"
#include "TTD/IReplayEngineStl.h"
#include "rust/cxx.h"

// Defined by the cxx-generated header after it includes this one.
struct Position;
struct PositionRange;
struct ThreadInfo;
struct ModuleInfo;
struct ExceptionInfo;
struct ActiveThread;
struct MemoryRange;
struct ReplayOutcome;
struct MemoryResult;

class Engine {
public:
    Engine();
    ~Engine() = default;

    bool IsValid() const;

    bool Load(std::string const& path);

    uint8_t Architecture() const;
    PositionRange Lifetime() const;
    uint8_t IndexStatus() const;
    uint8_t BuildIndex(rust::Fn<void(size_t, size_t, size_t)> progress, size_t context);

    size_t ThreadCount() const;
    ThreadInfo ThreadAt(size_t index) const;

    size_t ModuleCount() const;
    ModuleInfo ModuleAt(size_t index) const;
    rust::String ModuleName(size_t index) const;

    size_t ExceptionCount() const;
    ExceptionInfo ExceptionAt(size_t index) const;

    TTD::Replay::IReplayEngine& Get() const;

private:
    TTD::Replay::UniqueReplayEngine engine_;
};

class Cursor {
public:
    explicit Cursor(Engine const& engine);
    ~Cursor() = default;

    bool IsValid() const;

    void SetPosition(Position position);
    Position GetPosition() const;
    ThreadInfo GetThreadInfo() const;
    uint64_t ProgramCounter() const;
    uint64_t StackPointer() const;
    uint64_t TebAddress(uint32_t thread) const;

    size_t ActiveThreadCount() const;
    ActiveThread ActiveThreadAt(size_t index) const;

    bool AddMemoryWatchpoint(uint64_t address, uint64_t size, uint8_t access_mask,
                             uint32_t thread);
    bool RemoveMemoryWatchpoint(uint64_t address, uint64_t size,
                                uint8_t access_mask, uint32_t thread);

    void SetWatchpointCallback(
        rust::Fn<bool(size_t, uint64_t, uint64_t, uint8_t, uint32_t)> callback,
        size_t context);
    void ClearWatchpointCallback();

    void SetEventMask(uint32_t mask);
    void SetExceptionMask(uint32_t mask);
    void InterruptReplay() const;

    ReplayOutcome ReplayForward(Position limit, uint64_t steps);
    ReplayOutcome ReplayBackward(Position limit, uint64_t steps);

    size_t QueryMemory(uint64_t address, rust::Slice<uint8_t> buffer) const;
    MemoryResult QueryMemoryWithRanges(uint64_t address,
                                       rust::Slice<uint8_t> buffer,
                                       rust::Slice<MemoryRange> ranges) const;

    rust::Vec<uint8_t> GetAmd64Context(uint32_t thread) const;

private:
    static bool __fastcall OnWatchpoint(
        uintptr_t context,
        TTD::Replay::ICursorView::MemoryWatchpointResult const& result,
        TTD::Replay::IThreadView const* thread);

    TTD::Replay::UniqueCursor cursor_;
    std::optional<rust::Fn<bool(size_t, uint64_t, uint64_t, uint8_t, uint32_t)>>
        watchpoint_callback_;
    size_t watchpoint_context_ = 0;
};

std::unique_ptr<Engine> NewEngine();
std::unique_ptr<Cursor> NewCursor(Engine const& engine);
