use crate::ttd::types::{DataAccessMask, EventType};

/// Result alias for the TTD layer.
pub type Result<T> = std::result::Result<T, TtdError>;

#[derive(Debug, thiserror::Error)]
pub enum TtdError {
    /// Checked before the FFI call: the engine reports only "initialize
    /// failed", which is indistinguishable from a missing DLL or a malformed
    /// trace. Naming the missing path is the difference between a one-line fix
    /// and a hunt.
    #[error("trace file not found: '{path}'")]
    TraceNotFound { path: String },
    #[error("failed to load trace '{path}'")]
    TraceLoadFailed { path: String },
    #[error("failed to build trace index")]
    IndexBuildFailed,
    #[error("cannot remove unusable trace index '{path}': {message}")]
    IndexFileAccess { path: String, message: String },
    #[error("failed to create cursor")]
    CursorCreationFailed,
    #[error("FFI call failed: {0}")]
    Ffi(&'static str),
    #[error("{op} failed at {addr:#x} (size {size}, access {access:?})")]
    Watchpoint {
        op: &'static str,
        addr: u64,
        size: u64,
        access: DataAccessMask,
    },
    /// A watchpoint request whose address/length is not a safe guest range.
    /// Passing it to TTD's `AddMemoryWatchpoint` can crash the recorder process
    /// (observed with `Z2,ffffffffffffffff,ffffffff`), so it is rejected in
    /// Rust before the FFI call.
    #[error("invalid watchpoint range: addr {addr:#x}, size {size}")]
    InvalidWatchpoint { addr: u64, size: u64 },
    /// The replay engine stopped with `EventType::Error`, i.e. the backend
    /// itself failed rather than the requested replay reaching a boundary.
    #[error("the replay engine reported an internal error")]
    ReplayFailed,
    /// The backend reported an event this crate does not know how to map.
    /// Failing loudly is deliberate: silently mapping it to "step complete"
    /// would make the frontend resume from an invalid replay state.
    #[error("TTD replay returned an unsupported event ({event})")]
    UnsupportedEvent { event: EventType },
    /// The trace was recorded on an architecture whose register layout this
    /// crate does not implement (`x86_64` is the only one).
    #[error("unsupported trace architecture (only x86_64 is implemented)")]
    UnsupportedArchitecture,
}

impl TtdError {
    /// Build the error for a failed `AddMemoryWatchpoint`/`RemoveMemoryWatchpoint`
    /// call, carrying the full watchpoint identity for diagnostics.
    #[must_use]
    pub fn watchpoint(op: &'static str, addr: u64, size: u64, access: DataAccessMask) -> Self {
        Self::Watchpoint {
            op,
            addr,
            size,
            access,
        }
    }
}
