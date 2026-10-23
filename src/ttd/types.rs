//! TTD value types: the vocabulary the rest of the crate uses.
//!
//! Simple records are the cxx bridge's shared structs, re-exported here with
//! their methods and derives. Semantic shapes (`TtdReplayResult`, `EventType`,
//! `TtdX64Regs`, …) stay separate; register state is decoded from a fixed
//! little-endian word stream so no SDK layout crosses the bridge.

#![allow(non_snake_case)]

use bitflags::bitflags;

use crate::ttd::ffi;

pub use crate::ttd::ffi::{
    ExceptionInfo as TtdExceptionEvent, MemoryRange as TtdMemRange, Position as TtdPosition,
    PositionRange as TtdPositionRange, ThreadInfo as TtdThreadInfo,
};

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

impl TtdMemRange {
    /// Bytes held by this run of memory.
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
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

impl From<ffi::ReplayOutcome> for TtdReplayResult {
    fn from(value: ffi::ReplayOutcome) -> Self {
        Self {
            stop_reason: EventType::from(value.stop_reason),
            steps_executed: value.steps,
            instructions_executed: value.instructions,
            wp_address: value.wp_address,
            wp_size: value.wp_size,
            wp_access_type: DataAccessType::from(value.wp_access),
        }
    }
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

impl From<u8> for TtdIndexStatus {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Loaded,
            1 => Self::NotPresent,
            2 => Self::Unloadable,
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
            0 => Self::MemoryWatchpoint,
            1 => Self::PositionWatchpoint,
            2 => Self::Exception,
            3 => Self::Gap,
            4 => Self::Thread,
            5 => Self::StepCount,
            6 => Self::Position,
            7 => Self::Process,
            8 => Self::Interrupted,
            9 => Self::Error,
            10 => Self::Count,
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
            0 => Self::Read,
            1 => Self::Write,
            2 => Self::Execute,
            3 => Self::CodeFetch,
            4 => Self::Overwrite,
            5 => Self::DataMismatch,
            6 => Self::NewData,
            7 => Self::RedundantData,
            other => Self::Unknown(other),
        }
    }
}

bitflags! {
    /// The data accesses a watchpoint can observe, as the SDK's
    /// `DataAccessMask`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct DataAccessMask: u8 {
        const READ           = 1 << 0;
        const WRITE          = 1 << 1;
        const EXECUTE        = 1 << 2;
        const CODE_FETCH     = 1 << 3;
        const OVERWRITE      = 1 << 4;
        const DATA_MISMATCH  = 1 << 5;
        const NEW_DATA       = 1 << 6;
        const REDUNDANT_DATA = 1 << 7;
        const NONE           = 0;
        const READ_WRITE     = Self::READ.bits() | Self::WRITE.bits();
    }
}

bitflags! {
    /// The event kinds that stop a replay, as the SDK's `EventMask`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct EventMask: u32 {
        const MEMORY_WATCHPOINT   = 1 << 0;
        const POSITION_WATCHPOINT = 1 << 1;
        const EXCEPTION           = 1 << 2;
        const GAP                 = 1 << 3;
        const THREAD              = 1 << 4;
        const NONE                = 0;
        const ALL                 = Self::MEMORY_WATCHPOINT.bits()
                                  | Self::POSITION_WATCHPOINT.bits()
                                  | Self::EXCEPTION.bits()
                                  | Self::GAP.bits()
                                  | Self::THREAD.bits();
    }
}

