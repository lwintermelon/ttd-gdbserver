//! `gdbstub::Target` implementation over the `DebugTarget` trait.
//!
//! This is where the GDB Remote Serial Protocol is mapped onto the target:
//! registers/memory/threads through the base ops, breakpoints/watchpoints as
//! TTD watchpoints, forward/reverse execution as TTD replay, and
//! Delve-specific packets (`qRRCmd` when/checkpoints, ...) through the
//! `UnknownPacket` extension.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};

use gdbstub::common::{Endianness, Pid, Signal, Tid};
use gdbstub::stub::MultiThreadStopReason;
use gdbstub::target::ext::auxv::Auxv;
use gdbstub::target::ext::base::multithread::{
    MultiThreadBase, MultiThreadResume, MultiThreadSchedulerLocking, MultiThreadSingleStep,
};
use gdbstub::target::ext::base::reverse_exec::{ReverseCont, ReverseStep};
use gdbstub::target::ext::base::single_register_access::SingleRegisterAccess;
use gdbstub::target::ext::base::BaseOps;
use gdbstub::target::ext::breakpoints::{Breakpoints, HwWatchpoint, SwBreakpoint, WatchKind};
use gdbstub::target::ext::exec_file::ExecFile;
use gdbstub::target::ext::extended_mode::{AttachKind, ExtendedMode, ShouldTerminate};
use gdbstub::target::ext::process_info::{ProcessInfo, ProcessInfoResponse};
use gdbstub::target::ext::target_description_xml_override::TargetDescriptionXmlOverride;
use gdbstub::target::ext::unknown_packet::{ConsoleOutput, UnknownPacket};
use gdbstub::target::{Target, TargetError, TargetResult};

use crate::target::{BreakpointKind, DebugError, DebugTarget, ModuleInfo, StopReason};
use crate::ttd::types::TtdPosition;

use gdbstub_arch::x86::reg::id::{X86SegmentRegId, X86_64CoreRegId, X87FpuInternalRegId};

use super::regs::{target_xml, TtdArch, TtdRegId, TtdRegisters};

/// A checkpoint saved by `qRRCmd checkpoint`.
struct Checkpoint {
    position: TtdPosition,
    when: String,
    where_: String,
}

/// What kind of replay a resume should perform.
#[derive(Debug)]
enum ReplayOp {
    Continue,
    Step(Tid),
    BackwardContinue,
    BackwardStep(Tid),
    /// Range step: replay forward until the cursor leaves `[start, end)`.
    /// TTD can fast-forward to `end`; if the cursor doesn't reach `end`
    /// we still report whatever stop reason we got (e.g. breakpoint).
    RangeStep(Tid, u64, u64),
}

/// `gdbstub::Target` backed by a `DebugTarget` backend.
///
/// The backend is shared with replay worker threads through a mutex: resume
/// operations run on a worker while the gdbstub event loop keeps polling the
/// connection (so a ^C can interrupt a long replay).
pub struct GdbTarget<T: DebugTarget + Send + 'static> {
    backend: Arc<Mutex<T>>,
    /// Interrupts an in-flight replay (from any thread).
    interrupt: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Path to the traced executable (qXfer:exec-file).
    exe_path: Option<String>,

    /// Pending resume actions (only touched while the stub is stopped).
    resume_step: Option<Tid>,
    resume_continue: bool,
    /// Pending range-step action: step until the cursor leaves [start, end).
    /// With TTD we implement this as a single `replay_forward_to(end)`.
    resume_range_step: Option<(Tid, u64, u64)>,

    /// Stop reason delivery from replay worker threads.
    stop_rx: Mutex<Option<Receiver<MultiThreadStopReason<u64>>>>,
    /// Set when the client sent ^C during a replay.
    interrupted: Arc<AtomicBool>,
    /// True while a replay worker is in flight.
    replay_running: Arc<AtomicBool>,

    /// Client-set breakpoints: addr -> target-level ids.
    sw_bps: HashMap<u64, Vec<u64>>,
    /// Client-set watchpoints: (addr, len, kind[0=W,1=R,2=RW]) -> ids.
    watch_bps: HashMap<(u64, u64, u8), Vec<u64>>,

    /// Emulated rr checkpoints (id -> snapshot).
    checkpoints: HashMap<u64, Checkpoint>,
    next_checkpoint_id: u64,
}

