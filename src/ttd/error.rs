#[derive(Debug, thiserror::Error)]
pub enum TtdError {
    #[error("failed to create TTD replay engine")]
    EngineCreationFailed,
    #[error("failed to load trace '{path}': error code {code}")]
    TraceLoadFailed { path: String, code: i32 },
    #[error("failed to build trace index: error code {code}")]
    IndexBuildFailed { code: i32 },
    #[error("failed to create cursor")]
    CursorCreationFailed,
    #[error("null pointer encountered in TTD FFI call")]
    NullPointer,
    #[error("FFI call failed: {0}")]
    Ffi(&'static str),
    #[error("{op} failed at {addr:#x} (size {size}, access mask {access:#x})")]
    Watchpoint {
        op: &'static str,
        addr: u64,
        size: u64,
        access: u8,
    },
    /// The replay engine stopped with `EventType::Error`, i.e. the backend
    /// itself failed rather than the requested replay reaching a boundary.
    #[error("TTD replay failed (event type {event})")]
    ReplayFailed { event: u8 },
    /// The backend reported an event this crate does not know how to map.
    /// Failing loudly is deliberate: silently mapping it to "step complete"
    /// would make the frontend resume from an invalid replay state.
    #[error("TTD replay returned unsupported event type {event}")]
    UnsupportedEvent { event: u8 },
}

impl TtdError {
    /// Build the error for a failed `AddMemoryWatchpoint`/`RemoveMemoryWatchpoint`
    /// call, carrying the full watchpoint identity for diagnostics.
    pub fn watchpoint(op: &'static str, addr: u64, size: u64, access: u8) -> Self {
        Self::Watchpoint {
            op,
            addr,
            size,
            access,
        }
    }
}