bitflags! {
    /// The exception classes that stop a replay, as the SDK's
    /// `ExceptionMask`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ExceptionMask: u32 {
        const HARDWARE    = 1 << 0;
        const SOFTWARE    = 1 << 1;
        const CPLUS_PLUS  = 1 << 2;
        const DEBUG_PRINT = 1 << 3;
        const NONE        = 0;
        const ALL         = Self::HARDWARE.bits()
                          | Self::SOFTWARE.bits()
                          | Self::CPLUS_PLUS.bits()
                          | Self::DEBUG_PRINT.bits();
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct M128A {
    pub Low: u64,
    pub High: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct XmmSaveArea {
    pub ControlWord: u16,
    pub StatusWord: u16,
    pub TagWord: u8,
    pub ErrorOpcode: u16,
    pub ErrorOffset: u32,
    pub ErrorSelector: u16,
    pub DataOffset: u32,
    pub DataSelector: u16,
    pub MxCsr: u32,
    pub FloatRegisters: [M128A; 8],
    pub XmmRegisters: [M128A; 16],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TtdX64RegsAnon {
    pub FltSave: XmmSaveArea,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TtdX64Regs {
    pub Rax: u64,
    pub Rbx: u64,
    pub Rcx: u64,
    pub Rdx: u64,
    pub Rsi: u64,
    pub Rdi: u64,
    pub Rbp: u64,
    pub Rsp: u64,
    pub R8: u64,
    pub R9: u64,
    pub R10: u64,
    pub R11: u64,
    pub R12: u64,
    pub R13: u64,
    pub R14: u64,
    pub R15: u64,
    pub Rip: u64,
    pub EFlags: u32,
    pub SegCs: u16,
    pub SegDs: u16,
    pub SegEs: u16,
    pub SegFs: u16,
    pub SegGs: u16,
    pub SegSs: u16,
    pub fpu: TtdX64RegsAnon,
}

impl TtdX64Regs {
    /// The FPU/SSE half of this context.
    #[must_use]
    pub fn flt_save(&self) -> XmmSaveArea {
        self.fpu.FltSave
    }
}

/// Decode the 81-word little-endian stream produced by `GetAmd64Context`.
pub(crate) fn decode_amd64_context(bytes: &[u8]) -> Option<TtdX64Regs> {
    const WORDS: usize = 81;
    if bytes.len() != WORDS * 8 {
        return None;
    }
    let mut words = bytes.chunks(8).map(|chunk| {
        let mut word = [0u8; 8];
        word.copy_from_slice(chunk);
        u64::from_le_bytes(word)
    });
    let mut next = || words.next().unwrap_or(0);

    let mut regs = TtdX64Regs {
        Rax: next(),
        Rbx: next(),
        Rcx: next(),
        Rdx: next(),
        Rsi: next(),
        Rdi: next(),
        Rbp: next(),
        Rsp: next(),
        R8: next(),
        R9: next(),
        R10: next(),
        R11: next(),
        R12: next(),
        R13: next(),
        R14: next(),
        R15: next(),
        Rip: next(),
        ..TtdX64Regs::default()
    };
    regs.EFlags = u32::try_from(next()).ok()?;
    regs.SegCs = u16::try_from(next()).ok()?;
    regs.SegDs = u16::try_from(next()).ok()?;
    regs.SegEs = u16::try_from(next()).ok()?;
    regs.SegFs = u16::try_from(next()).ok()?;
    regs.SegGs = u16::try_from(next()).ok()?;
    regs.SegSs = u16::try_from(next()).ok()?;

    let mut flt = XmmSaveArea::default();
    for register in &mut flt.FloatRegisters {
        register.Low = next();
        register.High = next();
    }
    for register in &mut flt.XmmRegisters {
        register.Low = next();
        register.High = next();
    }
    flt.ControlWord = u16::try_from(next()).ok()?;
    flt.StatusWord = u16::try_from(next()).ok()?;
    flt.TagWord = u8::try_from(next()).ok()?;
    flt.ErrorOpcode = u16::try_from(next()).ok()?;
    flt.ErrorOffset = u32::try_from(next()).ok()?;
    flt.ErrorSelector = u16::try_from(next()).ok()?;
    flt.DataOffset = u32::try_from(next()).ok()?;
    flt.DataSelector = u16::try_from(next()).ok()?;
    flt.MxCsr = u32::try_from(next()).ok()?;
    regs.fpu.FltSave = flt;
    Some(regs)
}

/// Build a [`TtdMemRange`] with a size but no backing buffer.
#[cfg(test)]
#[must_use]
pub(crate) fn memory_range(address: u64, size: u64, sequence: u64) -> TtdMemRange {
    TtdMemRange {
        Address: address,
        size,
        Sequence: sequence,
    }
}