impl<T: DebugTarget + Send + 'static> GdbTarget<T> {
    pub fn new(backend: T) -> Self {
        let (_, stop_rx) = mpsc::channel();
        Self {
            backend: Arc::new(Mutex::new(backend)),
            interrupt: None,
            exe_path: None,
            resume_step: None,
            resume_continue: false,
            resume_range_step: None,
            stop_rx: Mutex::new(Some(stop_rx)),
            interrupted: Arc::new(AtomicBool::new(false)),
            replay_running: Arc::new(AtomicBool::new(false)),
            sw_bps: HashMap::new(),
            watch_bps: HashMap::new(),
            checkpoints: HashMap::new(),
            next_checkpoint_id: 1,
        }
    }

    pub fn with_interrupt(mut self, f: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.interrupt = Some(f);
        self
    }

    pub fn with_exe_path(mut self, path: String) -> Self {
        self.exe_path = Some(path);
        self
    }

    /// Interrupt the in-flight replay (from the gdbstub event loop).
    pub fn interrupt_replay(&self) {
        if self.replay_running.load(Ordering::SeqCst) {
            self.interrupted.store(true, Ordering::SeqCst);
            if let Some(f) = &self.interrupt {
                f();
            }
        }
    }

    /// Whether a replay worker is currently running.
    pub fn replay_in_flight(&self) -> bool {
        self.replay_running.load(Ordering::SeqCst)
    }

    /// Recover the backend (for state persistence across sessions).
    pub fn into_backend(self) -> Option<T> {
        Arc::try_unwrap(self.backend)
            .ok()
            .map(|m| m.into_inner().unwrap())
    }

    /// Take the stop reason a worker reported, if any.
    pub fn take_stop_reason(&self) -> Option<MultiThreadStopReason<u64>> {
        let guard = self.stop_rx.lock().unwrap();
        guard.as_ref()?.try_recv().ok()
    }

    /// Wait (blocking) for the worker to report a stop reason.
    pub fn wait_stop_reason(&self) -> Option<MultiThreadStopReason<u64>> {
        let guard = self.stop_rx.lock().unwrap();
        guard.as_ref()?.recv().ok()
    }

    fn backend(&self) -> std::sync::MutexGuard<'_, T> {
        self.backend.lock().unwrap()
    }

    /// Start a replay on a worker thread. Returns immediately.
    fn spawn_replay(&mut self, op: ReplayOp) {
        log::debug!("spawn_replay op={:?}", op);
        let (tx, rx) = mpsc::channel();
        *self.stop_rx.lock().unwrap() = Some(rx);

        let backend = self.backend.clone();
        let interrupted = self.interrupted.clone();
        let replay_running = self.replay_running.clone();
        replay_running.store(true, Ordering::SeqCst);

        std::thread::spawn(move || {
            let reason = {
                let mut b = backend.lock().unwrap();
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
                        // Range step: step while PC is in [start, end). TTD
                        // doesn't have a way to fast-forward to "next time
                        // PC exits the range" (we don't know the future),
                        // so we single-step and check the PC after each
                        // step. TTD step is fast enough for this to be
                        // practical.
                        b.set_current_thread(tid.get() as u64);
                        let mut last = b.step();
                        loop {
                            let in_range = match b.thread_state(Some(tid.get() as u64)) {
                                Some((regs, _)) => {
                                    let pc = regs.rip;
                                    pc >= start && pc < end
                                }
                                None => false,
                            };
                            if !in_range {
                                break last;
                            }
                            last = b.step();
                        }
                    }
                };
                // Map the backend stop reason to a gdbstub stop reason. The
                // interrupted flag makes a ^C abort report SIGINT instead of
                // the generic SIGTRAP.
                let stop = map_stop_reason(&mut *b, backend_reason);
                log::debug!(
                    "replay worker done: stop={:?} position={:?}",
                    stop,
                    b.position()
                );
                if interrupted.swap(false, Ordering::SeqCst) {
                    let tid = stop_tid(&mut *b);
                    MultiThreadStopReason::SignalWithThread {
                        tid,
                        signal: Signal::SIGINT,
                    }
                } else {
                    stop
                }
            };
            replay_running.store(false, Ordering::SeqCst);
            let _ = tx.send(reason);
        });
    }

    fn exe_module_base(&self) -> Option<u64> {
        let modules = self.backend().modules();
        if modules.is_empty() {
            return None;
        }
        if let Some(exe) = &self.exe_path {
            let want = std::path::Path::new(exe)
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.to_ascii_lowercase());
            if let Some(want) = want {
                for m in &modules {
                    let name = std::path::Path::new(&m.name)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.to_ascii_lowercase());
                    if name.as_deref() == Some(want.as_str()) {
                        return Some(m.base_addr);
                    }
                }
            }
        }
        Some(modules[0].base_addr)
    }

    fn format_when(&self) -> String {
        let pos = self.backend().position();
        format!("{:X}:{:X}", pos.sequence, pos.steps)
    }

    fn handle_qrrcmd(&mut self, args: &str) -> String {
        let args = parse_qrrcmd_args(args);
        if args.is_empty() {
            return String::new();
        }
        match args[0].as_str() {
            "when" => self.format_when(),
            "checkpoint" => {
                let where_ = args.get(1).cloned().unwrap_or_default();
                let id = self.next_checkpoint_id;
                self.next_checkpoint_id += 1;
                let when = self.format_when();
                let pos = self.backend().position();
                self.checkpoints.insert(
                    id,
                    Checkpoint {
                        position: pos,
                        when: when.clone(),
                        where_: where_.clone(),
                    },
                );
                format!("Checkpoint {} at {} {}", id, when, where_)
            }
            "info checkpoints" => {
                let mut out = String::from("Num\tWhen\tWhere\n");
                let mut ids: Vec<u64> = self.checkpoints.keys().copied().collect();
                ids.sort_unstable();
                for id in ids {
                    if let Some(cp) = self.checkpoints.get(&id) {
                        let where_ = if cp.where_.is_empty() {
                            "-"
                        } else {
                            &cp.where_
                        };
                        out.push_str(&format!("{}\t{}\t{}\n", id, cp.when, where_));
                    }
                }
                out
            }
            "delete checkpoint" => match args.get(1).and_then(|s| s.parse::<u64>().ok()) {
                Some(id) if self.checkpoints.remove(&id).is_some() => {
                    format!("Deleted checkpoint {}", id)
                }
                _ => "No such checkpoint".to_string(),
            },
            _ => String::new(),
        }
    }

    fn handle_vrun(&mut self, args: &mut dyn Iterator<Item = &[u8]>) {
        let pos_str = match args.next() {
            Some(s) => String::from_utf8_lossy(s).trim().to_string(),
            None => String::new(),
        };
        log::debug!("vRun pos_str={:?}", pos_str);
        let mut backend = self.backend();
        if pos_str.is_empty() {
            let (first, _) = backend.lifetime();
            let _ = backend.goto(first);
        } else if let Some(id_str) = pos_str
            .strip_prefix('c')
            .or_else(|| pos_str.strip_prefix('C'))
        {
            if let Ok(id) = id_str.parse::<u64>() {
                if let Some(cp) = self.checkpoints.get(&id) {
                    let _ = backend.goto(cp.position);
                }
            }
        } else if let Some((seq, steps)) = pos_str.split_once(':') {
            if let (Ok(sequence), Ok(steps)) =
                (u64::from_str_radix(seq, 16), u64::from_str_radix(steps, 16))
            {
                let _ = backend.goto(TtdPosition { sequence, steps });
            }
        }
        log::debug!("vRun -> position {:?}", backend.position());
    }
}

