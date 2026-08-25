use std::path::Path;
use ttd_gdbserver::ttd::TtdEngine;

/// Create a loaded engine from TTD_TRACE_PATH env var.
/// Set TTD_TRACE_PATH to the .run file before running tests:
///   TTD_TRACE_PATH=path/to/file.run cargo test --test ttd -- --nocapture
pub fn create_engine() -> TtdEngine {
    let path = std::env::var("TTD_TRACE_PATH")
        .expect("TTD_TRACE_PATH env var must be set to the .run trace file");
    TtdEngine::open(Path::new(&path)).expect("Failed to create engine and load trace")
}
