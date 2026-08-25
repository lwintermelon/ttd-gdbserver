//! Multithread base ops and resume actions for [`GdbTarget`](super::GdbTarget).
//!
//! Covers `g`/`G`/`m`/`M`, `Hg`, thread listing (`qfThreadInfo`), the
//! `vCont` action state machine, single/range step and reverse
//! continue/step. Register serialization itself lives in
//! [`crate::gdb::regs`].

use std::num::NonZeroUsize;

use gdbstub::common::{Signal, Tid};
use gdbstub::target::ext::base::multithread::{
    MultiThreadBase, MultiThreadResume, MultiThreadSchedulerLocking, MultiThreadSingleStep,
};
use gdbstub::target::ext::base::reverse_exec::{ReverseCont, ReverseStep};
use gdbstub::target::ext::base::single_register_access::SingleRegisterAccess;
use gdbstub::target::{TargetError, TargetResult};

use crate::target::DebugTarget;

use super::GdbTarget;
use crate::gdb::regs::{TtdRegisters, read_register_value};
use crate::gdb::resume::ReplayOp;

// ─── Base ops (g / G / m / M / Hg / qfThreadInfo / qsThreadInfo / vCont) ──

impl<T: DebugTarget + Send + 'static> MultiThreadBase for GdbTarget<T> {
    fn read_registers(
        &mut self,
        regs: &mut <Self::Arch as gdbstub::arch::Arch>::Registers,
        tid: Tid,
    ) -> TargetResult<(), Self> {
        let backend = self.backend();
        let thread_id = tid.get() as u64;
        let (regs_val, teb) = backend
            .thread_state(Some(thread_id))
            .ok_or(TargetError::NonFatal)?;
        *regs = TtdRegisters::from_ttd(&regs_val, teb);
        Ok(())
    }

    fn write_registers(
        &mut self,
        _regs: &<Self::Arch as gdbstub::arch::Arch>::Registers,
        _tid: Tid,
    ) -> TargetResult<(), Self> {
        // TTD replay is read-only.
        Err(TargetError::NonFatal)
    }

    fn support_single_register_access(
        &mut self,
    ) -> Option<
        gdbstub::target::ext::base::single_register_access::SingleRegisterAccessOps<'_, Tid, Self>,
    > {
        Some(self)
    }

    fn read_addrs(
        &mut self,
        start_addr: u64,
        data: &mut [u8],
        _tid: Tid,
    ) -> TargetResult<usize, Self> {
        let backend = self.backend();
        let n = backend.read_memory(start_addr, data);
        if n == 0 && !data.is_empty() {
            return Err(TargetError::NonFatal);
        }
        Ok(n)
    }

    fn write_addrs(&mut self, _start_addr: u64, _data: &[u8], _tid: Tid) -> TargetResult<(), Self> {
        // TTD replay is read-only.
        Err(TargetError::NonFatal)
    }

    fn list_active_threads(
        &mut self,
        thread_is_active: &mut dyn FnMut(Tid),
    ) -> Result<(), Self::Error> {
        for tid in self.backend().active_thread_ids() {
            if let Some(tid) = NonZeroUsize::new(tid as usize) {
                thread_is_active(tid);
            }
        }
        Ok(())
    }

    fn support_thread_extra_info(
        &mut self,
    ) -> Option<gdbstub::target::ext::thread_extra_info::ThreadExtraInfoOps<'_, Self>> {
        Some(self)
    }

    fn support_resume(
        &mut self,
    ) -> Option<gdbstub::target::ext::base::multithread::MultiThreadResumeOps<'_, Self>> {
        Some(self)
    }
}

// ─── Single register access (p / P) ──────────────────────────────

impl<T: DebugTarget + Send + 'static> SingleRegisterAccess<Tid> for GdbTarget<T> {
    fn read_register(
        &mut self,
        tid: Tid,
        reg_id: <Self::Arch as gdbstub::arch::Arch>::RegId,
        buf: &mut [u8],
    ) -> TargetResult<usize, Self> {
        let backend = self.backend();
        let (regs_val, teb) = backend
            .thread_state(Some(tid.get() as u64))
            .ok_or(TargetError::NonFatal)?;
        let regs = TtdRegisters::from_ttd(&regs_val, teb);
        read_register_value(reg_id, &regs, buf).ok_or(TargetError::NonFatal)
    }

    fn write_register(
        &mut self,
        _tid: Tid,
        _reg_id: <Self::Arch as gdbstub::arch::Arch>::RegId,
        _val: &[u8],
    ) -> TargetResult<(), Self> {
        // TTD replay is read-only.
        Err(TargetError::NonFatal)
    }
}

