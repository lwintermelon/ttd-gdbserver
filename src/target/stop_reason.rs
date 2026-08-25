use crate::ttd::error::TtdError;
use crate::ttd::types::TtdReplayResult;
use crate::ttd::types::*;

/// Why the debugger stopped.
#[derive(Debug, Clone)]
pub enum StopReason {
    /// Hit a breakpoint at the given address.
    Breakpoint { bp_id: u64, addr: u64 },
    /// Hit a data watchpoint. `access` is the `DataAccessMask` bitmask that
    /// fired (Read/Write bits), so frontends can report Read vs Write stops.
    Watchpoint { bp_id: u64, addr: u64, access: u8 },
    /// Single step completed.
    StepComplete,
    /// Reached the end of the trace.
    TraceEnd,
    /// Reached the beginning of the trace (reverse).
    TraceStart,
    /// The backend's interrupt handle aborted the replay (response to ^C).
    /// Distinct from [`StopReason::StepComplete`] so protocol frontends can
    /// answer ^C with a SIGINT stop instead of a misleading success.
    Interrupted,
    /// Exception encountered.
    Exception { code: u32, addr: u64 },
    /// Position reached (replay hit the limit position).
    PositionReached,
}

impl StopReason {
    /// Translate a TTD replay result into a frontend stop reason.
    ///
    /// `EventType::Error` and unknown event values are hard errors: the replay
    /// state is not trustworthy, so reporting "step complete" (the old
    /// catch-all) would make a client silently resume from a broken backend.
    #[allow(non_upper_case_globals)]
    pub fn try_from_replay_result(result: &TtdReplayResult) -> Result<Self, TtdError> {
        match result.stop_reason {
            TTD_Replay_EventType_MemoryWatchpoint => Ok(StopReason::Watchpoint {
                bp_id: 0,
                addr: result.wp_address,
                access: result.wp_access_type,
            }),
            TTD_Replay_EventType_StepCount => Ok(StopReason::StepComplete),
            TTD_Replay_EventType_Position | TTD_Replay_EventType_PositionWatchpoint => {
                Ok(StopReason::PositionReached)
            }
            // Continue paths turn these into TraceStart/TraceEnd using the
            // current position; the raw mapping is the closest neutral value.
            TTD_Replay_EventType_Process | TTD_Replay_EventType_Thread => Ok(StopReason::TraceEnd),
            TTD_Replay_EventType_Exception => Ok(StopReason::Exception { code: 0, addr: 0 }),
            TTD_Replay_EventType_Interrupted => Ok(StopReason::Interrupted),
            // A Gap means the trace jumped over unrecorded execution. The
            // replay cursor is still at a valid recorded position, so a step
            // is the least surprising frontend-visible outcome.
            TTD_Replay_EventType_Gap => Ok(StopReason::StepComplete),
            TTD_Replay_EventType_Error => Err(TtdError::ReplayFailed {
                event: result.stop_reason,
            }),
            other => Err(TtdError::UnsupportedEvent { event: other }),
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
            // Execute = 1<<2 in the DataAccessMask bitmask convention.
            wp_access_type: 4,
            ..Default::default()
        }
    }

    #[test]
    fn from_step_count() {
        let r = make_result(TTD_Replay_EventType_StepCount);
        assert!(matches!(
            StopReason::try_from_replay_result(&r).unwrap(),
            StopReason::StepComplete
        ));
    }

    #[test]
    fn from_memory_watchpoint() {
        let r = make_result(TTD_Replay_EventType_MemoryWatchpoint);
        assert!(matches!(
            StopReason::try_from_replay_result(&r).unwrap(),
            StopReason::Watchpoint {
                addr: 0xDEAD,
                access: 4,
                ..
            }
        ));
    }

    #[test]
    fn from_position_and_position_watchpoint() {
        for event in [
            TTD_Replay_EventType_Position,
            TTD_Replay_EventType_PositionWatchpoint,
        ] {
            let r = make_result(event);
            assert!(matches!(
                StopReason::try_from_replay_result(&r).unwrap(),
                StopReason::PositionReached
            ));
        }
    }

    #[test]
    fn from_process_and_thread() {
        for event in [TTD_Replay_EventType_Process, TTD_Replay_EventType_Thread] {
            let r = make_result(event);
            assert!(matches!(
                StopReason::try_from_replay_result(&r).unwrap(),
                StopReason::TraceEnd
            ));
        }
    }

    #[test]
    fn from_exception() {
        let r = make_result(TTD_Replay_EventType_Exception);
        assert!(matches!(
            StopReason::try_from_replay_result(&r).unwrap(),
            StopReason::Exception { code: 0, addr: 0 }
        ));
    }

    #[test]
    fn from_interrupted() {
        let r = make_result(TTD_Replay_EventType_Interrupted);
        assert!(matches!(
            StopReason::try_from_replay_result(&r).unwrap(),
            StopReason::Interrupted
        ));
    }

    #[test]
    fn from_gap() {
        let r = make_result(TTD_Replay_EventType_Gap);
        assert!(matches!(
            StopReason::try_from_replay_result(&r).unwrap(),
            StopReason::StepComplete
        ));
    }

    /// `EventType::Error` must abort loudly rather than masquerading as a
    /// bytecode-level step stop.
    #[test]
    fn backend_error_is_returned_as_error() {
        let r = make_result(TTD_Replay_EventType_Error);
        assert!(matches!(
            StopReason::try_from_replay_result(&r),
            Err(crate::ttd::TtdError::ReplayFailed { event })
                if event == TTD_Replay_EventType_Error
        ));
    }

    /// Unknown event types (including `EventType::Invalid`/future SDK values)
    /// must not be swallowed by a catch-all `StepComplete`.
    #[test]
    fn unknown_event_is_returned_as_error() {
        let r = make_result(0xFE);
        assert!(matches!(
            StopReason::try_from_replay_result(&r),
            Err(crate::ttd::TtdError::UnsupportedEvent { event: 0xFE })
        ));
    }
}
