//! Exception handling against the corpus's fault recording.
//!
//! Every other trace test uses the canonical `c-core` recording, which has no
//! exceptions; `c-fault` is a separate recording of a program that
//! dereferences a bad pointer, so these tests exercise the real event path:
//! a hardware exception stops a continue, and its code/address come from the
//! trace's exception table rather than from the replay result.

mod common;

use ttd_gdbserver::target::{DebugTarget, StopReason};

use common::open_fault_trace;

/// The recorded fault is an access violation.
const STATUS_ACCESS_VIOLATION: u32 = 0xC000_0005;

#[test]
#[cfg_attr(not(has_ttd_fault_trace), ignore = "requires TTD_FAULT_TRACE_PATH")]
fn the_trace_records_one_hardware_exception() {
    let process = open_fault_trace();
    let events = process.exceptions();
    assert_eq!(
        events.len(),
        1,
        "the fault recording must contain exactly one exception event: {events:?}"
    );
    assert_eq!(events[0].code, STATUS_ACCESS_VIOLATION);
    assert!(
        events[0].position > process.lifetime().0,
        "the fault must happen after the trace starts"
    );
}

/// A continue must stop on the hardware fault instead of running to the trace
/// end — that is what makes the client see a SIGSEGV — and the stop reason
/// must carry the code and address recorded in the trace.
#[test]
#[cfg_attr(not(has_ttd_fault_trace), ignore = "requires TTD_FAULT_TRACE_PATH")]
fn continue_stops_on_the_recorded_exception() {
    let mut process = open_fault_trace();
    let event = process.exceptions()[0];

    let reason = process.continue_forward().expect("continue to the fault");
    match reason {
        StopReason::Exception { code, addr } => {
            assert_eq!(
                code, STATUS_ACCESS_VIOLATION,
                "the stop must carry the recorded exception code"
            );
            assert_eq!(
                addr, event.record_address,
                "the stop must carry the recorded exception address"
            );
        }
        other => panic!("a hardware fault must stop the continue, got {other:?}"),
    }
    assert!(
        process.position() >= event.position,
        "the cursor must be at or past the recorded fault position"
    );
}

/// After the exception stop, a further continue must pass the fault instead
/// of stopping on it again. Delve "propagates" a fault by resuming, so a
/// repeat stop here would loop forever.
#[test]
#[cfg_attr(not(has_ttd_fault_trace), ignore = "requires TTD_FAULT_TRACE_PATH")]
fn a_second_continue_passes_the_fault() {
    let mut process = open_fault_trace();
    assert!(matches!(
        process.continue_forward().expect("continue"),
        StopReason::Exception { .. }
    ));
    let fault_position = process.position();

    let reason = process.continue_forward().expect("continue past the fault");
    assert!(
        matches!(reason, StopReason::TraceEnd | StopReason::ProcessExit),
        "resuming after a fault must run past it, got {reason:?}"
    );
    assert!(
        process.position() > fault_position,
        "the second continue must advance beyond the fault"
    );
}