// ─── Resume (vCont;c/s + reverse bc/bs + scheduler locking) ─────

// vCont action notes for replay:
//
// * `vCont;c[:tid]` — continue (we always replay to end-of-trace; per-tid
//   is a no-op because the trace model is single-threaded on the host).
// * `vCont;s[:tid]` — single step.
// * `vCont;r:start,end[:tid]` — range step (Phase 2a).
// * `vCont;C[:tid];sig` — continue with signal. We ignore the signal:
//   replay is read-only and the recorded signal stream is the only one
//   that matters.
// * `vCont;S[:tid];sig` — step with signal. Same as above (ignored).
// * `vCont;t[:tid]` — terminate-thread. Not meaningful for replay; gdbstub
//   0.7.10 returns a PacketUnexpected error to the client, which is the
//   cleanest answer (a real gdb doesn't send this for a replay target).
// * `vCont;T[:tid];sig` — terminate with signal. Parses as
//   ContinueWithSig; we already accept the signal argument and ignore
//   it via `set_resume_action_continue`.

impl<T: DebugTarget + Send + 'static> MultiThreadResume for GdbTarget<T> {
    fn resume(&mut self) -> Result<(), Self::Error> {
        // `take` resets to `Continue`, so a resume is never replayed twice.
        let op = self.resume.take();
        self.spawn_replay(op)
    }

    /// Drop every pending resume action. gdbstub calls this before applying a
    /// new vCont action list; every kind must go, or a range step survives
    /// into a later plain continue and the stub replays the wrong operation.
    fn clear_resume_actions(&mut self) -> Result<(), Self::Error> {
        self.resume.clear();
        Ok(())
    }

    fn set_resume_action_continue(
        &mut self,
        _tid: Tid,
        _signal: Option<Signal>,
    ) -> Result<(), Self::Error> {
        // Replay continues the whole trace: TTD replay is single-threaded on
        // the host, so a per-thread continue advances every thread anyway
        // (the same thing rr advertises). The signal is likewise ignored —
        // the recorded signal stream is the only one that matters.
        //
        // A continue must not clobber a pending step/range-step: plain GDB
        // sends mixed vCont lists like `vCont;s:p1.1;c` (step one thread,
        // continue the rest) and gdbstub applies the actions in wire order,
        // so the trailing wildcard continue would silently downgrade the
        // step. A replay target can only run one operation per resume, and
        // the step is the more specific request.
        self.resume.set_continue();
        Ok(())
    }

    fn support_single_step(
        &mut self,
    ) -> Option<gdbstub::target::ext::base::multithread::MultiThreadSingleStepOps<'_, Self>> {
        Some(self)
    }

    fn support_scheduler_locking(
        &mut self,
    ) -> Option<gdbstub::target::ext::base::multithread::MultiThreadSchedulerLockingOps<'_, Self>>
    {
        Some(self)
    }

    fn support_reverse_step(
        &mut self,
    ) -> Option<gdbstub::target::ext::base::reverse_exec::ReverseStepOps<'_, Tid, Self>> {
        Some(self)
    }

    fn support_reverse_cont(
        &mut self,
    ) -> Option<gdbstub::target::ext::base::reverse_exec::ReverseContOps<'_, Tid, Self>> {
        Some(self)
    }

    fn support_range_step(
        &mut self,
    ) -> Option<gdbstub::target::ext::base::multithread::MultiThreadRangeSteppingOps<'_, Self>>
    {
        Some(self)
    }
}

impl<T: DebugTarget + Send + 'static> MultiThreadSingleStep for GdbTarget<T> {
    fn set_resume_action_step(
        &mut self,
        tid: Tid,
        _signal: Option<Signal>,
    ) -> Result<(), Self::Error> {
        self.resume.set_step(tid);
        Ok(())
    }
}

