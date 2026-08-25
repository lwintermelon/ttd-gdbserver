//! Configurable in-memory `DebugTarget` for the GDB frontend tests.
//!
//! A single configurable mock replaces the per-test wrapper structs (and the
//! twenty-method `DebugTarget` delegation boilerplate) that the suite used to
//! carry: tests override only the behavior they care about via
//! [`MockTarget::with_forward`] and friends.
use std::collections::BTreeMap;
use std::sync::mpsc;

use ttd_gdbserver::target::{
    BreakpointKind, DebugError, DebugTarget, ModuleInfo, StopReason, TargetDiagnostics,
    ThreadDiagnostics, ThreadExtraInfoData,
};
use ttd_gdbserver::ttd::TtdError;
use ttd_gdbserver::ttd::types::{TtdPosition, TtdX64Regs};

/// What a forward continue should report.
pub enum ForwardStop {
    /// Stop at the first registered breakpoint; run to trace end if none.
    /// This is the default Delve flow exercised by most frontend tests.
    BreakpointOrTraceEnd,
    /// Report this reason immediately without touching debug state.
    Fixed(StopReason),
    /// Block until the sender is dropped/signals, then report `reason`.
    /// Used by the concurrency tests to park a replay worker on demand.
    Gated {
        release: mpsc::Receiver<()>,
        reason: StopReason,
    },
}

pub struct MockTarget {
    pub position: TtdPosition,
    pub current_tid: Option<u64>,
    pub lifetime: (TtdPosition, TtdPosition),
    pub thread_ids: Vec<u64>,
    pub modules: Vec<ModuleInfo>,
    pub regs: TtdX64Regs,
    pub teb: u64,
    pub forward: ForwardStop,
    pub backward: StopReason,
    pub fail_breakpoints: bool,
    /// Logical breakpoints, keyed by id (the mock uses the address as id,
    /// matching what the frontend tests assert on the wire).
    bps: BTreeMap<u64, u64>,
}

impl Default for MockTarget {
    fn default() -> Self {
        let mut st = [0u8; 80];
        st[0] = 0x3f;
        st[1] = 0x80;
        let mut xmm = [0u8; 256];
        xmm[0] = 0x2a;
        let regs = TtdX64Regs {
            rax: 0x11,
            rcx: 0x22,
            rip: 0x1000,
            rsp: 0x7ffc_0000,
            rbp: 0x7ffc_0100,
            eflags: 0x202,
            cs: 0x33,
            st,
            xmm,
            ..Default::default()
        };
        Self {
            position: TtdPosition {
                sequence: 1,
                steps: 0,
            },
            current_tid: Some(100),
            lifetime: (
                TtdPosition {
                    sequence: 0,
                    steps: 0,
                },
                TtdPosition {
                    sequence: 9,
                    steps: 1000,
                },
            ),
            thread_ids: vec![100],
            modules: Vec::new(),
            regs,
            teb: 0x7ffe_0000,
            forward: ForwardStop::BreakpointOrTraceEnd,
            backward: StopReason::TraceStart,
            fail_breakpoints: false,
            bps: BTreeMap::new(),
        }
    }
}

impl MockTarget {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_forward(mut self, forward: ForwardStop) -> Self {
        self.forward = forward;
        self
    }

    pub fn with_fixed_forward(self, reason: StopReason) -> Self {
        self.with_forward(ForwardStop::Fixed(reason))
    }

    pub fn with_modules(mut self, modules: Vec<ModuleInfo>) -> Self {
        self.modules = modules;
        self
    }

    pub fn with_failing_breakpoints(mut self) -> Self {
        self.fail_breakpoints = true;
        self
    }

    fn missing_thread(&self, id: u64) -> bool {
        !self.thread_ids.contains(&id)
    }

    fn registration_error(addr: u64) -> DebugError {
        DebugError::BreakpointRegister {
            addr,
            source: TtdError::Ffi("add_watchpoint failed"),
        }
    }
}

