//! Execution control for [`TtdProcess`](super::TtdProcess).
//!
//! The replay machinery behind the step/continue/`goto` verbs: the
//! filtered-continue path (TTD watchpoint callback + speculative-execution
//! rules), position adoption after a stop, and stop-reason resolution. The
//! `DebugTarget` verbs themselves live in [`super::target_impl`] (Rust
//! allows only one impl block per type+trait); cursor positioning and cache
//! bookkeeping (`at_position`, `adopt_cursor_position`, …) live in
//! [`super`].

use std::sync::atomic::{AtomicU64, Ordering};

use crate::target::breakpoint::Breakpoint;
use crate::target::{BreakpointKind, DebugError, StopReason};
use crate::ttd::ReplayCursor;
use crate::ttd::error::TtdError;
use crate::ttd::types::{EventMask, EventType, ExceptionMask, TtdPosition, TtdReplayResult};

use super::TtdProcess;
use crate::target::cursors::{current_tid, position_of, replay_to};

// ─── Replay internals ────────────────────────────────────────────

/// Exception classes a step or continue stops on.
///
/// TTD's classification does not line up with the debugger's: an access
/// violation is `Hardware`, but a stack overflow and a MinGW C++ `throw` are
/// both filed under `Software`. Masking only `Hardware` therefore lets a fatal
/// stack overflow replay to the trace end unnoticed, while masking only
/// `Hardware | Software` would stop on every `throw`. Every class is admitted
/// here and the first-chance events a live debugger passes through are filtered
/// afterwards by code (see [`FIRST_CHANCE_EXCEPTION_CODES`]).
pub(crate) const EXCEPTION_MASK: ExceptionMask = ExceptionMask::all();

/// Exception codes a live debugger treats as first-chance and does not stop
/// on: the MSVC and MinGW/GCC C++ exception codes. A C++ `throw` is normal
/// control flow, so a continue must replay straight through it; a client that
/// wants to see throws sets catchpoints, which it resolves to ordinary
/// breakpoints on the runtime throw/catch functions.
const FIRST_CHANCE_EXCEPTION_CODES: [u32; 2] = [0xE06D_7363, 0x2047_4343];

/// One filtered replay, skipping the exceptions a live debugger passes
/// through.
///
/// TTD stops at the exception event's position and advances past it on the
/// next call, so a skipped event costs one extra replay call. The loop also
/// stops when the position stops advancing, so a backend that re-reports the
/// same event cannot spin.
fn replay_past_transparent_exceptions(
    cursor: &mut ReplayCursor,
    metadata: &crate::target::trace_metadata::TraceMetadata,
    limit: TtdPosition,
    forward: bool,
) -> Result<TtdReplayResult, TtdError> {
    let mut previous = None;
    loop {
        let result =
            replay_to(cursor, limit, forward).map_err(|_| TtdError::Ffi("replay failed"))?;
        if result.stop_reason != EventType::Exception {
            return Ok(result);
        }
        let position = position_of(cursor);
        if previous == Some(position) {
            return Ok(result);
        }
        previous = Some(position);
        let Some(code) = metadata.exception_code_at(position) else {
            return Ok(result);
        };
        if !FIRST_CHANCE_EXCEPTION_CODES.contains(&code) {
            return Ok(result);
        }
    }
}

