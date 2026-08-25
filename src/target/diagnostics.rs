//! Trace-level diagnostics served by [`DebugTarget::diagnostics`].
//!
//! Used by the `monitor ttd …` custom commands. The shape is intentionally
//! flat: each field is a primitive or a small struct so the wire format is
//! straightforward to render.
//!
//! [`DebugTarget::diagnostics`]: super::DebugTarget::diagnostics

use crate::ttd::types::{TtdIndexStatus, TtdPosition};

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
    /// State of the trace index when the trace was opened. `Loaded` is the
    /// only value compatible with accurate memory reads.
    pub index_status: TtdIndexStatus,
}

/// One entry of [`TargetDiagnostics::threads`].
#[derive(Clone, Debug)]
pub struct ThreadDiagnostics {
    pub unique_id: u32,
    pub os_thread_id: u32,
    /// Active time range; `min` is the position the thread became live,
    /// `max` is where it stopped running.
    pub active_time: (TtdPosition, TtdPosition),
}
