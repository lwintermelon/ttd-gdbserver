use crate::ttd::TtdError;

#[derive(Debug, thiserror::Error)]
pub enum DebugError {
    #[error("TTD error: {0}")]
    Ttd(#[from] TtdError),
    #[error("No trace loaded")]
    NotLoaded,
    #[error("Invalid address: {0:#x}")]
    InvalidAddress(u64),
    #[error("Replay failed: {0}")]
    ReplayFailed(String),
    #[error("Target description XML not found: {0}")]
    TargetXmlNotFound(String),
}
