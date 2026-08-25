//! Test double shared by the `target` and `target::base` unit tests.
//!
//! Compiled only under `#[cfg(test)]`; it exists so each test module can
//! build a minimal `DebugTarget` without duplicating the impl.

use crate::target::{
    BreakpointKind, DebugError, DebugTarget, ModuleInfo, StopReason, TargetDiagnostics,
    ThreadExtraInfoData,
};
use crate::ttd::types::{TtdPosition, TtdX64Regs};

#[derive(Default)]
pub(crate) struct Recorder {
    pub(crate) steps: usize,
    pub(crate) steps_back: usize,
    pub(crate) continues: usize,
    pub(crate) current_thread: Option<u64>,
    pub(crate) range_steps: Vec<(Option<u64>, u64, u64)>,
    pub(crate) panics: bool,
}
impl DebugTarget for Recorder {
    fn step(&mut self) -> Result<StopReason, DebugError> {
        self.steps += 1;
        Ok(StopReason::StepComplete)
    }
    fn step_back(&mut self) -> Result<StopReason, DebugError> {
        self.steps_back += 1;
        Ok(StopReason::StepComplete)
    }
    fn continue_forward(&mut self) -> Result<StopReason, DebugError> {
        assert!(!self.panics, "recorder panic");
        self.continues += 1;
        Ok(StopReason::TraceEnd)
    }
    fn continue_backward(&mut self) -> Result<StopReason, DebugError> {
        Ok(StopReason::TraceStart)
    }
    fn step_range(
        &mut self,
        thread_id: Option<u64>,
        start: u64,
        end: u64,
    ) -> Result<StopReason, DebugError> {
        self.range_steps.push((thread_id, start, end));
        Ok(StopReason::StepComplete)
    }
    fn goto(&mut self, _pos: TtdPosition) -> Result<StopReason, DebugError> {
        Ok(StopReason::PositionReached)
    }
    fn read_memory(&self, _addr: u64, _buf: &mut [u8]) -> usize {
        0
    }
    fn set_breakpoint(&mut self, _addr: u64, _t: Option<u64>) -> Result<u64, DebugError> {
        Ok(1)
    }
    fn set_data_breakpoint(
        &mut self,
        _a: u64,
        _s: u64,
        _k: BreakpointKind,
    ) -> Result<u64, DebugError> {
        Ok(1)
    }
    fn remove_breakpoint(&mut self, _id: u64) -> bool {
        true
    }
    fn active_thread_ids(&self) -> Vec<u64> {
        vec![100]
    }
    fn thread_info(&self, _id: u64) -> Option<ThreadExtraInfoData> {
        None
    }
    fn thread_state(&self, _id: Option<u64>) -> Option<(TtdX64Regs, u64)> {
        Some((TtdX64Regs::default(), 0))
    }
    fn current_thread_id(&self) -> Option<u64> {
        Some(100)
    }
    fn set_current_thread(&mut self, id: u64) {
        self.current_thread = Some(id);
    }
    fn modules(&self) -> Vec<ModuleInfo> {
        vec![]
    }
    fn diagnostics(&self) -> TargetDiagnostics {
        unimplemented!("unused")
    }
    fn position(&self) -> TtdPosition {
        TtdPosition::default()
    }
    fn lifetime(&self) -> (TtdPosition, TtdPosition) {
        Default::default()
    }
}
