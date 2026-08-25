//! Replay internals for [`TtdProcess`](super::TtdProcess).
//!
//! Owns the filtered-continue path (TTD watchpoint callback + speculative
//! execution rules), position adoption after a stop, stop-reason resolution,
//! and the single-step path. The public `DebugTarget` impl lives in
//! [`super`]; these methods are implementation details shared by the
//! continue/step entry points.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::target::{BreakpointKind, DebugError, StopReason};
use crate::ttd::types::{TTD_Replay_EventMask_MemoryWatchpoint, TtdPosition, TtdReplayResult};

use super::TtdProcess;

impl TtdProcess {
    /// Enrich an `Exception` stop reason with the code/address recorded at
    /// the stop position (the replay result itself carries neither).
    fn enrich_exception(&self, reason: StopReason) -> StopReason {
        let StopReason::Exception { code: 0, .. } = reason else {
            return reason;
        };
        let pos = self.position;
        match self
            .engine
            .exception_list()
            .into_iter()
            .find(|ev| ev.position == pos)
        {
            Some(ev) => StopReason::Exception {
                code: ev.code,
                addr: ev.exception_address,
            },
            None => reason,
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
        self.adopt_replay_position();
        self.resolve_continue_stop(&result, hit_bp_id, forward)
    }

    /// Run one filtered continue and return `(replay result, matching
    /// breakpoint id or 0)`.
    ///
    /// Exception stops stay masked: Delve expects to "propagate" fault signals
    /// by resuming, so stopping on them here would loop. Only watchpoint
    /// events may stop a continue; trace boundaries are detected by
    /// [`Self::resolve_continue_stop`] afterwards.
    fn replay_watchpoint_filtered(
        &mut self,
        limit: TtdPosition,
        forward: bool,
    ) -> Result<(TtdReplayResult, u64), DebugError> {
        self.persistent(|c| c.set_exception_mask(0));
        // The persistent cursor may have been left anywhere by earlier
        // operations (steps run on the step cursor, which owns its own
        // position) — position it before replaying.
        self.at_position(self.position, |_| ());
        self.bump(|s| s.replays += 1);

        // Set from the watchpoint callback when a hit matches a registered
        // breakpoint. Atomic because the callback API is `Send`.
        let hit_bp_id = AtomicU64::new(0);
        // Disjoint field borrows: the callback needs to consult the
        // breakpoint table while it holds the cursor.
        let breakpoints = &self.breakpoints;
        let cursor = self.cursors.persistent_mut().cursor_mut();
        let active = self.active_replay.clone();
        let result = cursor.with_watchpoint_callback(
            &mut |addr, size, access, thread_id| {
                // The callback receives the OS thread id of the hitting
                // thread, so thread-filtered breakpoints (set_breakpoint with
                // thread_id) only fire on the thread they were registered for.
                if let Some(bp) = breakpoints.find_by_watchpoint_hit_and_thread(
                    addr,
                    size,
                    access,
                    thread_id as u64,
                ) {
                    hit_bp_id.store(bp.id, Ordering::Release);
                    true
                } else {
                    false
                }
            },
            |cursor| {
                cursor.set_event_mask(TTD_Replay_EventMask_MemoryWatchpoint);

                // Speculative-execution note: TTD's replay engine can
                // speculatively execute ahead of the cursor to discover the
                // next watchpoint hit, so a hit at a position beyond `limit`
                // may be surfaced. We always pass the *trace boundary* as
                // `limit` (continue forward -> lifetime end, continue
                // backward -> lifetime start), so there is no "future" to
                // overreach into — any hit is real. A future
                // continue-to-position feature must filter callback hits by
                // `thread->GetPosition()` vs the requested limit; the
                // `TtdPosition` ordering it would rely on is unit-tested
                // in `ttd/types.rs`.

                // Publish the raw cursor so another thread can interrupt the
                // replay (^C); clear before returning so a late interrupt is
                // a no-op instead of poking a dead pointer.
                active.store(cursor.as_raw(), Ordering::Release);
                let result = if forward {
                    cursor.replay_forward_to(limit)
                } else {
                    cursor.replay_backward_to(limit)
                };
                active.store(std::ptr::null_mut(), Ordering::Release);
                result
            },
        );
        let result = match result {
            Ok(r) => r,
            Err(e) => {
                // A failed replay may still have moved the physical cursor;
                // drop the position cache so the next query re-seeks instead
                // of reading from wherever the aborted replay stopped.
                self.cursors.persistent().invalidate();
                return Err(e.into());
            }
        };
        Ok((result, hit_bp_id.load(Ordering::Acquire)))
    }

    /// Make the stopped cursor position the debug position and adopt the
    /// thread that reached the stop event.
    fn adopt_replay_position(&mut self) {
        self.position = self.persistent(|c| c.position());
        self.cursors.persistent().note_position(self.position);
        let tid = self.persistent(|c| c.current_tid());
        if tid != 0 {
            self.current_thread_id = Some(tid as u64);
        }
    }

    /// Translate a finished continue into a frontend stop reason.
    fn resolve_continue_stop(
        &self,
        result: &TtdReplayResult,
        hit_bp_id: u64,
        forward: bool,
    ) -> Result<StopReason, DebugError> {
        if hit_bp_id != 0 {
            let bp = self.breakpoints.get(hit_bp_id);
            let addr = bp.map(|b| b.addr).unwrap_or(result.wp_address);
            return match bp {
                Some(b) if b.kind == BreakpointKind::Execute => Ok(StopReason::Breakpoint {
                    bp_id: hit_bp_id,
                    addr,
                }),
                // Data breakpoint hit (or an unregistered watchpoint): carry
                // the access mask so frontends can report Read/Write/ReadWrite.
                _ => Ok(StopReason::Watchpoint {
                    bp_id: hit_bp_id,
                    addr,
                    access: bp.map(|b| b.access).unwrap_or(result.wp_access_type),
                }),
            };
        }
        // Hitting a trace boundary is a stronger signal than the generic
        // event TTD reports for it: Delve expects sig 0 at trace start
        // (backward) and its synthetic SIGKILL at trace end (forward).
        if !forward && self.position <= self.lifetime.0 {
            return Ok(StopReason::TraceStart);
        }
        if forward && self.position >= self.lifetime.1 {
            return Ok(StopReason::TraceEnd);
        }
        Ok(self.enrich_exception(StopReason::try_from_replay_result(result)?))
    }

    /// Single-step on the watchpoint-free step cursor.
    ///
    /// Steps deliberately do NOT use the persistent cursor: it carries the
    /// clients' watchpoints, and a step must always advance exactly one
    /// instruction regardless of registered breakpoints (GDB semantics: a
    /// step across a breakpoint does not re-trigger it). The step cursor is
    /// long-lived but positioned lazily, so consecutive steps — the common
    /// case, and the whole of a range step — need no repositioning at all.
    pub(super) fn step_on_step_cursor(&mut self, backward: bool) -> Result<StopReason, DebugError> {
        let (result, pos, tid, seeked) = {
            let step = self.cursors.step();
            let seeked = step.prepare(self.position);
            let cursor = step.cursor();
            let result = if backward {
                cursor.step_backward(1)
            } else {
                cursor.step_forward(1)
            };
            let result = match result {
                Ok(r) => r,
                Err(e) => {
                    // A partially-executed step may have moved the physical
                    // cursor; invalidate the cache so the next step re-seeks
                    // instead of stepping from the wrong place.
                    step.invalidate();
                    return Err(e.into());
                }
            };
            (result, cursor.position(), cursor.current_tid(), seeked)
        };
        if seeked {
            self.bump(|s| s.cursor_seeks += 1);
        }
        self.cursors.step().note_position(pos);
        self.position = pos;
        self.bump(|s| s.steps += 1);
        if tid != 0 {
            self.current_thread_id = Some(tid as u64);
        }
        Ok(self.enrich_exception(StopReason::try_from_replay_result(&result)?))
    }
}
