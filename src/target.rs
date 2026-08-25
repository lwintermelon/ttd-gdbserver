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
pub(crate) mod breakpoint_state;
pub(crate) mod cursors;
pub mod diagnostics;
pub mod error;
pub mod module_info;
pub mod process;
pub mod stats;
pub mod stop_reason;
pub(crate) mod thread_extra_info;
pub(crate) mod threads;
pub(crate) mod trace_metadata;
pub(crate) mod watchpoint;

pub use breakpoint::{BreakpointId, BreakpointKind};
pub use diagnostics::{TargetDiagnostics, ThreadDiagnostics};
pub use error::DebugError;
pub use module_info::ModuleInfo;
pub use process::TtdProcess;
pub use stats::TargetStats;
pub use stop_reason::StopReason;
pub use thread_extra_info::ThreadExtraInfoData;

use crate::ttd::types::{DataAccessMask, TtdMemRange, TtdPosition, TtdX64Regs};

/// Instruction-level debug API.
///
/// Frontends hold a concrete target (e.g. `TtdProcess`) behind this trait.
/// Construction is NOT part of the trait — frontends create the concrete
/// type directly (e.g. `TtdProcess::open()`).
pub trait DebugTarget {
    // ─── Execution control ─────────────────────────────────

    /// Single step forward one instruction.
    ///
    /// # Errors
    /// [`DebugError::Ttd`] when the backend cannot perform the step.
    fn step(&mut self) -> Result<StopReason, DebugError>;

    /// Single step backward one instruction.
    ///
    /// # Errors
    /// [`DebugError::Ttd`] when the backend cannot perform the step.
    fn step_back(&mut self) -> Result<StopReason, DebugError>;

    /// Range step: execute forward until the thread's PC is no longer in
    /// `[start, end)`, then report `StepComplete`.
    ///
    /// This is GDB's `vCont;r` semantic. Like single-stepping, range stepping
    /// is watchpoint-free: a client breakpoint inside the range must not stop
    /// it. `thread_id` follows [`Self::thread_state`] (`None` = current
    /// thread).
    ///
    /// The default implementation is the portable single-step loop. Real
    /// targets should override it when they can read the PC from the same
    /// cursor they are stepping: `TtdProcess` does, which avoids a persistent
    /// query-cursor seek after every instruction.
    ///
    /// This path is independent of TTD watchpoint callbacks and their
    /// speculative execution behavior: it consumes `StepCount` results and
    /// reads context at the exact stopped cursor position, never through the
    /// watchpoint-hit callback.
    ///
    /// # Errors
    /// Whatever [`Self::step`] reports; the portable default propagates it
    /// instead of silently stopping the range.
    ///
    /// The portable default also stops the range with `StepComplete` when the
    /// requested thread's state becomes unavailable mid-range (e.g. the thread
    /// exited): there is no PC to range-check against, and advancing blindly
    /// could run past the range end. That case is not an error.
    fn step_range(
        &mut self,
        thread_id: Option<u64>,
        start: u64,
        end: u64,
    ) -> Result<StopReason, DebugError> {
        let mut previous = self.position();
        loop {
            match self.step()? {
                StopReason::StepComplete => {}
                other => return Ok(other),
            }
            let Some((regs, _)) = self.thread_state(thread_id) else {
                return Ok(StopReason::StepComplete);
            };
            if !(start..end).contains(&regs.Rip) {
                return Ok(StopReason::StepComplete);
            }
            let now = self.position();
            if now <= previous {
                return Ok(StopReason::StepComplete);
            }
            previous = now;
        }
    }

    /// Continue forward until breakpoint, exception, or trace end.
    ///
    /// # Errors
    /// [`DebugError::Ttd`] when the backend cannot perform the replay.
    fn continue_forward(&mut self) -> Result<StopReason, DebugError>;

    /// Continue backward until breakpoint or trace start.
    ///
    /// # Errors
    /// [`DebugError::Ttd`] when the backend cannot perform the replay.
    fn continue_backward(&mut self) -> Result<StopReason, DebugError>;

    /// Jump to an arbitrary trace position.
    ///
    /// # Errors
    /// [`DebugError::Ttd`] when the backend cannot reposition its cursor.
    fn goto(&mut self, pos: TtdPosition) -> Result<StopReason, DebugError>;

    // ─── Memory ────────────────────────────────────────────

    /// Read memory into `buf`. Returns number of bytes actually read.
    fn read_memory(&self, addr: u64, buf: &mut [u8]) -> usize;

    /// Read memory and report where each run of bytes came from.
    ///
    /// Returns `(bytes_read, ranges)`. This is diagnostics, not a stronger
    /// guarantee: a backend that assembles one read from several trace
    /// positions reports one range per position, so a caller can see that the
    /// buffer is not a single-instant snapshot. Reporting an old position does
    /// not mean the value is wrong — a location nothing wrote since is served
    /// correctly from an old data line.
    ///
    /// The default implementation returns the bytes from [`Self::read_memory`]
    /// with no provenance, which is what a backend that cannot attribute bytes
    /// to a position should do.
    fn read_memory_ranges(
        &self,
        addr: u64,
        buf: &mut [u8],
        _max_ranges: u32,
    ) -> (usize, Vec<TtdMemRange>) {
        (self.read_memory(addr, buf), Vec::new())
    }

