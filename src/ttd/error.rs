use std::fmt;

#[derive(Debug)]
pub enum TtdError {
    EngineCreationFailed,
    TraceLoadFailed { path: String, code: i32 },
    IndexBuildFailed { code: i32 },
    CursorCreationFailed,
    NullPointer,
    Ffi(&'static str),
}

impl std::error::Error for TtdError {}

impl fmt::Display for TtdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TtdError::EngineCreationFailed => write!(f, "Failed to create TTD replay engine"),
            TtdError::TraceLoadFailed { path, code } => {
                write!(f, "Failed to load trace '{}': error code {}", path, code)
            }
            TtdError::IndexBuildFailed { code } => {
                write!(f, "Failed to build trace index: error code {}", code)
            }
            TtdError::CursorCreationFailed => write!(f, "Failed to create cursor"),
            TtdError::NullPointer => write!(f, "Null pointer encountered"),
            TtdError::Ffi(msg) => write!(f, "FFI error: {}", msg),
        }
    }
}
