//! GDB RSP wire mapping for backend results.
//!
//! Pure functions with no `GdbTarget` state: stop reason → `T` packet
//! vocabulary, TTD watch-access bitmask → GDB watch kind, Windows exception
//! code → GDB signal, and choosing the thread id a stop reply advertises.
//! Keeping them here makes the protocol mapping unit-testable without a
//! socket, a trace, or a live `TargetRunner`.

use std::num::NonZeroUsize;

use gdbstub::common::{Signal, Tid};
use gdbstub::stub::MultiThreadStopReason;
use gdbstub::target::ext::breakpoints::WatchKind;

use crate::target::{BreakpointKind, DebugError, DebugTarget, StopReason};
use crate::ttd::types::{TTD_Replay_DataAccessMask_Read, TTD_Replay_DataAccessMask_Write};

/// Pick the thread a stop reply should advertise.
///
/// Prefer the backend's current thread; fall back to the first active thread
/// when the current one is unknown (e.g. after a trace boundary), and finally
/// to thread 1 so the stop reply always has a syntactically valid id.
pub(super) fn stop_tid<T: DebugTarget>(backend: &T) -> Tid {
    backend
        .current_thread_id()
        .and_then(|t| NonZeroUsize::new(t as usize))
        .or_else(|| {
            backend
                .active_thread_ids()
                .first()
                .and_then(|t| NonZeroUsize::new(*t as usize))
        })
        .unwrap_or_else(|| NonZeroUsize::new(1).unwrap())
}

/// Map a `DataAccessMask` bitmask to the GDB watch kind reported in stop
/// replies, so `rwatch` hits are not misreported as writes. Execute-only
/// hits (possible when a data breakpoint was registered as execute) fall
/// back to Write, which is what GDB displays for hardware watch stops.
pub(super) fn watch_kind_from_access(access: u8) -> WatchKind {
    let read = access & TTD_Replay_DataAccessMask_Read != 0;
    let write = access & TTD_Replay_DataAccessMask_Write != 0;
    match (read, write) {
        (true, true) => WatchKind::ReadWrite,
        (true, false) => WatchKind::Read,
        _ => WatchKind::Write,
    }
}

/// Map a Windows exception code to the GDB signal the client expects.
/// Unmapped codes default to SIGSEGV (a fault), which is what gdb/Delve
/// display for an unexpected OS exception during replay.
pub(super) fn signal_for_exception(code: u32) -> Signal {
    const STATUS_BREAKPOINT: u32 = 0x8000_0003;
    const STATUS_SINGLE_STEP: u32 = 0x8000_0004;
    const STATUS_ACCESS_VIOLATION: u32 = 0xC000_0005;
    const CPP_EXCEPTION: u32 = 0xE06D_7363; // MSVC C++ EH magic ('msc\xE0')
    match code {
        // Both are debug traps the user can continue from, not faults.
        STATUS_BREAKPOINT | STATUS_SINGLE_STEP => Signal::SIGTRAP,
        STATUS_ACCESS_VIOLATION => Signal::SIGSEGV,
        CPP_EXCEPTION => Signal::SIGABRT,
        _ => Signal::SIGSEGV,
    }
}

/// Map a backend stop reason to the GDB RSP stop reply vocabulary.
///
/// The GDB layer deliberately collapses a backend `Err` to a trap: the
/// client must stop and can retry; the detailed error is logged by the
/// replay worker.
pub(super) fn map_stop_reason<T: DebugTarget>(
    backend: &mut T,
    reason: Result<StopReason, DebugError>,
) -> MultiThreadStopReason<u64> {
    let tid = stop_tid(backend);
    let reason = match reason {
        Ok(r) => r,
        Err(e) => {
            log::error!("replay failed: {}", e);
            return MultiThreadStopReason::SignalWithThread {
                tid,
                signal: Signal::SIGTRAP,
            };
        }
    };
    match reason {
        StopReason::TraceEnd => MultiThreadStopReason::SignalWithThread {
            tid,
            signal: Signal::SIGKILL,
        },
        StopReason::TraceStart => MultiThreadStopReason::SignalWithThread {
            tid,
            signal: Signal::SIGZERO,
        },
        StopReason::Watchpoint { addr, access, .. } => MultiThreadStopReason::Watch {
            tid,
            kind: watch_kind_from_access(access),
            addr,
        },
        StopReason::Exception { code, .. } => MultiThreadStopReason::SignalWithThread {
            tid,
            signal: signal_for_exception(code),
        },
        StopReason::Interrupted => MultiThreadStopReason::SignalWithThread {
            tid,
            signal: Signal::SIGINT,
        },
        _ => MultiThreadStopReason::SignalWithThread {
            tid,
            signal: Signal::SIGTRAP,
        },
    }
}

