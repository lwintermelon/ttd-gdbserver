use crate::ttd::TtdError;

#[derive(Debug, thiserror::Error)]
pub enum DebugError {
    /// `TargetRunner::start` was called while a previous replay worker was
    /// still in flight. Protocol clients should never trigger this; it is an
    /// internal safety net that turns the old panic into a normal RSP error.
    #[error("a replay is already in flight")]
    ReplayAlreadyRunning,
    #[error("TTD error: {0}")]
    Ttd(#[from] TtdError),
    /// The engine refused to register a watchpoint, so the breakpoint would
    /// never fire. Protocol frontends surface this to the client (RSP
    /// `E16`, "cannot insert breakpoint") instead of acknowledging it.
    #[error("breakpoint registration failed at {addr:#x}")]
    BreakpointRegister {
        addr: u64,
        #[source]
        source: TtdError,
    },
}