// ─── Stop reason mapping ─────────────────────────────────────────

// Note: GDB's T-packet can carry "expedited registers" inline (using
// gdbstub's `report_stop_with_regs` API) so the client doesn't have to
// follow up with a `g` round-trip after every stop. We don't use that
// here: the worker reports the stop reason over an mpsc channel and the
// BlockingEventLoop yields it to gdbstub, which means we never get a
// chance to attach register data to the report. GDB's standard behaviour
// is to send a `g` immediately after a T05, so the extra round-trip
// cost is one packet pair per stop — acceptable for replay, where stop
// frequency is bounded by user interaction. If this becomes a hotspot
// (e.g. for scripted batch stepping) we can switch to a custom event
// loop that pushes the reason + register bytes into a single struct and
// use gdbstub's `report_stop_with_regs` API directly.

fn stop_tid<T: DebugTarget>(backend: &mut T) -> Tid {
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

fn map_stop_reason<T: DebugTarget>(
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
        StopReason::Watchpoint { addr, .. } => MultiThreadStopReason::Watch {
            tid,
            kind: WatchKind::Write,
            addr,
        },
        StopReason::Exception { .. } => MultiThreadStopReason::SignalWithThread {
            tid,
            signal: Signal::SIGSEGV,
        },
        _ => MultiThreadStopReason::SignalWithThread {
            tid,
            signal: Signal::SIGTRAP,
        },
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
        let thread_id = tid.get() as u64;
        let (regs_val, teb) = backend
            .thread_state(Some(thread_id))
            .ok_or(TargetError::NonFatal)?;
        let regs = TtdRegisters::from_ttd(&regs_val, teb);
        // st0..st7 (10 bytes) and xmm0..xmm15 (16 bytes) are byte arrays;
        // everything else fits in a u64.
        let (value, size): (u64, usize) = match reg_id {
            TtdRegId::Core(X86_64CoreRegId::Gpr(i)) => (regs.core.regs[i as usize], 8),
            TtdRegId::Core(X86_64CoreRegId::Rip) => (regs.core.rip, 8),
            TtdRegId::Core(X86_64CoreRegId::Eflags) => (regs.core.eflags as u64, 4),
            TtdRegId::Core(X86_64CoreRegId::Segment(s)) => {
                let v = match s {
                    X86SegmentRegId::CS => regs.core.segments.cs,
                    X86SegmentRegId::SS => regs.core.segments.ss,
                    X86SegmentRegId::DS => regs.core.segments.ds,
                    X86SegmentRegId::ES => regs.core.segments.es,
                    X86SegmentRegId::FS => regs.core.segments.fs,
                    X86SegmentRegId::GS => regs.core.segments.gs,
                };
                (v as u64, 4)
            }
            TtdRegId::Core(X86_64CoreRegId::St(i)) => {
                let n = buf.len().min(10);
                buf[..n].copy_from_slice(&regs.core.st[i as usize][..n]);
                return Ok(n);
            }
            TtdRegId::Core(X86_64CoreRegId::Fpu(f)) => {
                let v = match f {
                    X87FpuInternalRegId::Fctrl => regs.core.fpu.fctrl,
                    X87FpuInternalRegId::Fstat => regs.core.fpu.fstat,
                    X87FpuInternalRegId::Ftag => regs.core.fpu.ftag,
                    X87FpuInternalRegId::Fiseg => regs.core.fpu.fiseg,
                    X87FpuInternalRegId::Fioff => regs.core.fpu.fioff,
                    X87FpuInternalRegId::Foseg => regs.core.fpu.foseg,
                    X87FpuInternalRegId::Fooff => regs.core.fpu.fooff,
                    X87FpuInternalRegId::Fop => regs.core.fpu.fop,
                };
                (v as u64, 4)
            }
            TtdRegId::Core(X86_64CoreRegId::Xmm(i)) => {
                let n = buf.len().min(16);
                buf[..n].copy_from_slice(&regs.core.xmm[i as usize].to_le_bytes()[..n]);
                return Ok(n);
            }
            TtdRegId::Core(X86_64CoreRegId::Mxcsr) => (regs.core.mxcsr as u64, 4),
            TtdRegId::FsBase => (regs.fs_base, 8),
            TtdRegId::GsBase => (regs.gs_base, 8),
            // X86_64CoreRegId is #[non_exhaustive]; unknown variants are
            // unreachable in practice (from_raw_id only yields known ids).
            TtdRegId::Core(_) => return Err(TargetError::NonFatal),
        };
        let size = buf.len().min(size);
        buf[..size].copy_from_slice(&value.to_le_bytes()[..size]);
        Ok(size)
    }

    fn write_register(
        &mut self,
        _tid: Tid,
        _reg_id: <Self::Arch as gdbstub::arch::Arch>::RegId,
        _val: &[u8],
    ) -> TargetResult<(), Self> {
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
        let op = if let Some((tid, start, end)) = self.resume_range_step.take() {
            ReplayOp::RangeStep(tid, start, end)
        } else if let Some(tid) = self.resume_step.take() {
            ReplayOp::Step(tid)
        } else {
            ReplayOp::Continue
        };
        self.resume_continue = false;
        self.spawn_replay(op);
        Ok(())
    }

    fn clear_resume_actions(&mut self) -> Result<(), Self::Error> {
        self.resume_step = None;
        self.resume_continue = false;
        Ok(())
    }

    fn set_resume_action_continue(
        &mut self,
        _tid: Tid,
        _signal: Option<Signal>,
    ) -> Result<(), Self::Error> {
        self.resume_continue = true;
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
        self.resume_step = Some(tid);
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
        if start == end {
            self.resume_step = Some(tid);
        } else {
            self.resume_range_step = Some((tid, start, end));
        }
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
        self.spawn_replay(ReplayOp::BackwardContinue);
        Ok(())
    }
}

