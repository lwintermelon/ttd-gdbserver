//! Lazy-positioned TTD cursors.
//!
//! A TTD `SetPosition` may replay from a keyframe, so `TtdProcess` remembers
//! where each long-lived cursor currently is and only repositions it when the
//! debug position actually moved. This module owns that bookkeeping so
//! `TtdProcess` can read as execution orchestration instead of repeating
//! `Cell<Option<...>>` checks around every FFI query.

use std::cell::Cell;

use crate::ttd::types::TtdPosition;
use crate::ttd::{TtdCursor, TtdEngine, TtdError};

/// A cursor plus the last position it is known to be at.
pub(crate) struct PositionedCursor {
    cursor: Box<TtdCursor>,
    /// `None` means "unknown, must seek before next use".
    at: Cell<Option<TtdPosition>>,
}

impl PositionedCursor {
    fn new(cursor: TtdCursor) -> Self {
        Self {
            cursor: Box::new(cursor),
            at: Cell::new(None),
        }
    }

    pub(crate) fn cursor(&self) -> &TtdCursor {
        &self.cursor
    }

    pub(crate) fn cursor_mut(&mut self) -> &mut TtdCursor {
        &mut self.cursor
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
        self.cursor.set_position(pos);
        self.at.set(Some(self.cursor.position()));
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
}

/// The persistent query cursor and the watchpoint-free step cursor.
pub(crate) struct CursorPair {
    persistent: PositionedCursor,
    step: PositionedCursor,
}

impl CursorPair {
    pub(crate) fn new(engine: &TtdEngine) -> Result<Self, TtdError> {
        Ok(Self {
            persistent: PositionedCursor::new(engine.create_cursor()?),
            step: PositionedCursor::new(engine.create_cursor()?),
        })
    }

    /// Cursor that carries client watchpoints and serves all queries.
    pub(crate) fn persistent(&self) -> &PositionedCursor {
        &self.persistent
    }

    pub(crate) fn persistent_mut(&mut self) -> &mut PositionedCursor {
        &mut self.persistent
    }

    /// Cursor used for single steps; never carries watchpoints.
    pub(crate) fn step(&self) -> &PositionedCursor {
        &self.step
    }
}
