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
use crate::ttd::types::DataAccessMask;

/// Pick the thread a stop reply should advertise.
///
/// Prefer the backend's current thread; fall back to the first active thread
/// when the current one is unknown (e.g. after a trace boundary), and finally
/// to thread 1 so the stop reply always has a syntactically valid id.
pub(super) fn stop_tid<T: DebugTarget>(backend: &T) -> Tid {
    backend
        .current_thread_id()
        .and_then(to_wire_tid)
        .or_else(|| {
            backend
                .active_thread_ids()
                .first()
                .copied()
                .and_then(to_wire_tid)
        })
        .unwrap_or_else(|| NonZeroUsize::new(1).unwrap())
}

/// Narrow a backend tid into the wire `Tid` space (`NonZeroUsize`). A tid
/// that cannot be represented there (0, or wider than `usize`) is
/// undeliverable; dropping it is better than truncating it into a different
/// thread's id.
pub(super) fn to_wire_tid(t: u64) -> Option<Tid> {
    usize::try_from(t).ok().and_then(NonZeroUsize::new)
}

/// Widen a wire `Tid` into the u64 id space the backend speaks. `usize` is
/// at most 64 bits on every target this crate builds for, so the widening is
/// lossless; the fallible form (rather than `as`) keeps the assumption
/// explicit and turns it into a loud failure instead of silent truncation if
/// it ever stops holding.
pub(super) fn from_wire_tid(tid: Tid) -> u64 {
    u64::try_from(tid.get()).expect("usize is at most 64 bits on supported targets")
}

/// Map a `DataAccessMask` bitmask to the GDB watch kind reported in stop
/// replies, so `rwatch` hits are not misreported as writes. Execute-only
/// hits (possible when a data breakpoint was registered as execute) fall
/// back to Write, which is what GDB displays for hardware watch stops.
pub(super) fn watch_kind_from_access(access: DataAccessMask) -> WatchKind {
    let read = access.contains(DataAccessMask::READ);
    let write = access.contains(DataAccessMask::WRITE);
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
    // C++ exception magics, one per toolchain: MSVC raises 0xE06D7363
    // ("msc" + 0xE0) and GCC/MinGW raises 0x20474343 ("CCG " = " GCC"
    // little-endian). Both mean "first-chance C++ exception", and a trace
    // records whichever runtime the program was built with, so both map to
    // SIGABRT.
    const MSVC_CPP_EXCEPTION: u32 = 0xE06D_7363;
    const GCC_CPP_EXCEPTION: u32 = 0x2047_4343;
    // The named ACCESS_VIOLATION arm and the catch-all share a body on
    // purpose: keeping the well-known code explicit documents that it was
    // considered, rather than folded into "everything else".
    #[allow(clippy::match_same_arms)]
    match code {
        // Both are debug traps the user can continue from, not faults.
        STATUS_BREAKPOINT | STATUS_SINGLE_STEP => Signal::SIGTRAP,
        STATUS_ACCESS_VIOLATION => Signal::SIGSEGV,
        MSVC_CPP_EXCEPTION | GCC_CPP_EXCEPTION => Signal::SIGABRT,
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
            log::error!("replay failed: {e}");
            return MultiThreadStopReason::SignalWithThread {
                tid,
                signal: Signal::SIGTRAP,
            };
        }
    };
    match reason {
        // A recorded process exit is reported exactly like trace end: the
        // rr-style synthetic SIGKILL keeps the stop rewindable (Delve's
        // "almost exited"). The real exit reply (`W00`) does not come from
        // this mapping — it is answered by the end-state interception in
        // `GdbTarget::spawn_replay` when the client resumes *forward* again
        // from the exit stop, the same two-stage sequence rr implements
        // (`in_debuggee_end_state` → `handle_exited_state`).
        StopReason::ProcessExit | StopReason::TraceEnd => MultiThreadStopReason::SignalWithThread {
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
pub(super) fn access_mask_from_watch_kind(kind: WatchKind) -> DataAccessMask {
    match kind {
        WatchKind::Write => DataAccessMask::WRITE,
        WatchKind::Read => DataAccessMask::READ,
        WatchKind::ReadWrite => DataAccessMask::READ_WRITE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_tid_conversion_roundtrips_and_rejects_bad_ids() {
        // Every nonzero u64 that fits `usize` widens back unchanged.
        for t in [1u64, 100, 0xdead_beef] {
            let tid = to_wire_tid(t).expect("must be representable on the wire");
            assert_eq!(from_wire_tid(tid), t);
        }
        // 0 is not a wire tid (NonZeroUsize), and neither is an id too wide
        // for `usize`; both are dropped rather than truncated.
        assert!(to_wire_tid(0).is_none());
        #[cfg(target_pointer_width = "32")]
        assert!(to_wire_tid(u64::from(u32::MAX) + 1).is_none());
    }

    #[test]
    fn watch_kind_from_access_mask() {
        let r = DataAccessMask::READ;
        let w = DataAccessMask::WRITE;
        assert!(matches!(watch_kind_from_access(r), WatchKind::Read));
        assert!(matches!(watch_kind_from_access(w), WatchKind::Write));
        assert!(matches!(
            watch_kind_from_access(r | w),
            WatchKind::ReadWrite
        ));
        // Execute-only has no GDB watch kind; it degrades to Write rather
        // than misreporting a read hit.
        assert!(matches!(
            watch_kind_from_access(DataAccessMask::EXECUTE),
            WatchKind::Write
        ));
        // A mask with no read/write bit at all (e.g. an uninitialized 0)
        // must not claim a read.
        assert!(matches!(
            watch_kind_from_access(DataAccessMask::NONE),
            WatchKind::Write
        ));
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
        // MinGW/GCC raises a different magic for the same first-chance C++
        // exception (`0x20474343`, " GCC" reversed); a trace built with g++
        // records this one, so it must not fall through to SIGSEGV.
        assert_eq!(signal_for_exception(0x2047_4343), Signal::SIGABRT);
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
                DataAccessMask::WRITE,
            ),
            (WatchKind::Read, BreakpointKind::Read, DataAccessMask::READ),
            (
                WatchKind::ReadWrite,
                BreakpointKind::Access,
                DataAccessMask::READ_WRITE,
            ),
        ] {
            assert_eq!(breakpoint_kind_from_watch_kind(kind), bp_kind);
            assert_eq!(access_mask_from_watch_kind(kind), access);
        }
    }
}