impl<T: DebugTarget + Send + 'static> ReverseStep<Tid> for GdbTarget<T> {
    fn reverse_step(&mut self, tid: Tid) -> Result<(), Self::Error> {
        self.spawn_replay(ReplayOp::BackwardStep(tid));
        Ok(())
    }
}

// ─── Breakpoints (Z0 sw + Z1 hw + Z2/Z3/Z4 watchpoints / z* remove) ─

impl<T: DebugTarget + Send + 'static> Breakpoints for GdbTarget<T> {
    fn support_sw_breakpoint(
        &mut self,
    ) -> Option<gdbstub::target::ext::breakpoints::SwBreakpointOps<'_, Self>> {
        Some(self)
    }

    fn support_hw_breakpoint(
        &mut self,
    ) -> Option<gdbstub::target::ext::breakpoints::HwBreakpointOps<'_, Self>> {
        Some(self)
    }

    fn support_hw_watchpoint(
        &mut self,
    ) -> Option<gdbstub::target::ext::breakpoints::HwWatchpointOps<'_, Self>> {
        Some(self)
    }
}

impl<T: DebugTarget + Send + 'static> SwBreakpoint for GdbTarget<T> {
    fn add_sw_breakpoint(&mut self, addr: u64, _kind: usize) -> TargetResult<bool, Self> {
        let id = self.backend().set_breakpoint(addr, None);
        log::debug!("Z0 sw bp add addr={:#x} id={}", addr, id);
        self.sw_bps.entry(addr).or_default().push(id);
        Ok(true)
    }

    fn remove_sw_breakpoint(&mut self, addr: u64, _kind: usize) -> TargetResult<bool, Self> {
        log::debug!("z0 sw bp remove addr={:#x}", addr);
        // Remove every breakpoint at this address: restarts (vRun) make the
        // client re-insert breakpoints we still hold, so the client's single
        // z0 must clear all accumulated ids, or leftovers keep firing after
        // the client considers the breakpoint deleted.
        if let Some(ids) = self.sw_bps.remove(&addr) {
            for id in ids {
                self.backend().remove_breakpoint(id);
            }
        }
        Ok(true)
    }
}

impl<T: DebugTarget + Send + 'static> gdbstub::target::ext::breakpoints::HwBreakpoint
    for GdbTarget<T>
{
    fn add_hw_breakpoint(&mut self, addr: u64, _kind: usize) -> TargetResult<bool, Self> {
        let id = self.backend().set_breakpoint(addr, None);
        log::debug!("Z1 hw bp add addr={:#x} id={}", addr, id);
        self.sw_bps.entry(addr).or_default().push(id);
        Ok(true)
    }

    fn remove_hw_breakpoint(&mut self, addr: u64, _kind: usize) -> TargetResult<bool, Self> {
        log::debug!("z1 hw bp remove addr={:#x}", addr);
        // See remove_sw_breakpoint: clear all ids at this address.
        if let Some(ids) = self.sw_bps.remove(&addr) {
            for id in ids {
                self.backend().remove_breakpoint(id);
            }
        }
        Ok(true)
    }
}

