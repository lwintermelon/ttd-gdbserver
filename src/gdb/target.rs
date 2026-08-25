//! `gdbstub::Target` implementation over the `DebugTarget` trait.
//!
//! This is where the GDB Remote Serial Protocol is mapped onto the target:
//! registers/memory/threads through the base ops, breakpoints/watchpoints as
//! TTD watchpoints, forward/reverse execution as TTD replay, and
//! Delve-specific packets (`qRRCmd` when/checkpoints, ...) through the
//! `UnknownPacket` extension.
//!
//! Submodules split the implementation by protocol area:
//! `base` (registers/memory/threads/resume), `breakpoints`,
//! `inspect` (process description and `monitor`), and `unknown`
//! (Delve-specific packets).

use gdbstub::common::Signal;
use gdbstub::stub::MultiThreadStopReason;
use gdbstub::target::Target;
use gdbstub::target::ext::base::BaseOps;

use crate::target::{DebugError, DebugTarget};

use super::checkpoint::CheckpointTable;
use super::mapping::{map_stop_reason, stop_tid};
use super::regs::TtdArch;
use super::resume::{ReplayOp, ResumeState};
use super::runner::{TargetRunner, WorkerPanic};

mod base;
mod breakpoints;
mod inspect;
mod unknown;

#[cfg(test)]
mod test_util;
/// `gdbstub::Target` backed by a `DebugTarget` backend.
///
/// Pure protocol mapping: all threading (replay workers, stop-reason
/// delivery, ^C cancellation) lives in [`TargetRunner`]. Resume operations
/// run on a worker thread while the gdbstub event loop keeps polling the
/// connection, so a long replay can be interrupted from the wire at any
/// time.
pub struct GdbTarget<T: DebugTarget + Send + 'static> {
    runner: TargetRunner<T, MultiThreadStopReason<u64>>,

    /// The vCont action to perform on the next `resume` (only touched while
    /// the stub is stopped).
    resume: ResumeState,

    /// Emulated rr checkpoints (id -> saved position).
    checkpoints: CheckpointTable,
}

impl<T: DebugTarget + Send + 'static> GdbTarget<T> {
    pub fn new(backend: T) -> Self {
        // The runner picks up the backend's own interrupt handle (^C wiring),
        // so callers never thread one through manually.
        Self {
            runner: TargetRunner::new(backend),
            resume: ResumeState::default(),
            checkpoints: CheckpointTable::new(),
        }
    }

    /// Interrupt the in-flight replay (from the gdbstub event loop).
    pub fn interrupt_replay(&self) {
        self.runner.request_interrupt();
    }

    /// Whether a replay worker is currently running.
    pub fn replay_in_flight(&self) -> bool {
        self.runner.in_flight()
    }

    /// Recover the backend (for state persistence across sessions).
    pub fn into_backend(self) -> Option<T> {
        self.runner.into_backend()
    }

    /// Take the stop reason a worker reported, if any.
    pub fn take_stop_reason(&self) -> Option<MultiThreadStopReason<u64>> {
        self.map_worker_result(self.runner.take_result())
    }

    /// Wait (blocking) for the worker to report a stop reason.
    pub fn wait_stop_reason(&self) -> Option<MultiThreadStopReason<u64>> {
        self.map_worker_result(self.runner.wait_result())
    }

    /// Map a worker outcome onto the protocol stop vocabulary. A panicking
    /// backend becomes a terminal SIGTRAP stop: the worker has already cleared
    /// the in-flight flag and released the backend lock, so we can query the
    /// current thread without contending with a dead worker.
    fn map_worker_result(
        &self,
        result: Option<Result<MultiThreadStopReason<u64>, WorkerPanic>>,
    ) -> Option<MultiThreadStopReason<u64>> {
        match result {
            None => None,
            Some(Ok(reason)) => Some(reason),
            Some(Err(panic)) => {
                log::error!("replay worker panicked: {panic}");
                let backend = self.backend();
                Some(MultiThreadStopReason::SignalWithThread {
                    tid: stop_tid(&*backend),
                    signal: Signal::SIGTRAP,
                })
            }
        }
    }

    fn backend(&self) -> std::sync::MutexGuard<'_, T> {
        self.runner.lock()
    }

    /// Whether the backend can be locked without waiting on a replay.
    ///
    /// `TargetRunner` clears its in-flight flag only *after* the worker has
    /// released the backend lock, so `false` here implies the lock is free —
    /// the caller will not contend.
    fn backend_available(&self) -> bool {
        !self.replay_in_flight()
    }

    /// Start a replay on a worker thread. Returns immediately.
    fn spawn_replay(&mut self, op: ReplayOp) -> Result<(), DebugError> {
        log::debug!("spawn_replay op={:?}", op);
        self.runner
            .start(
                move |b| {
                    let backend_reason = match op {
                        ReplayOp::Continue => b.continue_forward(),
                        ReplayOp::Step(tid) => {
                            b.set_current_thread(tid.get() as u64);
                            b.step()
                        }
                        ReplayOp::BackwardContinue => b.continue_backward(),
                        ReplayOp::BackwardStep(tid) => {
                            b.set_current_thread(tid.get() as u64);
                            b.step_back()
                        }
                        ReplayOp::RangeStep(tid, start, end) => {
                            // The backend owns the step loop (and its PC reads)
                            // so a target with a reusable step cursor can avoid
                            // a query-cursor seek per instruction.
                            b.set_current_thread(tid.get() as u64);
                            b.step_range(Some(tid.get() as u64), start, end)
                        }
                    };
                    let stop = map_stop_reason(b, backend_reason);
                    log::debug!(
                        "replay worker done: stop={:?} position={:?}",
                        stop,
                        b.position()
                    );
                    stop
                },
                // A ^C that aborted the blocking replay surfaces as SIGINT
                // instead of whatever the interrupted op would have reported.
                |b| MultiThreadStopReason::SignalWithThread {
                    tid: stop_tid(b),
                    signal: Signal::SIGINT,
                },
            )
            .map_err(|_| DebugError::ReplayAlreadyRunning)
    }

    fn exe_module_base(&self) -> Option<u64> {
        // The first module of the trace is the main executable.
        self.backend().modules().first().map(|m| m.base_addr)
    }
}

