//! `gdbstub::Target` implementation over the `DebugTarget` trait.
//!
//! This is where the GDB Remote Serial Protocol is mapped onto the target:
//! registers/memory/threads through the base ops, breakpoints/watchpoints as
//! TTD watchpoints, forward/reverse execution as TTD replay, and
//! Delve-specific packets (`qRRCmd` when/checkpoints, ...) through the
//! `UnknownPacket` extension.
//!
//! Submodules split the implementation by protocol area:
//! `base` (registers/memory/threads/resume), `breakpoints`, `extended`
//! (`vRun`/`R` restart), `description` (exec-file/auxv/section
//! offsets/target XML), `memory_map` (library list), `thread_info`
//! (`qThreadExtraInfo`), `monitor_cmd` (`monitor`), and `unknown`
//! (Delve-specific packets).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, MutexGuard};

use gdbstub::common::{Pid, Signal};
use gdbstub::stub::MultiThreadStopReason;
use gdbstub::target::ext::base::BaseOps;
use gdbstub::target::{Target, TargetError, TargetResult};

use crate::target::{DebugError, DebugTarget, StopReason};

use super::checkpoint::CheckpointTable;
use super::mapping::{from_wire_tid, map_stop_reason, stop_tid};
use super::pe;
use super::regs::TtdArch;
use super::resume::{ReplayOp, ResumeState};
use super::runner::{TargetRunner, WorkerPanic};

mod base;
mod breakpoints;
mod description;
mod extended;
mod memory_map;
mod monitor_cmd;
mod thread_info;
mod unknown;

#[cfg(test)]
mod test_support;

/// The single process id this stub advertises. Replay has no `fork`, so a
/// constant is the honest model; `Pid` is non-zero by construction.
fn server_pid() -> Pid {
    Pid::new(1).expect("1 is a valid GDB pid")
}

/// `gdbstub::Target` backed by a `DebugTarget` backend.
///
/// Pure protocol mapping: all threading (replay workers, stop-reason
/// delivery, ^C cancellation) lives in [`TargetRunner`]. Resume operations
/// run on a worker thread while the gdbstub event loop keeps polling the
/// connection, so a long replay can be interrupted from the wire at any
/// time.
pub struct GdbTarget<T: DebugTarget + Send + 'static> {
    runner: TargetRunner<T, MultiThreadStopReason<u64>>,

    /// Runtime address of the main executable's `.text` section, snapshotted
    /// at construction. GDB's `qOffsets` `Segments` form expects the text
    /// segment address, not the image base; the module list is immutable, so
    /// this can answer correctly even while a replay holds the backend.
    text_seg: Option<u64>,

    /// The `qXfer:libraries` payload, built once at construction.
    ///
    /// The module list is immutable for a loaded trace and building the XML
    /// reads every module's PE section table from disk, so rebuilding it per
    /// chunk would repeat that I/O for a payload that cannot change (GDB reads
    /// it in ~4 KiB chunks).
    libraries_xml: String,

    /// The vCont action to perform on the next `resume` (only touched while
    /// the stub is stopped).
    resume: ResumeState,

    /// rr's `in_debuggee_end_state` (`GdbServer.cc`): set when a forward
    /// replay reports the recorded process exit (which the client sees as
    /// the synthetic SIGKILL stop). While set, a forward resume does not
    /// replay again — it is answered with the real exit packet `W00`, rr's
    /// `handle_exited_state` — while a backward resume or a restart clears
    /// the flag and replays normally, so the exit stop stays rewindable.
    /// Shared with the replay worker, which is where the backend's stop
    /// reason is known.
    end_state: Arc<AtomicBool>,

    /// A `vRun`/`R` restart moved the backend to an explicit position. The
    /// client always follows a restart with a resume (Delve does it
    /// unconditionally, "because that's what gdb does"), and rr answers that
    /// resume at the restart position instead of replaying forward. Without
    /// this the restart would immediately run to the trace boundary and the
    /// checkpoint would be lost.
    restart_pending: bool,

    /// Emulated rr checkpoints (id -> saved position). Owned by the session
    /// layer and only borrowed for one session's lifetime: like position and
    /// breakpoints, checkpoints are trace-global state, so a client that
    /// reconnects must still see them (`restart c1` after a drop must not
    /// silently fall back to the trace start).
    checkpoints: CheckpointTable,
}