impl<T: DebugTarget + Send + 'static> HwWatchpoint for GdbTarget<T> {
    fn add_hw_watchpoint(
        &mut self,
        addr: u64,
        len: u64,
        kind: WatchKind,
    ) -> TargetResult<bool, Self> {
        let bp_kind = match kind {
            WatchKind::Write => BreakpointKind::Write,
            WatchKind::Read => BreakpointKind::Read,
            WatchKind::ReadWrite => BreakpointKind::Access,
        };
        let kind_code = watch_kind_code(kind);
        let id = self.backend().set_data_breakpoint(addr, len, bp_kind);
        self.watch_bps
            .entry((addr, len, kind_code))
            .or_default()
            .push(id);
        Ok(true)
    }

    fn remove_hw_watchpoint(
        &mut self,
        addr: u64,
        len: u64,
        kind: WatchKind,
    ) -> TargetResult<bool, Self> {
        let kind_code = watch_kind_code(kind);
        // See remove_sw_breakpoint: clear all ids for this watchpoint key.
        if let Some(ids) = self.watch_bps.remove(&(addr, len, kind_code)) {
            for id in ids {
                self.backend().remove_breakpoint(id);
            }
        }
        Ok(true)
    }
}

fn watch_kind_code(kind: WatchKind) -> u8 {
    match kind {
        WatchKind::Write => 0,
        WatchKind::Read => 1,
        WatchKind::ReadWrite => 2,
    }
}

// ─── Extended mode (vRun restart) ────────────────────────────────

impl<T: DebugTarget + Send + 'static> ExtendedMode for GdbTarget<T> {
    fn run(
        &mut self,
        _filename: Option<&[u8]>,
        args: gdbstub::target::ext::extended_mode::Args<'_, '_>,
    ) -> TargetResult<Pid, Self> {
        self.handle_vrun(&mut args.into_iter());
        Ok(Pid::new(1).unwrap())
    }

    fn attach(&mut self, _pid: Pid) -> TargetResult<(), Self> {
        Err(TargetError::NonFatal)
    }

    fn query_if_attached(&mut self, _pid: Pid) -> TargetResult<AttachKind, Self> {
        Ok(AttachKind::Attach)
    }

    fn kill(&mut self, _pid: Option<Pid>) -> TargetResult<ShouldTerminate, Self> {
        Ok(ShouldTerminate::Yes)
    }

    fn restart(&mut self) -> Result<(), Self::Error> {
        let (first, _) = self.backend().lifetime();
        let _ = self.backend().goto(first);
        Ok(())
    }
}

// ─── Exec-file / auxv / process-info / libraries / memory-map ─────

use gdbstub::outputln;
use gdbstub::target::ext::libraries::Libraries;
use gdbstub::target::ext::memory_map::MemoryMap;
use gdbstub::target::ext::monitor_cmd::MonitorCmd;
use gdbstub::target::ext::section_offsets::{Offsets, SectionOffsets};
use gdbstub::target::ext::thread_extra_info::ThreadExtraInfo;
use quick_xml::Writer;

/// Build a GDB library-list XML for the current backend's modules.
///
/// Schema (from the gdbstub `Libraries` trait doc):
///   `<library-list version="1.0"><library name="…"><segment address="0x…"/></library>…</library-list>`
///
/// The `<segment address>` is the address the *first section* was loaded at
/// per the GDB manual. For TTD the only address we have is the module's
/// image base (`ModuleInstance.Address`); we report that. gdb's PE loader
/// reads the PE headers from this address, so symbols are found correctly
/// even though the address is technically the image base rather than the
/// first section. For full PE-fidelity we would need a per-section table
/// from TTD (not currently exposed), which can be added in a follow-up.
fn build_libraries_xml(modules: &[ModuleInfo]) -> String {
    let mut buf = Vec::with_capacity(64 + modules.len() * 96);
    let mut w = Writer::new(&mut buf);
    // The image base is what TTD gives us; the name is a fully-qualified
    // path on Windows. quick-xml handles the escaping.
    w.create_element("library-list")
        .with_attribute(("version", "1.0"))
        .write_inner_content(|w| {
            for m in modules {
                let addr = format!("0x{:x}", m.base_addr);
                w.create_element("library")
                    .with_attribute(("name", m.name.as_str()))
                    .write_inner_content(|w| {
                        w.create_element("segment")
                            .with_attribute(("address", addr.as_str()))
                            .write_empty()?;
                        Ok(())
                    })?;
            }
            Ok(())
        })
        .expect("writing to a Vec cannot fail");
    String::from_utf8(buf).expect("XML is valid UTF-8")
}

/// Build a GDB memory-map XML for the current backend's modules.
///
/// Schema:
///   `<memory-map><memory type="ram" start="0x…" length="0x…"/>…</memory-map>`
///
/// We emit one `ram` region per loaded module covering `[base, base+size)`.
/// Querying the engine for every observed address range would be too
/// expensive (and meaningless for replay: the cursor's "memory at this
/// position" is the memory the trace recorded touching, which is the
/// module's image anyway). gdb's PE loader is happy with module-level
/// regions; if a client needs finer granularity we serve it through the
/// `monitor ttd memory-ranges` custom command.
fn build_memory_map_xml(modules: &[ModuleInfo]) -> String {
    let mut buf = Vec::with_capacity(64 + modules.len() * 80);
    let mut w = Writer::new(&mut buf);
    w.create_element("memory-map")
        .write_inner_content(|w| {
            for m in modules {
                let start = format!("0x{:x}", m.base_addr);
                let len = format!("0x{:x}", m.size);
                w.create_element("memory")
                    .with_attribute(("type", "ram"))
                    .with_attribute(("start", start.as_str()))
                    .with_attribute(("length", len.as_str()))
                    .write_empty()?;
            }
            Ok(())
        })
        .expect("writing to a Vec cannot fail");
    String::from_utf8(buf).expect("XML is valid UTF-8")
}