// ─── Target ──────────────────────────────────────────────────────

impl<T: DebugTarget + Send + 'static> Target for GdbTarget<T> {
    type Arch = TtdArch;
    type Error = DebugError;

    fn base_ops(&mut self) -> BaseOps<'_, Self::Arch, Self::Error> {
        BaseOps::MultiThread(self)
    }

    fn use_no_ack_mode(&self) -> bool {
        true
    }

    fn use_target_description_xml(&self) -> bool {
        true
    }

    fn use_x_lowcase_packet(&self) -> bool {
        true
    }

    fn use_rle(&self) -> bool {
        // gdbstub's RLE compresses runs of repeated bytes with `*`, but
        // Delve's binary packet decoder (binarywiredecode) does not handle
        // run-length encoding — binary qXfer payloads like auxv would be
        // mangled. Our responses are small, so RLE buys nothing.
        false
    }

    fn guard_rail_implicit_sw_breakpoints(&self) -> bool {
        true
    }

    fn support_breakpoints(
        &mut self,
    ) -> Option<gdbstub::target::ext::breakpoints::BreakpointsOps<'_, Self>> {
        Some(self)
    }

    fn support_extended_mode(
        &mut self,
    ) -> Option<gdbstub::target::ext::extended_mode::ExtendedModeOps<'_, Self>> {
        Some(self)
    }

    fn support_exec_file(
        &mut self,
    ) -> Option<gdbstub::target::ext::exec_file::ExecFileOps<'_, Self>> {
        Some(self)
    }

    fn support_auxv(&mut self) -> Option<gdbstub::target::ext::auxv::AuxvOps<'_, Self>> {
        Some(self)
    }

    fn support_process_info(
        &mut self,
    ) -> Option<gdbstub::target::ext::process_info::ProcessInfoOps<'_, Self>> {
        Some(self)
    }

    fn support_libraries(
        &mut self,
    ) -> Option<gdbstub::target::ext::libraries::LibrariesOps<'_, Self>> {
        Some(self)
    }

    fn support_memory_map(
        &mut self,
    ) -> Option<gdbstub::target::ext::memory_map::MemoryMapOps<'_, Self>> {
        Some(self)
    }

    fn support_section_offsets(
        &mut self,
    ) -> Option<gdbstub::target::ext::section_offsets::SectionOffsetsOps<'_, Self>> {
        Some(self)
    }

    fn support_monitor_cmd(
        &mut self,
    ) -> Option<gdbstub::target::ext::monitor_cmd::MonitorCmdOps<'_, Self>> {
        Some(self)
    }

    fn support_unknown_packet(
        &mut self,
    ) -> Option<gdbstub::target::ext::unknown_packet::UnknownPacketOps<'_, Self>> {
        Some(self)
    }

    fn support_target_description_xml_override(
        &mut self,
    ) -> Option<
        gdbstub::target::ext::target_description_xml_override::TargetDescriptionXmlOverrideOps<
            '_,
            Self,
        >,
    > {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for [`GdbTarget`]'s module-level behavior (worker results and
    //! resume plumbing); protocol-area tests live next to their submodule.

    use gdbstub::common::Signal;
    use gdbstub::stub::MultiThreadStopReason;

    use crate::gdb::resume::ReplayOp;

    use super::GdbTarget;
    use super::test_util::Recorder;

    /// A panicking backend must surface as a SIGTRAP stop, not a wedged event
    /// loop. `TargetRunner` clears the in-flight flag and reports the panic
    /// through the stop channel; `GdbTarget::wait_stop_reason` translates it.
    #[test]
    fn worker_panic_surfaces_as_sigtrap_stop() {
        let mut target = GdbTarget::new(Recorder {
            panics: true,
            ..Default::default()
        });
        target.spawn_replay(ReplayOp::Continue).unwrap();

        match target.wait_stop_reason() {
            Some(MultiThreadStopReason::SignalWithThread { signal, tid }) => {
                assert_eq!(signal, Signal::SIGTRAP, "panic must be a target error stop");
                assert_eq!(tid.get(), 100, "stop must carry the current thread id");
            }
            other => panic!("panic outcome must map to a stop reason, got {other:?}"),
        }
        assert!(!target.replay_in_flight());
    }
}