impl<T: DebugTarget + Send + 'static>
    gdbstub::target::ext::base::multithread::MultiThreadRangeStepping for GdbTarget<T>
{
    fn set_resume_action_range_step(
        &mut self,
        tid: Tid,
        start: <Self::Arch as gdbstub::arch::Arch>::Usize,
        end: <Self::Arch as gdbstub::arch::Arch>::Usize,
    ) -> Result<(), Self::Error> {
        // TTD can't predict where the cursor will exit the range, so we
        // record the range and implement the resume as a single-step loop
        // in `spawn_replay`. `start == end` is treated as a single step.
        self.resume.set_range_step(tid, start, end);
        Ok(())
    }
}

impl<T: DebugTarget + Send + 'static> MultiThreadSchedulerLocking for GdbTarget<T> {
    fn set_resume_action_scheduler_lock(&mut self) -> Result<(), Self::Error> {
        // TTD replay always follows the recorded thread schedule; nothing to do.
        Ok(())
    }
}

impl<T: DebugTarget + Send + 'static> ReverseCont<Tid> for GdbTarget<T> {
    fn reverse_cont(&mut self) -> Result<(), Self::Error> {
        self.spawn_replay(ReplayOp::BackwardContinue)
    }
}

impl<T: DebugTarget + Send + 'static> ReverseStep<Tid> for GdbTarget<T> {
    fn reverse_step(&mut self, tid: Tid) -> Result<(), Self::Error> {
        self.spawn_replay(ReplayOp::BackwardStep(tid))
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for the resume-action state machine in [`super`].

    use std::num::NonZeroUsize;

    use gdbstub::target::ext::base::multithread::{MultiThreadResume, MultiThreadSingleStep};

    use crate::gdb::resume::ReplayOp;
    use crate::gdb::target::GdbTarget;
    use crate::gdb::target::test_util::Recorder;

    /// A pending resume action is consumed by `resume`, and
    /// `clear_resume_actions` must drop *every* kind — a range step that
    /// survived into a later plain continue would replay the wrong op.
    #[test]
    fn pending_resume_action_is_single_and_clearable() {
        use gdbstub::target::ext::base::multithread::MultiThreadRangeStepping;

        let mut t = GdbTarget::new(Recorder::default());
        let tid = NonZeroUsize::new(100).unwrap();

        // A range step is recorded, then dropped by the next clear.
        t.set_resume_action_range_step(tid, 0x1000, 0x2000).unwrap();
        assert_eq!(t.resume.pending(), ReplayOp::RangeStep(tid, 0x1000, 0x2000));
        t.clear_resume_actions().unwrap();
        assert_eq!(
            t.resume.pending(),
            ReplayOp::Continue,
            "range step must be cleared"
        );

        // An empty range is a plain step (GSP-defined).
        t.set_resume_action_range_step(tid, 0x1000, 0x1000).unwrap();
        assert_eq!(t.resume.pending(), ReplayOp::Step(tid));

        // A wildcard continue must not clobber a pending step/range-step:
        // plain GDB sends mixed vCont lists like `vCont;s:p1.1;c` and gdbstub
        // applies the actions in wire order, so the trailing continue would
        // silently downgrade the step. The step is the more specific request.
        t.set_resume_action_step(tid, None).unwrap();
        t.set_resume_action_continue(tid, None).unwrap();
        assert_eq!(
            t.resume.pending(),
            ReplayOp::Step(tid),
            "step must survive a trailing continue"
        );
        t.clear_resume_actions().unwrap();

        // A continue first, then a step: the step wins (last specific action).
        t.set_resume_action_continue(tid, None).unwrap();
        t.set_resume_action_step(tid, None).unwrap();
        assert_eq!(t.resume.pending(), ReplayOp::Step(tid));
        t.clear_resume_actions().unwrap();

        // A step is consumed once: the next resume (no new action) continues.
        t.set_resume_action_step(tid, None).unwrap();
        t.resume().unwrap();
        assert_eq!(
            t.resume.pending(),
            ReplayOp::Continue,
            "resume must consume the op"
        );
        t.wait_stop_reason();
        t.resume().unwrap();
        t.wait_stop_reason();

        let (steps, continues) = t.runner.query(|b| (b.steps, b.continues));
        assert_eq!((steps, continues), (1, 1));
    }
}