/// Translate a finished continue into a frontend stop reason (pure decision
/// core — see [`TtdProcess::resolve_continue_stop`] for the engine access).
///
/// Returns `None` when the decision needs the replay result itself: only
/// exception enrichment and unknown-event rejection remain, and those are the
/// caller's job.
#[allow(clippy::too_many_arguments)]
fn classify_continue_stop(
    result: &TtdReplayResult,
    hit_bp_id: u64,
    bp: Option<&Breakpoint>,
    forward: bool,
    position: TtdPosition,
    lifetime: (TtdPosition, TtdPosition),
    ends_with_process_exit: bool,
) -> Option<StopReason> {
    if hit_bp_id != 0 {
        let addr = bp.map_or(result.wp_address, |b| b.addr);
        return Some(match bp {
            Some(b) if b.kind == BreakpointKind::Execute => StopReason::Breakpoint {
                bp_id: hit_bp_id,
                addr,
            },
            // Data breakpoint hit (or an unregistered watchpoint): carry
            // the access mask so frontends can report Read/Write/ReadWrite.
            _ => StopReason::Watchpoint {
                bp_id: hit_bp_id,
                addr,
                access: bp.map_or(result.wp_access_type.mask(), |b| b.access),
            },
        });
    }
    // Reaching the last recorded position of a trace whose process exited is
    // a process exit, not a plain boundary: the GDB layer runs rr's two-stage
    // end sequence on it (synthetic SIGKILL first, `W00` when the client
    // resumes forward again). TTD only reports the `Process` event when the
    // cursor is replayed *past* the last position, which a plain continue
    // never does, so the fact is probed once at open time.
    if forward && ends_with_process_exit && position >= lifetime.1 {
        return Some(StopReason::ProcessExit);
    }
    if forward && result.stop_reason == EventType::Process {
        return Some(StopReason::ProcessExit);
    }
    // Hitting a trace boundary is a stronger signal than the generic
    // event TTD reports for it: Delve expects sig 0 at trace start
    // (backward) and its synthetic SIGKILL at trace end (forward).
    if !forward && position <= lifetime.0 {
        return Some(StopReason::TraceStart);
    }
    if forward && position >= lifetime.1 {
        return Some(StopReason::TraceEnd);
    }
    None
}

impl TtdProcess {
    /// Enrich an `Exception` stop reason with the code/address recorded at
    /// the stop position (the replay result itself carries neither).
    fn enrich_exception(&self, reason: StopReason) -> StopReason {
        let StopReason::Exception { code: 0, .. } = reason else {
            return reason;
        };
        let Some(ev) = self.metadata.exception_at(self.position) else {
            return reason;
        };
        StopReason::Exception {
            code: ev.code,
            addr: ev.record_address,
        }
    }

    /// Replay with watchpoints — the core pattern from TTD's
    /// `FilteredWatchpointQuery`: replay to the given limit on the persistent
    /// cursor (watchpoints already synced) with the breakpoint-matching
    /// callback installed, and return the stop reason.
    pub(super) fn watchpoint_replay(
        &mut self,
        limit: TtdPosition,
        forward: bool,
    ) -> Result<StopReason, DebugError> {
        let (result, hit_bp_id) = self.replay_watchpoint_filtered(limit, forward)?;
        // Adopt where the replay actually stopped (TTD rounds to the nearest
        // valid position and may switch threads) before resolving the reason.
        self.adopt_cursor_position();
        self.resolve_continue_stop(&result, hit_bp_id, forward)
    }

