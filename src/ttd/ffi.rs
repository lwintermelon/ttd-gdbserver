#[cxx::bridge]
mod bridge {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
    struct Position {
        Sequence: u64,
        Steps: u64,
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
    struct PositionRange {
        Min: Position,
        Max: Position,
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    struct ThreadInfo {
        UniqueId: u32,
        Id: u32,
        Lifetime: PositionRange,
        ActiveTime: PositionRange,
    }

    struct ModuleInfo {
        address: u64,
        size: u64,
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    struct ExceptionInfo {
        position: Position,
        code: u32,
        record_address: u64,
        program_counter: u64,
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    struct ActiveThread {
        thread: ThreadInfo,
        current_position: Position,
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    struct MemoryRange {
        Address: u64,
        size: u64,
        Sequence: u64,
    }

    struct ReplayOutcome {
        stop_reason: u8,
        steps: u64,
        instructions: u64,
        wp_address: u64,
        wp_size: u64,
        wp_access: u8,
    }

    struct MemoryResult {
        read: u64,
        range_count: u64,
    }

    unsafe extern "C++" {
        include!("ttd_cxx.hpp");

        type Engine;
        type Cursor;

        fn NewEngine() -> UniquePtr<Engine>;
        fn IsValid(self: &Engine) -> bool;
        fn Load(self: Pin<&mut Engine>, path: &CxxString) -> bool;

        fn Architecture(self: &Engine) -> u8;
        fn Lifetime(self: &Engine) -> PositionRange;
        fn IndexStatus(self: &Engine) -> u8;
        fn BuildIndex(
            self: Pin<&mut Engine>,
            progress: fn(usize, usize, usize),
            context: usize,
        ) -> u8;

        fn ThreadCount(self: &Engine) -> usize;
        fn ThreadAt(self: &Engine, index: usize) -> ThreadInfo;

        fn ModuleCount(self: &Engine) -> usize;
        fn ModuleAt(self: &Engine, index: usize) -> ModuleInfo;
        fn ModuleName(self: &Engine, index: usize) -> String;

        fn ExceptionCount(self: &Engine) -> usize;
        fn ExceptionAt(self: &Engine, index: usize) -> ExceptionInfo;

        fn NewCursor(engine: &Engine) -> UniquePtr<Cursor>;
        fn IsValid(self: &Cursor) -> bool;

        fn SetPosition(self: Pin<&mut Cursor>, position: Position);
        fn GetPosition(self: &Cursor) -> Position;
        fn GetThreadInfo(self: &Cursor) -> ThreadInfo;
        fn ProgramCounter(self: &Cursor) -> u64;
        fn StackPointer(self: &Cursor) -> u64;
        fn TebAddress(self: &Cursor, thread: u32) -> u64;

        fn ActiveThreadCount(self: &Cursor) -> usize;
        fn ActiveThreadAt(self: &Cursor, index: usize) -> ActiveThread;

        fn AddMemoryWatchpoint(
            self: Pin<&mut Cursor>,
            address: u64,
            size: u64,
            access_mask: u8,
            thread: u32,
        ) -> bool;
        fn RemoveMemoryWatchpoint(
            self: Pin<&mut Cursor>,
            address: u64,
            size: u64,
            access_mask: u8,
            thread: u32,
        ) -> bool;

        fn SetWatchpointCallback(
            self: Pin<&mut Cursor>,
            callback: fn(usize, u64, u64, u8, u32) -> bool,
            context: usize,
        );
        fn ClearWatchpointCallback(self: Pin<&mut Cursor>);

        fn SetEventMask(self: Pin<&mut Cursor>, mask: u32);
        fn SetExceptionMask(self: Pin<&mut Cursor>, mask: u32);
        fn InterruptReplay(self: &Cursor);

        fn ReplayForward(self: Pin<&mut Cursor>, limit: Position, steps: u64) -> ReplayOutcome;
        fn ReplayBackward(self: Pin<&mut Cursor>, limit: Position, steps: u64) -> ReplayOutcome;

        fn QueryMemory(self: &Cursor, address: u64, buffer: &mut [u8]) -> usize;
        fn QueryMemoryWithRanges(
            self: &Cursor,
            address: u64,
            buffer: &mut [u8],
            ranges: &mut [MemoryRange],
        ) -> MemoryResult;

        fn GetAmd64Context(self: &Cursor, thread: u32) -> Vec<u8>;
    }
}

pub use bridge::*;
