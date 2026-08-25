//! TTD value types: the vocabulary the rest of the crate uses.
//!
//! Most are the SDK's own structs under the crate's names. The two that are
//! not — [`TtdReplayResult`] and [`TtdExceptionEvent`] — are the flat shapes
//! the target layer consumes, derived once from the SDK's raw records.

use bitflags::bitflags;

use crate::ttd_sys as sys;

pub type TtdPosition = sys::TTD_Replay_Position;
pub type TtdPositionRange = sys::TTD_Replay_PositionRange;
pub type TtdThreadInfo = sys::TTD_Replay_ThreadInfo;
pub type TtdMemRange = sys::TTD_Replay_MemoryRange;
pub type TtdX64Regs = sys::AMD64_CONTEXT;
pub type XmmSaveArea = sys::AMD64_XMM_SAVE_AREA32;

impl TtdPosition {
    /// Build a position from its two coordinates.
    #[must_use]
    pub const fn new(sequence: u64, steps: u64) -> Self {
        Self {
            Sequence: sequence,
            Steps: steps,
        }
    }

    /// The sequence id: the position's fragment.
    #[must_use]
    pub const fn sequence(self) -> u64 {
        self.Sequence
    }

    /// The step count within the fragment.
    #[must_use]
    pub const fn steps(self) -> u64 {
        self.Steps
    }
}

impl std::fmt::Display for TtdPosition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:x}:{:x}", self.Sequence, self.Steps)
    }
}

impl Ord for TtdPosition {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.Sequence, self.Steps).cmp(&(other.Sequence, other.Steps))
    }
}

impl PartialOrd for TtdPosition {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// The outcome of one replay call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TtdReplayResult {
    pub stop_reason: EventType,
    pub steps_executed: u64,
    pub instructions_executed: u64,
    /// The watched access that stopped the replay; meaningless unless
    /// `stop_reason` is [`EventType::MemoryWatchpoint`].
    pub wp_address: u64,
    pub wp_size: u64,
    pub wp_access_type: DataAccessType,
}

impl From<sys::TTD_Replay_ICursorView_ReplayResult> for TtdReplayResult {
    fn from(value: sys::TTD_Replay_ICursorView_ReplayResult) -> Self {
        let (wp_address, wp_size, wp_access_type) =
            if value.StopReason == sys::TTD_Replay_EventType_MemoryWatchpoint {
                // SAFETY: the union member is active exactly for this stop reason.
                let hit = unsafe { value.__bindgen_anon_1.MemoryWatchpoint };
                (hit.Address, hit.Size, DataAccessType::from(hit.AccessType))
            } else {
                (0, 0, DataAccessType::Read)
            };

        Self {
            stop_reason: EventType::from(value.StopReason),
            steps_executed: value.StepsExecuted,
            instructions_executed: value.InstructionsExecuted,
            wp_address,
            wp_size,
            wp_access_type,
        }
    }
}

/// One exception event recorded in the trace.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TtdExceptionEvent {
    pub position: TtdPosition,
    pub code: u32,
    pub record_address: u64,
    pub program_counter: u64,
}

/// A module as the trace recorded it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayModule {
    pub name: String,
    pub address: u64,
    pub size: u64,
}

/// The state of the trace's index file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TtdIndexStatus {
    /// The index exists and matches the trace.
    Loaded,
    /// No index file: memory reads are inaccurate until one is built.
    NotPresent,
    /// An index exists but cannot be loaded (corrupt or built for another trace).
    Unloadable,
    /// The engine answered outside the documented states.
    #[default]
    Unknown,
}

impl From<sys::TTD_Replay_IndexStatus> for TtdIndexStatus {
    fn from(value: sys::TTD_Replay_IndexStatus) -> Self {
        match value {
            sys::TTD_Replay_IndexStatus_IndexFileLoaded => Self::Loaded,
            sys::TTD_Replay_IndexStatus_IndexFileNotPresent => Self::NotPresent,
            sys::TTD_Replay_IndexStatus_IndexFileUnloadable => Self::Unloadable,
            _ => Self::Unknown,
        }
    }
}

