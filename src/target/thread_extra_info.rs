//! Per-thread diagnostic info served by [`DebugTarget::thread_info`].
//!
//! [`DebugTarget::thread_info`]: super::DebugTarget::thread_info

use crate::ttd::types::TtdPosition;

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