/// Copy the substring `[offset, offset+length)` of `xml` into `buf`.
/// Returns `Ok(0)` when `offset` is past the end of `xml` (gdb expects
/// the `l` terminator in that case).
fn write_xml_chunk<E>(
    xml: &str,
    offset: u64,
    length: usize,
    buf: &mut [u8],
) -> Result<usize, gdbstub::target::TargetError<E>> {
    let start = (offset as usize).min(xml.len());
    let end = start.saturating_add(length).min(xml.len());
    if start == xml.len() {
        return Ok(0);
    }
    let n = (end - start).min(buf.len());
    buf[..n].copy_from_slice(&xml.as_bytes()[start..start + n]);
    Ok(n)
}

impl<T: DebugTarget + Send + 'static> ExecFile for GdbTarget<T> {
    fn get_exec_file(
        &self,
        _pid: Option<Pid>,
        offset: u64,
        length: usize,
        buf: &mut [u8],
    ) -> TargetResult<usize, Self> {
        let Some(path) = &self.exe_path else {
            return Err(TargetError::NonFatal);
        };
        let data = path.as_bytes();
        let start = (offset as usize).min(data.len());
        let end = start.saturating_add(length).min(data.len());
        let n = end - start;
        buf[..n].copy_from_slice(&data[start..end]);
        Ok(n)
    }
}

impl<T: DebugTarget + Send + 'static> Auxv for GdbTarget<T> {
    fn get_auxv(&self, offset: u64, length: usize, buf: &mut [u8]) -> TargetResult<usize, Self> {
        // Synthetic ELF auxv: AT_ENTRY = runtime base of the main module.
        // Delve needs the executable's runtime base to relocate DWARF; this
        // flows through its existing readAuxv -> EntryPointFromAuxv path.
        const AT_NULL: u64 = 0;
        const AT_ENTRY: u64 = 9;
        let Some(base) = self.exe_module_base() else {
            return Err(TargetError::NonFatal);
        };
        let mut auxv = Vec::with_capacity(32);
        auxv.extend_from_slice(&AT_ENTRY.to_le_bytes());
        auxv.extend_from_slice(&base.to_le_bytes());
        auxv.extend_from_slice(&AT_NULL.to_le_bytes());
        auxv.extend_from_slice(&0u64.to_le_bytes());

        let start = (offset as usize).min(auxv.len());
        let end = start.saturating_add(length).min(auxv.len());
        let n = end - start;
        buf[..n].copy_from_slice(&auxv[start..end]);
        Ok(n)
    }
}

impl<T: DebugTarget + Send + 'static> ProcessInfo for GdbTarget<T> {
    fn process_info(
        &self,
        write_item: &mut dyn FnMut(&ProcessInfoResponse<'_>),
    ) -> Result<(), Self::Error> {
        write_item(&ProcessInfoResponse::Pid(Pid::new(1).unwrap()));
        write_item(&ProcessInfoResponse::Triple("x86_64-pc-windows-msvc"));
        write_item(&ProcessInfoResponse::Endianness(Endianness::Little));
        write_item(&ProcessInfoResponse::PointerSize(8));
        Ok(())
    }
}

// ─── Library list (qXfer:libraries:read) + Memory map (qXfer:memory-map:read) ─

impl<T: DebugTarget + Send + 'static> Libraries for GdbTarget<T> {
    fn get_libraries(
        &self,
        offset: u64,
        length: usize,
        buf: &mut [u8],
    ) -> TargetResult<usize, Self> {
        let xml = build_libraries_xml(&self.backend().modules());
        write_xml_chunk(&xml, offset, length, buf)
    }
}

impl<T: DebugTarget + Send + 'static> MemoryMap for GdbTarget<T> {
    fn memory_map_xml(
        &self,
        offset: u64,
        length: usize,
        buf: &mut [u8],
    ) -> TargetResult<usize, Self> {
        let xml = build_memory_map_xml(&self.backend().modules());
        write_xml_chunk(&xml, offset, length, buf)
    }
}

// ─── Section offsets (qOffsets) ────────────────────────────────────

impl<T: DebugTarget + Send + 'static> SectionOffsets for GdbTarget<T> {
    fn get_section_offsets(
        &mut self,
    ) -> Result<Offsets<<Self::Arch as gdbstub::arch::Arch>::Usize>, Self::Error> {
        // For PE executables the convention is to report the image base as
        // the text segment, with the data segment at the same address (or
        // omitted). gdb uses this to relocate symbols; combined with the
        // correct AT_ENTRY from qXfer:auxv:read (see `Auxv::get_auxv`), the
        // symbol load is exact.
        let modules = self.backend().modules();
        let main = modules.first().map(|m| m.base_addr).unwrap_or(0);
        Ok(Offsets::Segments {
            text_seg: main,
            data_seg: Some(main),
        })
    }
}

