use crate::ttd::types::TtdReplayResult;
use crate::ttd::types::*;

/// Why the debugger stopped.
#[derive(Debug, Clone)]
pub enum StopReason {
    /// Hit a breakpoint at the given address.
    Breakpoint { bp_id: u64, addr: u64 },
    /// Hit a data watchpoint.
    Watchpoint { bp_id: u64, addr: u64 },
    /// Single step completed.
    StepComplete,
    /// Reached the end of the trace.
    TraceEnd,
    /// Reached the beginning of the trace (reverse).
    TraceStart,
    /// Exception encountered.
    Exception { code: u32, addr: u64 },
    /// Position reached (replay hit the limit position).
    PositionReached,
}

impl StopReason {
    #[allow(non_upper_case_globals)]
    pub fn from_replay_result(result: &TtdReplayResult) -> Self {
        match result.stop_reason {
            TTD_Replay_EventType_MemoryWatchpoint => StopReason::Watchpoint {
                bp_id: 0,
                addr: result.wp_address,
            },
            TTD_Replay_EventType_StepCount => StopReason::StepComplete,
            TTD_Replay_EventType_Position => StopReason::PositionReached,
            TTD_Replay_EventType_Process | TTD_Replay_EventType_Thread => StopReason::TraceEnd,
            TTD_Replay_EventType_Exception => StopReason::Exception { code: 0, addr: 0 },
            TTD_Replay_EventType_Interrupted | TTD_Replay_EventType_Gap => StopReason::StepComplete,
            _ => StopReason::StepComplete,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::StopReason;
    use crate::ttd::types::*;

    fn make_result(event_value: u8) -> TtdReplayResult {
        TtdReplayResult {
            stop_reason: event_value,
            wp_address: 0xDEAD,
            ..Default::default()
        }
    }

    #[test]
    fn from_step_count() {
        let r = make_result(TTD_Replay_EventType_StepCount);
        assert!(matches!(
            StopReason::from_replay_result(&r),
            StopReason::StepComplete
        ));
    }

    #[test]
    fn from_memory_watchpoint() {
        let r = make_result(TTD_Replay_EventType_MemoryWatchpoint);
        assert!(matches!(
            StopReason::from_replay_result(&r),
            StopReason::Watchpoint { addr: 0xDEAD, .. }
        ));
    }

    #[test]
    fn from_position() {
        let r = make_result(TTD_Replay_EventType_Position);
        assert!(matches!(
            StopReason::from_replay_result(&r),
            StopReason::PositionReached
        ));
    }

    #[test]
    fn from_process() {
        let r = make_result(TTD_Replay_EventType_Process);
        assert!(matches!(
            StopReason::from_replay_result(&r),
            StopReason::TraceEnd
        ));
    }

    #[test]
    fn from_thread() {
        let r = make_result(TTD_Replay_EventType_Thread);
        assert!(matches!(
            StopReason::from_replay_result(&r),
            StopReason::TraceEnd
        ));
    }

    #[test]
    fn from_exception() {
        let r = make_result(TTD_Replay_EventType_Exception);
        assert!(matches!(
            StopReason::from_replay_result(&r),
            StopReason::Exception { code: 0, addr: 0 }
        ));
    }

    #[test]
    fn from_interrupted() {
        let r = make_result(TTD_Replay_EventType_Interrupted);
        assert!(matches!(
            StopReason::from_replay_result(&r),
            StopReason::StepComplete
        ));
    }

    #[test]
    fn from_gap() {
        let r = make_result(TTD_Replay_EventType_Gap);
        assert!(matches!(
            StopReason::from_replay_result(&r),
            StopReason::StepComplete
        ));
    }
}