/// Backend breakpoint kind for a GDB hardware watchpoint (`Z2`/`Z3`/`Z4`).
pub(super) fn breakpoint_kind_from_watch_kind(kind: WatchKind) -> BreakpointKind {
    match kind {
        WatchKind::Write => BreakpointKind::Write,
        WatchKind::Read => BreakpointKind::Read,
        WatchKind::ReadWrite => BreakpointKind::Access,
    }
}

/// TTD `DataAccessMask` for a GDB hardware watchpoint, used by `z2`/`z3`/`z4`.
pub(super) fn access_mask_from_watch_kind(kind: WatchKind) -> u8 {
    match kind {
        WatchKind::Write => TTD_Replay_DataAccessMask_Write,
        WatchKind::Read => TTD_Replay_DataAccessMask_Read,
        WatchKind::ReadWrite => TTD_Replay_DataAccessMask_Read | TTD_Replay_DataAccessMask_Write,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ttd::types::TTD_Replay_DataAccessMask_Execute;

    #[test]
    fn watch_kind_from_access_mask() {
        let r = TTD_Replay_DataAccessMask_Read;
        let w = TTD_Replay_DataAccessMask_Write;
        assert!(matches!(watch_kind_from_access(r), WatchKind::Read));
        assert!(matches!(watch_kind_from_access(w), WatchKind::Write));
        assert!(matches!(
            watch_kind_from_access(r | w),
            WatchKind::ReadWrite
        ));
        // Execute-only has no GDB watch kind; it degrades to Write rather
        // than misreporting a read hit.
        assert!(matches!(
            watch_kind_from_access(TTD_Replay_DataAccessMask_Execute),
            WatchKind::Write
        ));
        // A mask with no read/write bit at all (e.g. an uninitialized 0)
        // must not claim a read.
        assert!(matches!(watch_kind_from_access(0), WatchKind::Write));
    }

    /// Windows exception codes must map to the signal the client expects:
    /// a breakpoint is a trap an interactive user can continue from, while
    /// an access violation is a fault.
    #[test]
    fn exception_codes_map_to_signals() {
        const STATUS_BREAKPOINT: u32 = 0x8000_0003;
        const STATUS_SINGLE_STEP: u32 = 0x8000_0004;
        const STATUS_ACCESS_VIOLATION: u32 = 0xC000_0005;
        const STATUS_STACK_OVERFLOW: u32 = 0xC000_00FD;
        const CPP_EXCEPTION: u32 = 0xE06D_7363;

        assert_eq!(signal_for_exception(STATUS_BREAKPOINT), Signal::SIGTRAP);
        assert_eq!(
            signal_for_exception(STATUS_ACCESS_VIOLATION),
            Signal::SIGSEGV
        );
        assert_eq!(signal_for_exception(CPP_EXCEPTION), Signal::SIGABRT);
        // Single step is a trap, like the breakpoint exception: it must be
        // continuable rather than reported as a fault.
        assert_eq!(signal_for_exception(STATUS_SINGLE_STEP), Signal::SIGTRAP);
        // Unmapped codes (stack overflow, garbage) fall back to SIGSEGV so
        // the client stops instead of silently resuming.
        assert_eq!(signal_for_exception(STATUS_STACK_OVERFLOW), Signal::SIGSEGV);
        assert_eq!(signal_for_exception(0), Signal::SIGSEGV);
        assert_eq!(signal_for_exception(0xDEAD_BEEF), Signal::SIGSEGV);
    }

    #[test]
    fn watch_kind_maps_to_backend_kind_and_access_mask() {
        for (kind, bp_kind, access) in [
            (
                WatchKind::Write,
                BreakpointKind::Write,
                TTD_Replay_DataAccessMask_Write,
            ),
            (
                WatchKind::Read,
                BreakpointKind::Read,
                TTD_Replay_DataAccessMask_Read,
            ),
            (
                WatchKind::ReadWrite,
                BreakpointKind::Access,
                TTD_Replay_DataAccessMask_Read | TTD_Replay_DataAccessMask_Write,
            ),
        ] {
            assert_eq!(breakpoint_kind_from_watch_kind(kind), bp_kind);
            assert_eq!(access_mask_from_watch_kind(kind), access);
        }
    }
}