    /// Run one filtered continue and return `(replay result, matching
    /// breakpoint id or 0)`.
    ///
    /// Exceptions stop a continue so a client sees the same stop a live
    /// debugger reports, instead of the replay silently running to the trace
    /// boundary. Which classes count is not up to the mask alone: TTD files
    /// an access violation under `Hardware` but a stack overflow and a
    /// MinGW C++ `throw` both under `Software`, so masking hardware only
    /// hides a real crash while masking hardware + software would stop on
    /// every `throw`. The mask therefore admits all three classes and
    /// [`is_transparent_exception`] drops the first-chance events a live
    /// debugger passes through, by replaying past them. Delve is unaffected:
    /// its continue loop resumes past an exception stop automatically.
    fn replay_watchpoint_filtered(
        &mut self,
        limit: TtdPosition,
        forward: bool,
    ) -> Result<(TtdReplayResult, u64), DebugError> {
        {
            let mut cursor = self.cursors.persistent().cursor_mut();
            cursor.set_exception_mask(EXCEPTION_MASK);
        }
        // The persistent cursor may have been left anywhere by earlier
        // operations (steps run on the step cursor, which owns its own
        // position) — position it before replaying.
        self.at_position(self.position, |_| ());
        self.bump(|s| s.replays += 1);

        // Set from the watchpoint callback when a hit matches a registered
        // breakpoint. Atomic because the callback API is `Send`.
        let hit_bp_id = AtomicU64::new(0);
        // A panicking callback unwinds through this call (the FFI shims
        // re-throw it after unwinding is safe), so catch it here to restore
        // the cursor bookkeeping before propagating: the physical cursor may
        // have moved.
        let outcome = {
            // Disjoint field borrows: the callback needs to consult the
            // breakpoint table while it holds the cursor.
            let breakpoints = &self.breakpoints;
            let metadata = &self.metadata;
            let cursor = self.cursors.persistent();
            let active = self.active_replay.clone();
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                cursor.with_watchpoint_callback(
                    &mut |addr, size, access, thread_id| {
                        // The callback receives the OS thread id of the hitting
                        // thread, so thread-filtered breakpoints (set_breakpoint
                        // with thread_id) only fire on the thread they were
                        // registered for.
                        if let Some(bp) = breakpoints.find_by_watchpoint_hit_and_thread(
                            addr,
                            size,
                            access,
                            u64::from(thread_id),
                        ) {
                            hit_bp_id.store(bp.id, Ordering::Release);
                            true
                        } else {
                            false
                        }
                    },
                    |cursor| {
                        cursor.set_event_mask(EventMask::MEMORY_WATCHPOINT | EventMask::EXCEPTION);

                        // Speculative-execution note: TTD's replay engine can
                        // speculatively execute ahead of the cursor to discover
                        // the next watchpoint hit, so a hit at a position
                        // beyond `limit` may be surfaced. We always pass the
                        // *trace boundary* as `limit` (continue forward ->
                        // lifetime end, continue backward -> lifetime start),
                        // so there is no "future" to overreach into — any hit
                        // is real. A future continue-to-position feature must
                        // filter callback hits by `thread->GetPosition()` vs
                        // the requested limit; the `TtdPosition` ordering it
                        // would rely on is unit-tested in `ttd/types.rs`.

                        // Publish the cursor so another thread can interrupt
                        // this replay (^C). The guard clears the slot on drop,
                        // including when the callback panics, so a late
                        // interrupt is a no-op instead of poking a cursor that
                        // is no longer replaying.
                        let _published = active.publish(cursor);
                        replay_past_transparent_exceptions(cursor, metadata, limit, forward)
                    },
                )
            }))
        };
        let result = match outcome {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                // A failed replay may still have moved the physical cursor;
                // drop the position cache so the next query re-seeks instead
                // of reading from wherever the aborted replay stopped.
                self.cursors.persistent().invalidate();
                return Err(e.into());
            }
            Err(payload) => {
                // Same recovery for the panic path, then keep the panic
                // visible to the worker/caller.
                self.cursors.persistent().invalidate();
                std::panic::resume_unwind(payload);
            }
        };
        Ok((result, hit_bp_id.load(Ordering::Acquire)))
    }

    /// Translate a finished continue into a frontend stop reason.
    fn resolve_continue_stop(
        &self,
        result: &TtdReplayResult,
        hit_bp_id: u64,
        forward: bool,
    ) -> Result<StopReason, DebugError> {
        let bp = (hit_bp_id != 0)
            .then(|| self.breakpoints.get(hit_bp_id))
            .flatten();
        if let Some(reason) = classify_continue_stop(
            result,
            hit_bp_id,
            bp,
            forward,
            self.position,
            self.lifetime,
            self.ends_with_process_exit,
        ) {
            return Ok(reason);
        }
        Ok(self.enrich_exception(StopReason::try_from(result)?))
    }

    /// Single-step on the watchpoint-free step cursor.
    ///
    /// Steps deliberately do NOT use the persistent cursor: it carries the
    /// clients' watchpoints, and a step must always advance exactly one
    /// instruction regardless of registered breakpoints (GDB semantics: a
    /// step across a breakpoint does not re-trigger it). The step cursor is
    /// long-lived but positioned lazily, so consecutive steps — the common
    /// case, and the whole of a range step — need no repositioning at all.
    ///
    /// Exception mask: the same [`EXCEPTION_MASK`] as the continue path, so a
    /// step onto a faulting instruction reports the same stop a continue
    /// would. A single step cannot skip a first-chance event the way the
    /// continue loop does (there is exactly one instruction to report), so a
    /// step onto a C++ `throw` still stops there — that is the honest answer
    /// for a one-instruction request, and it is what keeps stepping over a
    /// `throw` from silently running to the trace boundary.
    pub(super) fn step_on_step_cursor(&mut self, backward: bool) -> Result<StopReason, DebugError> {
        let (result, pos, tid, seeked) = {
            let step = self.cursors.step();
            let seeked = step.prepare(self.position);
            let mut cursor = step.cursor_mut();
            // The exception/event masks are set once, when the cursor pair is
            // created (`CursorPair::new`); they are sticky.
            let result = if backward {
                cursor.replay_backward_steps(1)
            } else {
                cursor.replay_forward_steps(1)
            };
            let Ok(result) = result else {
                // A partially-executed step may have moved the physical
                // cursor; invalidate the cache so the next step re-seeks
                // instead of stepping from the wrong place.
                step.invalidate();
                return Err(TtdError::Ffi(if backward {
                    "step_backward failed"
                } else {
                    "step_forward failed"
                })
                .into());
            };
            (result, position_of(&cursor), current_tid(&cursor), seeked)
        };
        if seeked {
            self.bump(|s| s.cursor_seeks += 1);
        }
        self.cursors.step().note_position(pos);
        self.position = pos;
        self.bump(|s| s.steps += 1);
        // Unlike `adopt_cursor_position`, a 0 tid here *keeps* the previous
        // current thread: a step continues on the same thread from the same
        // position, so TTD reporting "no current thread" right after the
        // stepped instruction is a transient bookkeeping gap far more often
        // than a real teleport to where the old thread never existed. Only
        // goto/replay (which jump across the trace) clear the current thread
        // on a 0 tid. Keep the two sites' semantics distinct on purpose.
        if tid != 0 {
            self.current_thread_id = Some(u64::from(tid));
        }
        let reason = classify_continue_stop(
            &result,
            0,
            None,
            !backward,
            self.position,
            self.lifetime,
            self.ends_with_process_exit,
        )
        .unwrap_or(StopReason::try_from(&result)?);
        Ok(self.enrich_exception(reason))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ttd::types::{DataAccessMask, DataAccessType};

    fn pos(sequence: u64, steps: u64) -> TtdPosition {
        TtdPosition::new(sequence, steps)
    }

    fn lifetime() -> (TtdPosition, TtdPosition) {
        (pos(1, 0), pos(9, 1000))
    }

    /// A plain mid-trace step-count result with no watchpoint payload.
    fn step_result() -> TtdReplayResult {
        TtdReplayResult {
            stop_reason: EventType::StepCount,
            steps_executed: 1,
            instructions_executed: 1,
            wp_address: 0,
            wp_size: 0,
            wp_access_type: DataAccessType::Read,
        }
    }

    fn execute_bp(id: u64, addr: u64) -> Breakpoint {
        Breakpoint {
            id,
            addr,
            size: 1,
            access: DataAccessMask::EXECUTE,
            kind: BreakpointKind::Execute,
            thread_id: None,
        }
    }

    fn write_bp(id: u64, addr: u64) -> Breakpoint {
        Breakpoint {
            id,
            addr,
            size: 8,
            access: DataAccessMask::WRITE,
            kind: BreakpointKind::Write,
            thread_id: None,
        }
    }

    #[test]
    fn execute_breakpoint_hit_wins_over_trace_boundary() {
        // A hit at the very last position must still report the breakpoint,
        // not be folded into the boundary TraceEnd.
        let r = TtdReplayResult {
            wp_address: 0x5000,
            ..step_result()
        };
        let bp = execute_bp(7, 0x5000);
        let reason =
            classify_continue_stop(&r, 7, Some(&bp), true, lifetime().1, lifetime(), false);
        match reason {
            Some(StopReason::Breakpoint { bp_id, addr }) => {
                assert_eq!(bp_id, 7);
                assert_eq!(addr, 0x5000);
            }
            other => panic!("expected Breakpoint, got {other:?}"),
        }
    }

    #[test]
    fn data_breakpoint_hit_reports_watchpoint_with_bp_access() {
        let r = TtdReplayResult {
            wp_address: 0x3000,
            wp_access_type: DataAccessType::NewData,
            ..step_result()
        };
        let bp = write_bp(3, 0x3000);
        let reason = classify_continue_stop(&r, 3, Some(&bp), true, pos(5, 0), lifetime(), false);
        match reason {
            Some(StopReason::Watchpoint {
                bp_id,
                addr,
                access,
            }) => {
                assert_eq!(bp_id, 3);
                assert_eq!(addr, 0x3000);
                assert_eq!(
                    access,
                    DataAccessMask::WRITE,
                    "the logical breakpoint's access wins"
                );
            }
            other => panic!("expected Watchpoint, got {other:?}"),
        }
    }

    #[test]
    fn hit_with_missing_breakpoint_falls_back_to_replay_fields() {
        // Defensive arm: a nonzero id the manager no longer knows about
        // (shouldn't happen — the id comes from the manager itself).
        let r = TtdReplayResult {
            wp_address: 0x7777,
            wp_access_type: DataAccessType::Read,
            ..step_result()
        };
        let reason = classify_continue_stop(&r, 42, None, true, pos(5, 0), lifetime(), false);
        match reason {
            Some(StopReason::Watchpoint {
                bp_id,
                addr,
                access,
            }) => {
                assert_eq!(bp_id, 42);
                assert_eq!(addr, 0x7777);
                assert_eq!(access, DataAccessMask::READ);
            }
            other => panic!("expected Watchpoint, got {other:?}"),
        }
    }

    #[test]
    fn backward_stop_at_trace_start_is_trace_start() {
        let r = step_result();
        let reason = classify_continue_stop(&r, 0, None, false, lifetime().0, lifetime(), false);
        assert!(
            matches!(reason, Some(StopReason::TraceStart)),
            "got {reason:?}"
        );
    }

    #[test]
    fn forward_stop_at_trace_end_is_trace_end() {
        let r = step_result();
        let reason = classify_continue_stop(&r, 0, None, true, lifetime().1, lifetime(), false);
        assert!(
            matches!(reason, Some(StopReason::TraceEnd)),
            "got {reason:?}"
        );
    }

    /// A trace whose recording ends in a process exit must report it at the
    /// last position even though the replay event there is a plain boundary:
    /// TTD only surfaces `Process` when the cursor is replayed past the end,
    /// and a continue never does that.
    #[test]
    fn boundary_of_an_exiting_trace_is_a_process_exit() {
        let r = step_result();
        let reason = classify_continue_stop(&r, 0, None, true, lifetime().1, lifetime(), true);
        assert!(
            matches!(reason, Some(StopReason::ProcessExit)),
            "got {reason:?}"
        );
        // A breakpoint at that same position still wins, and a backward
        // boundary is still the trace start.
        let bp = execute_bp(4, 0x4000);
        let r = TtdReplayResult {
            wp_address: 0x4000,
            ..step_result()
        };
        assert!(matches!(
            classify_continue_stop(&r, 4, Some(&bp), true, lifetime().1, lifetime(), true),
            Some(StopReason::Breakpoint { .. })
        ));
        let r = step_result();
        assert!(matches!(
            classify_continue_stop(&r, 0, None, false, lifetime().0, lifetime(), true),
            Some(StopReason::TraceStart)
        ));
    }

    #[test]
    fn forward_process_exit_is_not_a_generic_trace_end() {
        let r = TtdReplayResult {
            stop_reason: EventType::Process,
            ..step_result()
        };
        let reason = classify_continue_stop(&r, 0, None, true, lifetime().1, lifetime(), false);
        assert!(
            matches!(reason, Some(StopReason::ProcessExit)),
            "got {reason:?}"
        );
    }

    #[test]
    fn boundary_check_is_directional() {
        let r = step_result();
        // A backward stop at the *end* of the trace is not TraceEnd...
        let reason = classify_continue_stop(&r, 0, None, false, lifetime().1, lifetime(), false);
        assert!(reason.is_none(), "got {reason:?}");
        // ...and a forward stop at the *start* is not TraceStart.
        let reason = classify_continue_stop(&r, 0, None, true, lifetime().0, lifetime(), false);
        assert!(reason.is_none(), "got {reason:?}");
    }

    #[test]
    fn mid_trace_stop_defers_to_exception_enrichment() {
        let r = step_result();
        let reason = classify_continue_stop(&r, 0, None, true, pos(5, 10), lifetime(), false);
        assert!(reason.is_none(), "got {reason:?}");
    }
}
