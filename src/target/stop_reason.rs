use crate::ttd::error::TtdError;
use crate::ttd::types::{DataAccessMask, EventType, TtdReplayResult};

/// Why the debugger stopped.
#[derive(Debug, Clone)]
pub enum StopReason {
    /// Hit a breakpoint at the given address.
    Breakpoint { bp_id: u64, addr: u64 },
    /// Hit a data watchpoint. `access` is the mask that fired (read/write
    /// bits), so frontends can report read vs write stops.
    Watchpoint {
        bp_id: u64,
        addr: u64,
        access: DataAccessMask,
    },
    /// Single step completed.
    StepComplete,
    /// Reached the end of the trace.
    TraceEnd,
    /// The recorded process exited. Distinct from [`StopReason::TraceEnd`],
    /// which can also mean a thread ended or the client hit a trace boundary:
    /// only a real process exit puts the GDB layer into rr's end state, where
    /// a further forward resume is answered with the real exit packet `W00`.
    /// Both are first reported as the same synthetic SIGKILL stop.
    ProcessExit,
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

impl TryFrom<&TtdReplayResult> for StopReason {
    type Error = TtdError;

    /// Translate a TTD replay result into a frontend stop reason.
    ///
    /// `EventType::Error` and unknown event values are hard errors: the replay
    /// state is not trustworthy, so reporting "step complete" (the old
    /// catch-all) would make a client silently resume from a broken backend.
    fn try_from(result: &TtdReplayResult) -> Result<Self, Self::Error> {
        // StepCount and Gap deliberately share a body: both leave the cursor
        // on a valid instruction and both should look like a completed step to
        // the client, but the two events are reported separately so the
        // comment on each stays honest about why.
        #[allow(clippy::match_same_arms)]
        match result.stop_reason {
            EventType::MemoryWatchpoint => Ok(StopReason::Watchpoint {
                bp_id: 0,
                addr: result.wp_address,
                access: result.wp_access_type.mask(),
            }),
            EventType::StepCount => Ok(StopReason::StepComplete),
            EventType::Position | EventType::PositionWatchpoint => Ok(StopReason::PositionReached),
            // A Process event is a real process exit; a Thread event only
            // ends one thread. Continue paths refine both against the current
            // position, but the distinction matters to GDB's exit reply.
            EventType::Process => Ok(StopReason::ProcessExit),
            EventType::Thread => Ok(StopReason::TraceEnd),
            EventType::Exception => Ok(StopReason::Exception { code: 0, addr: 0 }),
            EventType::Interrupted => Ok(StopReason::Interrupted),
            // A Gap means the trace jumped over unrecorded execution. The
            // replay cursor is still at a valid recorded position, so a step
            // is the least surprising frontend-visible outcome.
            EventType::Gap => Ok(StopReason::StepComplete),
            EventType::Error => Err(TtdError::ReplayFailed),
            EventType::Count | EventType::Unknown(_) => Err(TtdError::UnsupportedEvent {
                event: result.stop_reason,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::StopReason;
    use crate::ttd::types::*;

    fn make_result(stop_reason: EventType) -> TtdReplayResult {
        TtdReplayResult {
            stop_reason,
            steps_executed: 1,
            instructions_executed: 1,
            wp_address: 0xDEAD,
            wp_size: 4,
            wp_access_type: DataAccessType::Execute,
        }
    }

    #[test]
    fn from_step_count() {
        let r = make_result(EventType::StepCount);
        assert!(matches!(
            StopReason::try_from(&r).unwrap(),
            StopReason::StepComplete
        ));
    }

    #[test]
    fn from_memory_watchpoint() {
        let r = make_result(EventType::MemoryWatchpoint);
        assert!(matches!(
            StopReason::try_from(&r).unwrap(),
            StopReason::Watchpoint {
                addr: 0xDEAD,
                access: DataAccessMask::EXECUTE,
                ..
            }
        ));
    }

    #[test]
    fn from_position_and_position_watchpoint() {
        for event in [EventType::Position, EventType::PositionWatchpoint] {
            let r = make_result(event);
            assert!(matches!(
                StopReason::try_from(&r).unwrap(),
                StopReason::PositionReached
            ));
        }
    }

    #[test]
    fn from_process_and_thread() {
        let process = make_result(EventType::Process);
        assert!(matches!(
            StopReason::try_from(&process).unwrap(),
            StopReason::ProcessExit
        ));
        let thread = make_result(EventType::Thread);
        assert!(matches!(
            StopReason::try_from(&thread).unwrap(),
            StopReason::TraceEnd
        ));
    }

    #[test]
    fn from_exception() {
        let r = make_result(EventType::Exception);
        assert!(matches!(
            StopReason::try_from(&r).unwrap(),
            StopReason::Exception { code: 0, addr: 0 }
        ));
    }

    #[test]
    fn from_interrupted() {
        let r = make_result(EventType::Interrupted);
        assert!(matches!(
            StopReason::try_from(&r).unwrap(),
            StopReason::Interrupted
        ));
    }

    #[test]
    fn from_gap() {
        let r = make_result(EventType::Gap);
        assert!(matches!(
            StopReason::try_from(&r).unwrap(),
            StopReason::StepComplete
        ));
    }

    /// `EventType::Error` must abort loudly rather than masquerading as a
    /// bytecode-level step stop.
    #[test]
    fn backend_error_is_returned_as_error() {
        let r = make_result(EventType::Error);
        assert!(matches!(
            StopReason::try_from(&r),
            Err(crate::ttd::TtdError::ReplayFailed)
        ));
    }

    /// Unknown event types (including `EventType::Invalid`/future SDK values)
    /// must not be swallowed by a catch-all `StepComplete`.
    #[test]
    fn unknown_event_is_returned_as_error() {
        let r = make_result(EventType::from(0xFE));
        assert!(matches!(
            StopReason::try_from(&r),
            Err(crate::ttd::TtdError::UnsupportedEvent {
                event: EventType::Unknown(0xFE)
            })
        ));
    }
}