impl<T: DebugTarget + Send + 'static> GdbTarget<T> {
    pub fn new(backend: T, checkpoints: CheckpointTable) -> Self {
        // The module list is immutable; snapshot the runtime address of the
        // main executable's `.text` section for qOffsets. Prefer the real PE
        // section RVA; fall back to the conventional 0x1000 when the recorded
        // executable is not available on this machine.
        let text_seg = backend.main_module().map(|main| {
            let rva = pe::text_rva(Path::new(&main.name)).unwrap_or(pe::DEFAULT_TEXT_RVA);
            main.base_addr.saturating_add(u64::from(rva))
        });
        // The main executable is not a shared library: GDB already has it from
        // `qXfer:exec-file` / the client's `file` command, and listing the
        // trace's recorded path again would make GDB re-open it.
        let main = backend.main_module();
        let shared: Vec<_> = backend
            .modules()
            .into_iter()
            .filter(|m| {
                main.as_ref()
                    .is_none_or(|main| main.base_addr != m.base_addr || main.name != m.name)
            })
            .collect();
        let libraries_xml = crate::gdb::xml::build_libraries_xml(&shared);
        // The runner picks up the backend's own interrupt handle (^C wiring),
        // so callers never thread one through manually.
        Self {
            runner: TargetRunner::new(backend),
            text_seg,
            libraries_xml,
            resume: ResumeState::default(),
            end_state: Arc::new(AtomicBool::new(false)),
            restart_pending: false,
            checkpoints,
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

    /// Recover the backend and the checkpoint table (both persist across
    /// sessions; see [`super::session::serve_sessions`]).
    pub fn into_parts(self) -> (Option<T>, CheckpointTable) {
        (self.runner.into_backend(), self.checkpoints)
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
                let backend = self.blocking_backend();
                Some(MultiThreadStopReason::SignalWithThread {
                    tid: stop_tid(&*backend),
                    signal: Signal::SIGTRAP,
                })
            }
        }
    }

    /// Blocking backend access for paths that are guaranteed to run only
    /// while no replay worker holds the lock (worker-result mapping after the
    /// worker published its outcome). Protocol query handlers use
    /// [`Self::try_backend`] instead.
    fn blocking_backend(&self) -> std::sync::MutexGuard<'_, T> {
        self.runner.lock()
    }

    /// Non-blocking backend access for query packets.
    ///
    /// gdbstub processes an arbitrary packet that arrives while the target is
    /// running; blocking on the replay worker's lock here would park the
    /// event loop and with it the ^C path. A running target answers `E` to
    /// such queries instead. The worker clears `replay_running` only after
    /// releasing the lock, so `in_flight() == false` guarantees the lock is
    /// free; a true flag may conservatively report busy for a moment after
    /// the worker dropped its guard.
    fn try_backend(&self) -> TargetResult<MutexGuard<'_, T>, Self> {
        self.runner.try_lock().ok_or(TargetError::NonFatal)
    }

    /// Start a replay on a worker thread. Returns immediately.
    fn spawn_replay(&mut self, op: ReplayOp) -> Result<(), DebugError> {
        log::debug!("spawn_replay op={op:?}");
        // A restart is always followed by a resume on the wire. rr stops that
        // resume at the restart position; mirroring it keeps `restart <pos>`
        // and `restart cN` at the requested position instead of replaying
        // straight to the trace boundary.
        if self.restart_pending {
            self.restart_pending = false;
            if op.is_forward() {
                return self.spawn_position_stop();
            }
        }
        // End state (rr's `in_debuggee_end_state`): the recorded process exit
        // was already reported as the synthetic SIGKILL stop, so a forward
        // resume must not replay again — it reports the real exit (`W00`),
        // which is the client's cue that forward execution is over. Any
        // backward op leaves the end state and replays normally.
        if self.end_state.load(Ordering::Acquire) {
            if op.is_forward() {
                return self.spawn_exit_reply();
            }
            self.end_state.store(false, Ordering::Release);
        }
        let end_state = Arc::clone(&self.end_state);
        let forward = op.is_forward();
        self.runner
            .start(
                move |b| {
                    let backend_reason = execute_op(op, b);
                    if forward && matches!(backend_reason, Ok(StopReason::ProcessExit)) {
                        end_state.store(true, Ordering::Release);
                    }
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

    /// Answer a forward resume from the end state with the real process exit
    /// (rr's `handle_exited_state`: `W00`; like rr, the recorded exit code is
    /// not reported). Runs on the worker like any other stop so the event
    /// loop's stop-reason plumbing is unchanged.
    fn spawn_exit_reply(&mut self) -> Result<(), DebugError> {
        self.runner
            .start(
                |_| MultiThreadStopReason::Exited(0),
                |_| MultiThreadStopReason::Exited(0),
            )
            .map_err(|_| DebugError::ReplayAlreadyRunning)
    }

    /// Leave the end state without replaying: a restart (`vRun` / `R`) moved
    /// the backend to a position where forward execution is possible again.
    pub(super) fn clear_end_state(&self) {
        self.end_state.store(false, Ordering::Release);
    }

    /// The backend was moved by a restart; the next forward resume stops
    /// there instead of replaying (see [`Self::spawn_replay`]).
    pub(super) fn note_restart(&mut self) {
        self.restart_pending = true;
    }

    /// Answer the resume that follows a restart with a stop at the current
    /// position. Runs on the worker like any other stop so the event loop's
    /// plumbing is unchanged; the backend is only read for its thread id.
    fn spawn_position_stop(&mut self) -> Result<(), DebugError> {
        self.runner
            .start(
                |b| MultiThreadStopReason::SignalWithThread {
                    tid: stop_tid(b),
                    signal: Signal::SIGTRAP,
                },
                |b| MultiThreadStopReason::SignalWithThread {
                    tid: stop_tid(b),
                    signal: Signal::SIGTRAP,
                },
            )
            .map_err(|_| DebugError::ReplayAlreadyRunning)
    }
}

// ─── Resume-op dispatch ──────────────────────────────────────────

/// Run one resume op against the backend, applying the op's thread selection
/// first so the stop reply and later queries observe the stepped thread.
///
/// A free function rather than a `ReplayOp` method on purpose:
/// [`super::resume`] stays a pure vCont state machine with no backend
/// dependency, and the dispatch is unit-testable against a plain
/// [`DebugTarget`] stub.
fn execute_op<T: DebugTarget + ?Sized>(
    op: ReplayOp,
    b: &mut T,
) -> Result<crate::target::StopReason, DebugError> {
    match op {
        ReplayOp::Continue => b.continue_forward(),
        ReplayOp::Step(tid) => {
            b.set_current_thread(from_wire_tid(tid));
            b.step()
        }
        ReplayOp::BackwardContinue => b.continue_backward(),
        ReplayOp::BackwardStep(tid) => {
            b.set_current_thread(from_wire_tid(tid));
            b.step_back()
        }
        ReplayOp::RangeStep(tid, start, end) => {
            // The backend owns the step loop (and its PC reads) so a target
            // with a reusable step cursor can avoid a query-cursor seek per
            // instruction.
            let tid = from_wire_tid(tid);
            b.set_current_thread(tid);
            b.step_range(Some(tid), start, end)
        }
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

    fn support_libraries(
        &mut self,
    ) -> Option<gdbstub::target::ext::libraries::LibrariesOps<'_, Self>> {
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

    use crate::gdb::checkpoint::CheckpointTable;
    use crate::gdb::resume::ReplayOp;

    use super::GdbTarget;
    use super::execute_op;
    use super::test_support::Recorder;

    /// A panicking backend must surface as a SIGTRAP stop, not a wedged event
    /// loop. `TargetRunner` clears the in-flight flag and reports the panic
    /// through the stop channel; `GdbTarget::wait_stop_reason` translates it.
    #[test]
    fn worker_panic_surfaces_as_sigtrap_stop() {
        let mut target = GdbTarget::new(
            Recorder {
                panics: true,
                ..Default::default()
            },
            CheckpointTable::new(),
        );
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

    /// `execute_op` is the single place that maps a resume op onto backend
    /// calls: per-thread ops set the current thread first (so the stop reply
    /// and later queries observe it), the range step forwards its bounds.
    #[test]
    fn execute_op_dispatches_each_resume_action() {
        use std::num::NonZeroUsize;

        let tid = NonZeroUsize::new(7).unwrap();

        let mut r = Recorder::default();
        execute_op(ReplayOp::Continue, &mut r).unwrap();
        assert_eq!(r.continues, 1);
        assert_eq!(r.current_thread, None, "plain continue keeps the thread");

        let mut r = Recorder::default();
        execute_op(ReplayOp::Step(tid), &mut r).unwrap();
        assert_eq!(r.steps, 1);
        assert_eq!(r.current_thread, Some(7), "step selects the thread first");

        let mut r = Recorder::default();
        execute_op(ReplayOp::BackwardStep(tid), &mut r).unwrap();
        assert_eq!(r.steps_back, 1);
        assert_eq!(r.current_thread, Some(7));

        let mut r = Recorder::default();
        execute_op(ReplayOp::RangeStep(tid, 0x1000, 0x2000), &mut r).unwrap();
        assert_eq!(
            r.range_steps,
            vec![(Some(7), 0x1000, 0x2000)],
            "range step carries its thread + bounds"
        );
        assert_eq!(r.current_thread, Some(7));
        assert_eq!(
            r.steps, 0,
            "the backend owns the range loop, not the dispatcher"
        );
    }
}
