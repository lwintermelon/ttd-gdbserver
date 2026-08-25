//! Replay/query counters, exposed by [`DebugTarget::stats`].
//!
//! The interesting ratio is `cursor_seeks / queries`: a replay cursor has to
//! be seeked (`SetPosition`) to the debug position before it can answer
//! anything, and a seek may replay from a keyframe — i.e. it is the expensive
//! part of a query. The target caches each cursor's position, so a stopped
//! target should answer dozens of queries per seek. If that ratio ever
//! approaches 1, the cache has been broken. The physical-watchpoint counters
//! make the dedup/registry contract observable to tests and `monitor`.
//!
//! [`DebugTarget::stats`]: super::DebugTarget::stats

/// One flat snapshot of the target's counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TargetStats {
    /// Cursor reads (`g`/`m`/thread queries) served.
    pub queries: u64,
    /// `SetPosition` calls those reads needed.
    pub cursor_seeks: u64,
    /// Single steps (forward or backward).
    pub steps: u64,
    /// Continue-style replays (forward or backward).
    pub replays: u64,
    /// Physical TTD watchpoints actually registered (`AddMemoryWatchpoint`).
    /// Several logical breakpoints may collapse onto one key; this counter
    /// makes the deduplication and convergent-sync behavior observable.
    pub watchpoint_adds: u64,
    /// Physical TTD watchpoints actually removed (`RemoveMemoryWatchpoint`).
    pub watchpoint_removes: u64,
}