impl<T: DebugTarget + Send + 'static> ThreadExtraInfo for GdbTarget<T> {
    fn thread_extra_info(&self, tid: Tid, buf: &mut [u8]) -> Result<usize, Self::Error> {
        // Format: "UTID <unique> (OS 0x<os>); pos <seq>:<steps>".
        // The OS thread id is implicit (gdb already shows it as `Thread N.M`),
        // but we include it for self-containment. The trace position lets
        // the user locate the thread on the time axis at a glance.
        let info = self.backend().thread_info(tid.get() as u64);
        let s = match info {
            Some(i) => format!(
                "UTID {} (OS 0x{:x}); pos {:x}:{:x}",
                i.unique_id,
                tid.get(),
                i.current_position.sequence,
                i.current_position.steps
            ),
            None => format!("UTID ? (OS 0x{:x})", tid.get()),
        };
        let n = s.len().min(buf.len());
        buf[..n].copy_from_slice(&s.as_bytes()[..n]);
        Ok(n)
    }
}

// ─── Unknown packets (Delve-specific) ────────────────────────────

impl<T: DebugTarget + Send + 'static> UnknownPacket for GdbTarget<T> {
    fn handle_unknown_packet(
        &mut self,
        body: &[u8],
        mut out: ConsoleOutput<'_>,
    ) -> Result<(), Self::Error> {
        let s = std::str::from_utf8(body).unwrap_or("");
        log::debug!("gdb custom <- {}", s);

        if let Some(args) = s.strip_prefix("qRRCmd") {
            // qRRCmd responses are hex-encoded text (Delve decodes pairs).
            let resp = self.handle_qrrcmd(args);
            let hex: String = resp.bytes().map(|b| format!("{:02x}", b)).collect();
            out.write_raw(hex.as_bytes());
            return Ok(());
        }

        if s == "QThreadSuffixSupported" {
            // Unsupported -> Delve falls back to plain `H` thread selection,
            // which gdbstub handles natively. (write nothing = unsupported)
            return Ok(());
        }

        if s == "QListThreadsInStopReply" {
            // Unsupported -> Delve uses qfThreadInfo. (write nothing)
            return Ok(());
        }

        if s == "QListThreadsInStopReply" {
            // Unsupported -> Delve uses qfThreadInfo.
            return Ok(());
        }

        // `QPassSignals` / `QProgramSignals`: client enumerates which
        // signals it wants delivered. We acknowledge; replay is
        // read-only and the recorded signal stream is the only one we
        // care about.
        if s.starts_with("QPassSignals") || s.starts_with("QProgramSignals") {
            out.write_raw(b"OK");
            return Ok(());
        }

        // `vCtrlC` is extended-remote's preferred form of ^C; ACK so
        // the client knows we saw it (the BlockingEventLoop's
        // on_interrupt path will abort the replay separately).
        if s == "vCtrlC" {
            out.write_raw(b"OK");
            return Ok(());
        }

        if let Some(rest) = s.strip_prefix("qGetTLSAddr:") {
            // Format: `qGetTLSAddr:p<pid>.<tid>,<offset>,<lm>`. On Windows
            // the TLS address is the TEB; the offset is a TLS-index used
            // by Linux glibc — we ignore it because our TEB base is what
            // the kernel/runtime would use to resolve it on Windows.
            // We also ignore `lm` (link map) which is irrelevant here.
            let tid_part = rest.split(',').next().unwrap_or("");
            let os_tid = parse_tid_from_qpacket(tid_part);
            if let Some(os_tid) = os_tid {
                let teb = self
                    .backend()
                    .thread_info(os_tid)
                    .map(|i| i.teb)
                    .unwrap_or(0);
                if teb == 0 {
                    out.write_raw(b"E01");
                } else {
                    out.write_raw(format!("{:x}", teb).as_bytes());
                }
            } else {
                out.write_raw(b"E01");
            }
            return Ok(());
        }

        // _M (allocate memory), jGetLoadedDynamicLibrariesInfos,
        // qMemoryRegionInfo, qXfer:siginfo, ... — unsupported.
        Ok(())
    }
}

// ─── Monitor (RR-style custom commands: monitor ttd …) ──────────
//
// gdb sends `monitor ttd <subcmd> [<args>]` for the user-facing
// introspection commands. Subcommands:
//   - `info`     : trace path, lifetime, module/thread/exception counts,
//                  current position
//   - `threads`  : list every thread that ever existed
//   - `module <addr>` : resolve an address to a module + range
//   - `events [n]`   : show the first n exception events (default 20)
//
// Wire format is plain text (one line per record), as RR does. The
// response is `out.write_raw` so it lands in the gdb console.