/// What stopped a replay, as the SDK's `EventType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventType {
    MemoryWatchpoint,
    PositionWatchpoint,
    Exception,
    Gap,
    Thread,
    StepCount,
    Position,
    Process,
    Interrupted,
    /// The engine itself failed; the replay state is not trustworthy.
    Error,
    /// The SDK's boundary sentinel, never reported by a replay call.
    Count,
    /// A value outside the documented set.
    Unknown(u8),
}

impl From<u8> for EventType {
    fn from(value: u8) -> Self {
        match value {
            sys::TTD_Replay_EventType_MemoryWatchpoint => Self::MemoryWatchpoint,
            sys::TTD_Replay_EventType_PositionWatchpoint => Self::PositionWatchpoint,
            sys::TTD_Replay_EventType_Exception => Self::Exception,
            sys::TTD_Replay_EventType_Gap => Self::Gap,
            sys::TTD_Replay_EventType_Thread => Self::Thread,
            sys::TTD_Replay_EventType_StepCount => Self::StepCount,
            sys::TTD_Replay_EventType_Position => Self::Position,
            sys::TTD_Replay_EventType_Process => Self::Process,
            sys::TTD_Replay_EventType_Interrupted => Self::Interrupted,
            sys::TTD_Replay_EventType_Error => Self::Error,
            sys::TTD_Replay_EventType_Count => Self::Count,
            other => Self::Unknown(other),
        }
    }
}

impl std::fmt::Display for EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::MemoryWatchpoint => "memory watchpoint",
            Self::PositionWatchpoint => "position watchpoint",
            Self::Exception => "exception",
            Self::Gap => "gap",
            Self::Thread => "thread",
            Self::StepCount => "step count",
            Self::Position => "position",
            Self::Process => "process",
            Self::Interrupted => "interrupted",
            Self::Error => "error",
            Self::Count => "count",
            Self::Unknown(raw) => return write!(f, "unknown ({raw:#04x})"),
        };
        f.write_str(name)
    }
}

/// The access a watchpoint hit performed, as the SDK's `DataAccessType`.
///
/// An enum rather than a mask: the engine reports exactly one access kind per
/// hit, and the SDK numbers them (`Read = 0`, `Write = 1`, …) instead of
/// setting bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DataAccessType {
    #[default]
    Read,
    Write,
    Execute,
    CodeFetch,
    Overwrite,
    DataMismatch,
    NewData,
    RedundantData,
    /// A value outside the documented set.
    Unknown(u8),
}

impl DataAccessType {
    /// The single mask bit this access kind corresponds to.
    ///
    /// The engine reports a `DataAccessType` enum value while watchpoints are
    /// registered with a mask, so a hit has to be translated before it can be
    /// matched against a breakpoint.
    #[must_use]
    pub const fn mask(self) -> DataAccessMask {
        match self {
            Self::Read => DataAccessMask::READ,
            Self::Write => DataAccessMask::WRITE,
            Self::Execute => DataAccessMask::EXECUTE,
            Self::CodeFetch => DataAccessMask::CODE_FETCH,
            Self::Overwrite => DataAccessMask::OVERWRITE,
            Self::DataMismatch => DataAccessMask::DATA_MISMATCH,
            Self::NewData => DataAccessMask::NEW_DATA,
            Self::RedundantData => DataAccessMask::REDUNDANT_DATA,
            Self::Unknown(_) => DataAccessMask::NONE,
        }
    }
}

impl From<u8> for DataAccessType {
    fn from(value: u8) -> Self {
        match value {
            sys::TTD_Replay_DataAccessType_Read => Self::Read,
            sys::TTD_Replay_DataAccessType_Write => Self::Write,
            sys::TTD_Replay_DataAccessType_Execute => Self::Execute,
            sys::TTD_Replay_DataAccessType_CodeFetch => Self::CodeFetch,
            sys::TTD_Replay_DataAccessType_Overwrite => Self::Overwrite,
            sys::TTD_Replay_DataAccessType_DataMismatch => Self::DataMismatch,
            sys::TTD_Replay_DataAccessType_NewData => Self::NewData,
            sys::TTD_Replay_DataAccessType_RedundantData => Self::RedundantData,
            other => Self::Unknown(other),
        }
    }
}

