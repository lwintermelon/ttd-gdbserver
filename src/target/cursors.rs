//! Lazy-positioned TTD cursors.
//!
//! A TTD `SetPosition` may replay from a keyframe, so `TtdProcess` remembers
//! where each long-lived cursor currently is and only repositions it when the
//! debug position actually moved. This module owns that bookkeeping so
//! `TtdProcess` can read as execution orchestration instead of repeating
//! `Cell<Option<...>>` checks around every FFI query.

use std::cell::{Cell, Ref, RefCell, RefMut};

use crate::ttd::{ReplayCursor, ReplayEngine};

use crate::ttd::error::TtdError;
use crate::ttd::types::{DataAccessMask, EventMask, TtdPosition, TtdReplayResult};

use super::process::EXCEPTION_MASK;

/// Position `cursor` at `pos` (TTD rounds to the nearest valid position).
pub(crate) fn seek(cursor: &mut ReplayCursor, pos: TtdPosition) {
    cursor.set_position(pos);
}

/// Replay `cursor` toward `limit`, stopping on a subscribed event.
pub(crate) fn replay_to(
    cursor: &mut ReplayCursor,
    limit: TtdPosition,
    forward: bool,
) -> crate::ttd::error::Result<TtdReplayResult> {
    if forward {
        cursor.replay_forward(limit)
    } else {
        cursor.replay_backward(limit)
    }
}

/// The cursor's current position, or the default when the engine has none.
pub(crate) fn position_of(cursor: &ReplayCursor) -> TtdPosition {
    cursor.position()
}

/// OS thread id of the cursor's current thread (0 if none).
pub(crate) fn current_tid(cursor: &ReplayCursor) -> u32 {
    cursor.thread_info().Id
}

/// A cursor plus the last position it is known to be at.
///
/// The cursor is boxed: `InterruptSlot::publish` hands its address to other
/// threads, so it must not move when the owning `TtdProcess` does.
pub(crate) struct PositionedCursor {
    cursor: Box<RefCell<ReplayCursor>>,
    /// `None` means "unknown, must seek before next use".
    at: Cell<Option<TtdPosition>>,
}

/// Clears the installed watchpoint callback when it goes out of scope.
struct CallbackGuard<'a>(RefMut<'a, ReplayCursor>);

impl Drop for CallbackGuard<'_> {
    fn drop(&mut self) {
        self.0.clear_watchpoint_callback();
    }
}

impl PositionedCursor {
    fn new(cursor: ReplayCursor) -> Self {
        Self {
            cursor: Box::new(RefCell::new(cursor)),
            at: Cell::new(None),
        }
    }

    /// Shared borrow for read-only queries.
    pub(crate) fn cursor(&self) -> Ref<'_, ReplayCursor> {
        self.cursor.borrow()
    }

    /// Exclusive borrow for navigation and watchpoint changes.
    pub(crate) fn cursor_mut(&self) -> RefMut<'_, ReplayCursor> {
        self.cursor.borrow_mut()
    }

    /// The cursor's cell, for callers that borrow it themselves.
    pub(crate) fn cell(&self) -> &RefCell<ReplayCursor> {
        self.cursor.as_ref()
    }

    /// Ensure the cursor is at `pos`, seeking when the cache says otherwise.
    ///
    /// The position is read back after `SetPosition` because TTD rounds to
    /// the nearest valid position; caching the requested value would make the
    /// next query seek forever.
    ///
    /// Returns `true` when a seek was actually issued, so the caller can count
    /// it.
    pub(crate) fn prepare(&self, pos: TtdPosition) -> bool {
        if self.at.get() == Some(pos) {
            return false;
        }
        let mut cursor = self.cursor_mut();
        seek(&mut cursor, pos);
        self.at.set(Some(position_of(&cursor)));
        true
    }

    /// Forget the cached position after a failed/partial cursor operation.
    pub(crate) fn invalidate(&self) {
        self.at.set(None);
    }

    /// Record the actual landing position after a successful move.
    pub(crate) fn note_position(&self, pos: TtdPosition) {
        self.at.set(Some(pos));
    }

    /// Run `f` with a temporary watchpoint callback.
    ///
    /// The callback may borrow local state; it is uninstalled before this
    /// method returns — even if `f` or the callback panics.
    pub(crate) fn with_watchpoint_callback<R>(
        &self,
        cb: &mut (dyn FnMut(u64, u64, DataAccessMask, u32) -> bool + Send),
        f: impl FnOnce(&mut ReplayCursor) -> R,
    ) -> R {
        let mut cursor = self.cursor_mut();
        cursor.install_borrowed_callback(cb);
        let mut guard = CallbackGuard(cursor);
        f(&mut guard.0)
    }
}

/// The persistent query cursor and the watchpoint-free step cursor.
pub(crate) struct CursorPair {
    persistent: PositionedCursor,
    step: PositionedCursor,
}

impl CursorPair {
    pub(crate) fn new(engine: &ReplayEngine) -> Result<Self, TtdError> {
        let pair = Self {
            persistent: PositionedCursor::new(
                engine
                    .cursor()
                    .map_err(|_| TtdError::CursorCreationFailed)?,
            ),
            step: PositionedCursor::new(
                engine
                    .cursor()
                    .map_err(|_| TtdError::CursorCreationFailed)?,
            ),
        };
        // A step must report the fault it lands on instead of completing
        // silently, and exceptions are the only event a step subscribes to.
        // Both masks are sticky, so this is a one-time setup — a range step is
        // one `StepForward` per instruction, and re-setting them per
        // instruction would be pure FFI overhead on the hottest loop.
        {
            let mut step = pair.step.cursor_mut();
            step.set_exception_mask(EXCEPTION_MASK);
            step.set_event_mask(EventMask::EXCEPTION);
        }
        Ok(pair)
    }

    /// Cursor that carries client watchpoints and serves all queries.
    pub(crate) fn persistent(&self) -> &PositionedCursor {
        &self.persistent
    }

    /// Cursor used for single steps; never carries watchpoints.
    pub(crate) fn step(&self) -> &PositionedCursor {
        &self.step
    }
}