impl<T: DebugTarget + Send + 'static> MonitorCmd for GdbTarget<T> {
    fn handle_monitor_cmd(
        &mut self,
        cmd: &[u8],
        mut out: gdbstub::target::ext::monitor_cmd::ConsoleOutput<'_>,
    ) -> Result<(), Self::Error> {
        let s = std::str::from_utf8(cmd).unwrap_or("");
        let mut words = s.split_whitespace();
        let head = words.next().unwrap_or("");
        if head != "ttd" {
            // Unknown subcommand — print a brief help so users can discover
            // what we accept.
            outputln!(out, "ttd-gdbserver: unknown monitor subcommand '{head}'");
            outputln!(
                out,
                "supported: ttd info | ttd threads | ttd module <addr> | ttd events [n]"
            );
            return Ok(());
        }
        let sub = words.next().unwrap_or("");
        // Re-join the remainder with single spaces so the subcommands
        // can pull a single argument (`module 0x401000`, `events 5`).
        let rest_args: Vec<&str> = words.collect();
        match sub {
            "info" => {
                let d = self.backend().diagnostics();
                outputln!(out, "trace: loaded (TTD replay engine)");
                outputln!(
                    out,
                    "lifetime: {:x}:{:x} .. {:x}:{:x}",
                    d.first_position.sequence,
                    d.first_position.steps,
                    d.last_position.sequence,
                    d.last_position.steps
                );
                outputln!(
                    out,
                    "current: {:x}:{:x}",
                    d.current_position.sequence,
                    d.current_position.steps
                );
                outputln!(out, "modules: {}", d.module_count);
                outputln!(out, "threads (lifetime): {}", d.threads.len());
                outputln!(out, "exception events: {}", d.exception_count);
            }
            "threads" => {
                let d = self.backend().diagnostics();
                outputln!(
                    out,
                    "{:>6}  {:>10}  {:>14}  {:>14}",
                    "UTID",
                    "OS_TID",
                    "active_min",
                    "active_max"
                );
                for t in &d.threads {
                    outputln!(
                        out,
                        "{:>6}  0x{:08x}  {:>9x}:{:>04x}  {:>9x}:{:>04x}",
                        t.unique_id,
                        t.os_thread_id,
                        t.active_time.0.sequence,
                        t.active_time.0.steps,
                        t.active_time.1.sequence,
                        t.active_time.1.steps,
                    );
                }
            }
            "module" => {
                if rest_args.is_empty() {
                    outputln!(out, "usage: monitor ttd module <hex-addr>");
                    return Ok(());
                }
                let rest = rest_args[0];
                let addr = match u64::from_str_radix(rest.trim_start_matches("0x"), 16) {
                    Ok(v) => v,
                    Err(_) => {
                        outputln!(out, "ttd: bad address '{rest}'");
                        return Ok(());
                    }
                };
                let modules = self.backend().modules();
                let hit = modules
                    .iter()
                    .find(|m| addr >= m.base_addr && addr < m.base_addr + m.size);
                match hit {
                    Some(m) => outputln!(
                        out,
                        "{:#x}..{:#x} (+{:#x}) {}",
                        m.base_addr,
                        m.base_addr + m.size,
                        m.size,
                        m.name
                    ),
                    None => outputln!(out, "no module contains {addr:#x}"),
                }
            }
            "events" => {
                let _n: usize = rest_args.first().and_then(|s| s.parse().ok()).unwrap_or(20);
                // Per-event listing goes through `qTTDEvents:offset,length`
                // (handled in `UnknownPacket`) for paginated access; this
                // monitor command is a thin summary for now.
                let count = self.backend().diagnostics().exception_count;
                outputln!(out, "exception events: {count}");
                outputln!(
                    out,
                    "(use qXfer 'qTTDEvents' for the full list, when wired up)"
                );
            }
            other => {
                outputln!(out, "ttd: unknown subcommand '{other}'");
                outputln!(
                    out,
                    "supported: ttd info | ttd threads | ttd module <addr> | ttd events [n]"
                );
            }
        }
        Ok(())
    }
}

// ─── Helpers ─────────────────────────────────────────────────────

/// Parse qRRCmd arguments, handling both styles:
/// new: `:<cmd>:-1[:<hexarg>...]`, old: `:<hexcmd>[:<hexarg>...]`.
fn parse_qrrcmd_args(rest: &str) -> Vec<String> {
    let rest = rest.trim_start_matches(':');
    if rest.is_empty() {
        return Vec::new();
    }
    let parts: Vec<&str> = rest.split(':').collect();
    // New style: literal command followed by "-1".
    if parts.len() >= 2 && parts[1] == "-1" {
        let mut args = vec![parts[0].to_string()];
        for p in &parts[2..] {
            let decoded = decode_hex(p.as_bytes())
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_else(|| (*p).to_string());
            args.push(decoded);
        }
        return args;
    }
    // Old style: all tokens hex-encoded.
    parts
        .iter()
        .filter_map(|p| decode_hex(p.as_bytes()).map(|b| String::from_utf8_lossy(&b).into_owned()))
        .collect()
}

/// Parse a qGetTLSAddr thread-id fragment like `p1.100` or `100` into
/// the OS thread id (100). We do not currently honor multi-process pids
/// beyond the stub's single process.
fn parse_tid_from_qpacket(s: &str) -> Option<u64> {
    // Strip the optional `p<pid>.` prefix; what remains is the hex tid.
    if let Some(rest) = s.strip_prefix('p') {
        let (_pid, tail) = rest.split_once('.')?;
        return u64::from_str_radix(tail, 16).ok();
    }
    u64::from_str_radix(s, 16).ok()
}

fn decode_hex(s: &[u8]) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in s.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
    }
    Some(out)
}

impl<T: DebugTarget + Send + 'static> TargetDescriptionXmlOverride for GdbTarget<T> {
    fn target_description_xml(
        &self,
        annex: &[u8],
        offset: u64,
        length: usize,
        buf: &mut [u8],
    ) -> TargetResult<usize, Self> {
        let annex = std::str::from_utf8(annex).map_err(|_| TargetError::NonFatal)?;
        let xml = target_xml(annex).map_err(|_| TargetError::NonFatal)?;
        let start = (offset as usize).min(xml.len());
        let end = start.saturating_add(length).min(xml.len());
        let n = (end - start).min(buf.len());
        buf[..n].copy_from_slice(&xml[start..start + n]);
        Ok(n)
    }
}