    // ─── Breakpoints ───────────────────────────────────────

    /// Set an execution breakpoint at `addr`. Returns breakpoint ID.
    /// `thread_id` limits the breakpoint to a specific thread; `None` = any.
    ///
    /// An `Err` means the backend could not register the breakpoint (e.g.
    /// the TTD cursor rejected the watchpoint); protocol frontends surface
    /// that to the client instead of acknowledging a breakpoint that will
    /// never fire.
    ///
    /// # Errors
    /// [`DebugError::BreakpointRegister`] when the backend rejects the
    /// watchpoint; the logical entry is rolled back before returning.
    fn set_breakpoint(&mut self, addr: u64, thread_id: Option<u64>) -> Result<u64, DebugError>;

    /// Set a data watchpoint. Returns breakpoint ID.
    ///
    /// # Errors
    /// [`DebugError::BreakpointRegister`] when the backend rejects the
    /// watchpoint; the logical entry is rolled back before returning.
    fn set_data_breakpoint(
        &mut self,
        addr: u64,
        size: u64,
        kind: BreakpointKind,
    ) -> Result<u64, DebugError>;

    /// Remove a breakpoint by ID. Returns true if found.
    fn remove_breakpoint(&mut self, id: u64) -> bool;

    /// Remove **every** breakpoint registered at this exact
    /// `(addr, size, access)` triple, whatever their IDs. Returns true when
    /// anything was removed.
    ///
    /// Address-keyed removal is the wire-level contract: RSP clients identify
    /// breakpoints by address (`z0,<addr>,…`), may insert several IDs for one
    /// address across reconnects/restarts, and expect one `z` packet to clear
    /// them all.
    fn remove_breakpoint_exact(&mut self, _addr: u64, _size: u64, _access: DataAccessMask) -> bool {
        false
    }

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
    /// `UniqueId` is unique for the whole trace).
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

    /// The process's main executable module, when the trace has one.
    ///
    /// TTD reports the recorded executable as the first entry of the module
    /// list, but that ordering is a backend detail: callers that need "the
    /// program" (exec-file, `AT_ENTRY`, `qOffsets`, the shared-library list)
    /// ask the backend here instead of indexing [`Self::modules`].
    ///
    /// The default is `None` on purpose — a backend that does not model a
    /// main executable must say so rather than have a GDB-layer convention
    /// silently pick one of its modules.
    fn main_module(&self) -> Option<ModuleInfo> {
        None
    }

    /// All exception events recorded by the trace (code, address, position),
    /// in trace order. Used by the `monitor ttd events` command.
    fn exceptions(&self) -> Vec<crate::ttd::types::TtdExceptionEvent> {
        Vec::new()
    }

    // ─── Diagnostics (TTD-specific introspection) ──────────

    /// Snapshot of trace-level information used by the `monitor ttd …`
    /// custom commands and the `qTTD…` custom packets. Cheap to compute;
    /// the underlying engine APIs are O(1) for counts and O(n) for the
    /// thread list.
    fn diagnostics(&self) -> TargetDiagnostics;

    /// Replay/query counters — mostly for `monitor ttd stats`. The default is
    /// all zeros: targets that don't instrument themselves simply report
    /// nothing rather than forcing every backend to keep counters.
    fn stats(&self) -> TargetStats {
        TargetStats::default()
    }

    // ─── Position ──────────────────────────────────────────

    /// Current trace position.
    fn position(&self) -> TtdPosition;

    /// Trace lifetime (first, last).
    fn lifetime(&self) -> (TtdPosition, TtdPosition);

    // ─── Cancellation ──────────────────────────────────────

    /// A thread-safe handle that aborts an in-flight replay, if the target
    /// supports it. Protocol frontends call it when they see ^C while a
    /// resume operation is running on a worker thread.
    ///
    /// This is a factory taken once at construction time precisely so the
    /// handle can fire *while the target is borrowed by the replay worker*:
    /// implementations must not require locking anything the replay path
    /// holds. The default is `None` (targets without blocking replays don't
    /// need cancellation).
    fn interrupt_handle(&self) -> Option<std::sync::Arc<dyn Fn() + Send + Sync>> {
        None
    }

    /// Called once before each resume operation starts, so per-operation
    /// cancellation state cannot leak in from an earlier one.
    ///
    /// A target whose operations are abortable at the SDK level (one blocking
    /// call) needs nothing here. A target that implements a resume as a loop
    /// of many short calls — range stepping — records "an interrupt was
    /// requested" for its loop to poll, and must clear that record here:
    /// otherwise a ^C that arrived while the target was stopped would abort
    /// the next operation before it did any work.
    fn begin_resume(&self) {}
}
