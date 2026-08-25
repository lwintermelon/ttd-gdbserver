//! Resume-action state machine (`vCont`, reverse resume, range step).
//!
//! gdbstub first clears the pending action, then applies zero or more
//! `set_resume_action_*` calls in wire order, then calls `resume`. This module
//! owns that small state machine so `GdbTarget` only has to start the chosen
//! operation on its worker.

use gdbstub::common::Tid;

/// What kind of replay the next `resume` should perform.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(super) enum ReplayOp {
    /// `vCont;c` (also the default when no per-thread action was set).
    #[default]
    Continue,
    Step(Tid),
    BackwardContinue,
    BackwardStep(Tid),
    /// Range step: replay forward until the cursor leaves `[start, end)`.
    RangeStep(Tid, u64, u64),
}

/// Pending resume action state.
#[derive(Debug, Default)]
pub(super) struct ResumeState {
    pending: ReplayOp,
}

impl ResumeState {
    /// Consume the pending action; the next `resume` without new actions is a
    /// plain continue.
    pub(super) fn take(&mut self) -> ReplayOp {
        std::mem::take(&mut self.pending)
    }

    /// Drop every pending action. gdbstub calls this before applying a new
    /// `vCont` list; a range step that survived into a later plain continue
    /// would replay the wrong operation.
    pub(super) fn clear(&mut self) {
        self.pending = ReplayOp::Continue;
    }

    /// Apply `vCont;c`. A wildcard continue must not clobber a pending
    /// step/range-step: plain GDB sends mixed lists like
    /// `vCont;s:p1.1;c` and the more specific action is the intended one.
    pub(super) fn set_continue(&mut self) {
        // Intentionally a no-op: a pending Step/RangeStep is more specific
        // and target replay can only run one operation per resume.
    }

    pub(super) fn set_step(&mut self, tid: Tid) {
        self.pending = ReplayOp::Step(tid);
    }

    /// `start == end` is a plain single step; TTD cannot predict where the
    /// cursor would leave an empty range.
    pub(super) fn set_range_step(&mut self, tid: Tid, start: u64, end: u64) {
        self.pending = if start == end {
            ReplayOp::Step(tid)
        } else {
            ReplayOp::RangeStep(tid, start, end)
        };
    }

    #[cfg(test)]
    pub(super) fn pending(&self) -> ReplayOp {
        self.pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tid() -> Tid {
        std::num::NonZeroUsize::new(100).unwrap()
    }

    #[test]
    fn default_take_is_continue_and_take_consumes() {
        let mut state = ResumeState::default();
        assert_eq!(state.take(), ReplayOp::Continue);
        assert_eq!(state.take(), ReplayOp::Continue);
    }

    #[test]
    fn clear_drops_every_action_kind() {
        let mut state = ResumeState::default();
        state.set_range_step(tid(), 0x1000, 0x2000);
        assert_eq!(state.pending(), ReplayOp::RangeStep(tid(), 0x1000, 0x2000));
        state.clear();
        assert_eq!(state.take(), ReplayOp::Continue);

        state.set_step(tid());
        state.clear();
        assert_eq!(state.take(), ReplayOp::Continue);
    }

    #[test]
    fn wildcard_continue_preserves_the_more_specific_action() {
        let mut state = ResumeState::default();

        state.set_step(tid());
        state.set_continue();
        assert_eq!(state.pending(), ReplayOp::Step(tid()));

        state.clear();
        state.set_range_step(tid(), 0x1000, 0x2000);
        state.set_continue();
        assert_eq!(state.pending(), ReplayOp::RangeStep(tid(), 0x1000, 0x2000));

        // A continue followed by a step still ends in the step.
        state.clear();
        state.set_continue();
        state.set_step(tid());
        assert_eq!(state.pending(), ReplayOp::Step(tid()));
    }

    #[test]
    fn empty_range_is_a_plain_step() {
        let mut state = ResumeState::default();
        state.set_range_step(tid(), 0x1000, 0x1000);
        assert_eq!(state.pending(), ReplayOp::Step(tid()));
    }
}