impl DebugTarget for MockTarget {
    fn step(&mut self) -> Result<StopReason, DebugError> {
        self.position.steps += 1;
        Ok(StopReason::StepComplete)
    }

    fn step_back(&mut self) -> Result<StopReason, DebugError> {
        self.position.steps = self.position.steps.saturating_sub(1);
        Ok(StopReason::StepComplete)
    }

    fn continue_forward(&mut self) -> Result<StopReason, DebugError> {
        match &mut self.forward {
            ForwardStop::BreakpointOrTraceEnd => match self.bps.keys().next().copied() {
                Some(addr) => Ok(StopReason::Breakpoint { bp_id: addr, addr }),
                None => {
                    self.position.steps += 100;
                    Ok(StopReason::TraceEnd)
                }
            },
            ForwardStop::Fixed(reason) => Ok(reason.clone()),
            ForwardStop::Gated { release, reason } => {
                let _ = release.recv();
                Ok(reason.clone())
            }
        }
    }

    fn continue_backward(&mut self) -> Result<StopReason, DebugError> {
        self.position.steps = 0;
        Ok(self.backward.clone())
    }

    fn goto(&mut self, pos: TtdPosition) -> Result<StopReason, DebugError> {
        self.position = pos;
        Ok(StopReason::PositionReached)
    }

    fn read_memory(&self, addr: u64, buf: &mut [u8]) -> usize {
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (addr + i as u64) as u8;
        }
        buf.len()
    }

    fn set_breakpoint(&mut self, addr: u64, _thread_id: Option<u64>) -> Result<u64, DebugError> {
        if self.fail_breakpoints {
            return Err(Self::registration_error(addr));
        }
        self.bps.insert(addr, addr);
        Ok(addr)
    }

    fn set_data_breakpoint(
        &mut self,
        addr: u64,
        _size: u64,
        _kind: BreakpointKind,
    ) -> Result<u64, DebugError> {
        if self.fail_breakpoints {
            return Err(Self::registration_error(addr));
        }
        self.bps.insert(addr, addr);
        Ok(addr)
    }

    fn remove_breakpoint(&mut self, id: u64) -> bool {
        self.bps.remove(&id).is_some()
    }

    fn remove_breakpoint_exact(&mut self, addr: u64, _size: u64, _access: u8) -> bool {
        self.bps.remove(&addr).is_some()
    }

    fn active_thread_ids(&self) -> Vec<u64> {
        self.thread_ids.clone()
    }

    fn thread_state(&self, thread_id: Option<u64>) -> Option<(TtdX64Regs, u64)> {
        let tid = thread_id.or(self.current_tid)?;
        if self.missing_thread(tid) {
            return None;
        }
        Some((self.regs, self.teb))
    }

    fn thread_info(&self, thread_id: u64) -> Option<ThreadExtraInfoData> {
        let index = self.thread_ids.iter().position(|id| *id == thread_id)?;
        Some(ThreadExtraInfoData {
            unique_id: (index + 1) as u32,
            current_position: self.position,
            teb: self.teb,
        })
    }

    fn current_thread_id(&self) -> Option<u64> {
        self.current_tid
    }

    fn set_current_thread(&mut self, id: u64) {
        self.current_tid = Some(id);
    }

    fn modules(&self) -> Vec<ModuleInfo> {
        self.modules.clone()
    }

    fn diagnostics(&self) -> TargetDiagnostics {
        TargetDiagnostics {
            first_position: self.lifetime.0,
            last_position: self.lifetime.1,
            current_position: self.position,
            threads: self
                .thread_ids
                .iter()
                .enumerate()
                .map(|(i, os_tid)| ThreadDiagnostics {
                    unique_id: (i + 1) as u32,
                    os_thread_id: *os_tid as u32,
                    active_time: (self.position, self.lifetime.1),
                })
                .collect(),
            exception_count: 0,
            module_count: self.modules.len() as u32,
        }
    }

    fn position(&self) -> TtdPosition {
        self.position
    }

    fn lifetime(&self) -> (TtdPosition, TtdPosition) {
        self.lifetime
    }
}