bitflags! {
    /// The data accesses a watchpoint can observe, as the SDK's
    /// `DataAccessMask`.
    ///
    /// Only [`READ`](Self::READ), [`WRITE`](Self::WRITE) and
    /// [`EXECUTE`](Self::EXECUTE) are meaningful to `AddMemoryWatchpoint`; the
    /// rest exist because the SDK defines them and a hit can report one.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct DataAccessMask: u8 {
        const READ           = sys::TTD_Replay_DataAccessMask_Read;
        const WRITE          = sys::TTD_Replay_DataAccessMask_Write;
        const EXECUTE        = sys::TTD_Replay_DataAccessMask_Execute;
        const CODE_FETCH     = sys::TTD_Replay_DataAccessMask_CodeFetch;
        const OVERWRITE      = sys::TTD_Replay_DataAccessMask_Overwrite;
        const DATA_MISMATCH  = sys::TTD_Replay_DataAccessMask_DataMismatch;
        const NEW_DATA       = sys::TTD_Replay_DataAccessMask_NewData;
        const REDUNDANT_DATA = sys::TTD_Replay_DataAccessMask_RedundantData;
        const NONE           = sys::TTD_Replay_DataAccessMask_None;
        const READ_WRITE     = sys::TTD_Replay_DataAccessMask_ReadWrite;
    }
}

bitflags! {
    /// The event kinds that stop a replay, as the SDK's `EventMask`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct EventMask: u32 {
        const MEMORY_WATCHPOINT   = sys::TTD_Replay_EventMask_MemoryWatchpoint;
        const POSITION_WATCHPOINT = sys::TTD_Replay_EventMask_PositionWatchpoint;
        const EXCEPTION           = sys::TTD_Replay_EventMask_Exception;
        const GAP                 = sys::TTD_Replay_EventMask_Gap;
        const THREAD              = sys::TTD_Replay_EventMask_Thread;
        const NONE                = sys::TTD_Replay_EventMask_None;
        const ALL                 = sys::TTD_Replay_EventMask_All;
    }
}

bitflags! {
    /// The exception classes that stop a replay, as the SDK's
    /// `ExceptionMask`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ExceptionMask: u32 {
        const HARDWARE    = sys::TTD_Replay_ExceptionMask_Hardware;
        const SOFTWARE    = sys::TTD_Replay_ExceptionMask_Software;
        const CPLUS_PLUS  = sys::TTD_Replay_ExceptionMask_CPlusPlus;
        const DEBUG_PRINT = sys::TTD_Replay_ExceptionMask_DebugPrint;
        const NONE        = sys::TTD_Replay_ExceptionMask_None;
        const ALL         = sys::TTD_Replay_ExceptionMask_All;
    }
}

impl TtdMemRange {
    /// Bytes held by this run of memory.
    ///
    /// The SDK's `ConstBufferView` is a `{ pointer, size }` pair that bindgen
    /// can only see as opaque bytes, so the size is read from its second word.
    #[must_use]
    pub fn size(&self) -> u64 {
        // SAFETY: `ConstBufferView` is a `{ pointer, size }` pair.
        unsafe { *(&raw const self.Memory).cast::<u64>().add(1) }
    }
}

/// Build a [`TtdMemRange`] with a size but no backing buffer.
#[cfg(test)]
#[must_use]
pub(crate) fn memory_range(address: u64, size: u64, sequence: u64) -> TtdMemRange {
    let mut range = TtdMemRange {
        Address: address,
        Memory: crate::ttd_sys::TTD_ConstBufferView::default(),
        Sequence: sequence,
    };
    // SAFETY: `ConstBufferView` is a `{ pointer, size }` pair.
    unsafe {
        let words = (&raw mut range.Memory).cast::<u64>();
        *words.add(1) = size;
    }
    range
}

impl TtdX64Regs {
    /// The FPU/SSE half of this context.
    #[must_use]
    pub fn flt_save(&self) -> XmmSaveArea {
        // SAFETY: the floating-point member is the active interpretation.
        unsafe { self.__bindgen_anon_1.FltSave }
    }
}
