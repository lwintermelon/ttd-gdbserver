//! The debug target layer — the API consumed by protocol frontends.
//!
//! [`DebugTarget`] is the instruction-level debug API implemented by
//! [`TtdProcess`] and consumed by protocol frontends (currently the GDB RSP
//! server in `src/gdb/`): execution control (forward/reverse), memory reads,
//! breakpoints, thread/register access, trace position navigation.
//!
//! No source/line concepts, no expression evaluation, no symbol browsing.
//! The target is read-only (no memory/register writes).

pub mod breakpoint;
pub mod error;
pub mod process;
pub mod stop_reason;

pub use breakpoint::{BreakpointId, BreakpointKind};
pub use error::DebugError;
pub use process::TtdProcess;
pub use stop_reason::StopReason;

use crate::ttd::types::{TtdPosition, TtdX64Regs};

/// A loaded module (DLL/EXE) in the trace.
#[derive(Clone, Debug)]
pub struct ModuleInfo {
    pub base_addr: u64,
    pub size: u64,
    pub name: String,
}

/// Per-thread diagnostic info served by [`DebugTarget::thread_info`].
///
/// `unique_id` is TTD's internal id, stable for the whole trace (the OS
/// thread id may be recycled). `current_position` is the thread's trace
/// position at the current cursor — useful for `info threads` output, which
/// shows the user where each thread is in the recording.
#[derive(Clone, Debug)]
pub struct ThreadExtraInfoData {
    pub unique_id: u32,
    pub current_position: TtdPosition,
    pub teb: u64,
}

/// Trace-level diagnostics served by [`DebugTarget::diagnostics`].
///
/// Used by the `monitor ttd …` custom commands. The shape is intentionally
/// flat: each field is a primitive or a small struct so the wire format
/// is straightforward to render.
#[derive(Clone, Debug)]
pub struct TargetDiagnostics {
    /// (sequence, steps) of the first recorded position.
    pub first_position: TtdPosition,
    /// (sequence, steps) of the last recorded position.
    pub last_position: TtdPosition,
    /// Current cursor position.
    pub current_position: TtdPosition,
    /// All threads that ever existed in the trace, in TTD's order.
    pub threads: Vec<ThreadDiagnostics>,
    /// Total exception events recorded by the trace.
    pub exception_count: u32,
    /// Total number of loaded modules.
    pub module_count: u32,
}

#[derive(Clone, Debug)]
pub struct ThreadDiagnostics {
    pub unique_id: u32,
    pub os_thread_id: u32,
    /// Active time range; `min` is the position the thread became live,
    /// `max` is where it stopped running.
    pub active_time: (TtdPosition, TtdPosition),
}

/// Instruction-level debug API.
///
/// Frontends hold a concrete target (e.g. `TtdProcess`) behind this trait.
/// Construction is NOT part of the trait — frontends create the concrete
/// type directly (e.g. `TtdProcess::open()`).
pub trait DebugTarget {
    // ─── Execution control ─────────────────────────────────

    /// Single step forward one instruction.
    fn step(&mut self) -> Result<StopReason, DebugError>;

    /// Single step backward one instruction.
    fn step_back(&mut self) -> Result<StopReason, DebugError>;

    /// Continue forward until breakpoint, exception, or trace end.
    fn continue_forward(&mut self) -> Result<StopReason, DebugError>;

    /// Continue backward until breakpoint or trace start.
    fn continue_backward(&mut self) -> Result<StopReason, DebugError>;

    /// Jump to an arbitrary trace position.
    fn goto(&mut self, pos: TtdPosition) -> Result<StopReason, DebugError>;

    // ─── Memory ────────────────────────────────────────────

    /// Read memory into `buf`. Returns number of bytes actually read.
    fn read_memory(&self, addr: u64, buf: &mut [u8]) -> usize;

    // ─── Breakpoints ───────────────────────────────────────

    /// Set an execution breakpoint at `addr`. Returns breakpoint ID.
    /// `thread_id` limits the breakpoint to a specific thread; `None` = any.
    fn set_breakpoint(&mut self, addr: u64, thread_id: Option<u64>) -> u64;

    /// Set a data watchpoint. Returns breakpoint ID.
    fn set_data_breakpoint(&mut self, addr: u64, size: u64, kind: BreakpointKind) -> u64;

    /// Remove a breakpoint by ID. Returns true if found.
    fn remove_breakpoint(&mut self, id: u64) -> bool;

    // ─── Threads ───────────────────────────────────────────

    /// IDs (OS thread IDs) of the threads active at the current position.
    fn active_thread_ids(&self) -> Vec<u64>;

    /// Per-thread diagnostic info: TTD's internal `UniqueId`, the trace
    /// position the thread has reached at the current cursor, and the
    /// thread's TEB. Returns `None` when the thread is unknown.
    ///
    /// Used by `qThreadExtraInfo` and the `monitor ttd threads` custom
    /// command to give the user a stable identifier for the thread (the
    /// OS thread id may be recycled across the trace lifetime; the
    /// UniqueId is unique for the whole trace).
    fn thread_info(&self, thread_id: u64) -> Option<ThreadExtraInfoData>;

    /// Registers + TEB for one thread. `None` selects the current thread.
    /// Returns `(registers, teb)`; the TEB is the GS base on Windows x64,
    /// 0 when unavailable.
    fn thread_state(&self, thread_id: Option<u64>) -> Option<(TtdX64Regs, u64)>;

    /// Current thread ID.
    fn current_thread_id(&self) -> Option<u64>;

    /// Set current thread.
    fn set_current_thread(&mut self, id: u64);

    // ─── Modules ───────────────────────────────────────────

    /// List loaded modules (DLLs/EXEs) in the trace.
    fn modules(&self) -> Vec<ModuleInfo>;

    // ─── Diagnostics (TTD-specific introspection) ──────────

    /// Snapshot of trace-level information used by the `monitor ttd …`
    /// custom commands and the `qTTD…` custom packets. Cheap to compute;
    /// the underlying engine APIs are O(1) for counts and O(n) for the
    /// thread list.
    fn diagnostics(&self) -> TargetDiagnostics;

    // ─── Position ──────────────────────────────────────────

    /// Current trace position.
    fn position(&self) -> TtdPosition;

    /// Trace lifetime (first, last).
    fn lifetime(&self) -> (TtdPosition, TtdPosition);
}
