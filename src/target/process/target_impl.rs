//! The `DebugTarget` implementation for [`TtdProcess`](super::TtdProcess) —
//! the whole backend contract in one view.
//!
//! Rust allows exactly one impl block per type+trait pair, so this file is
//! the single place every `DebugTarget` method lives, grouped by area:
//!
//! 1. **execution** — step / continue / `goto` (the bodies behind the verbs
//!    live in [`super::execution`]);
//! 2. **breakpoints** — pure delegation to `BreakpointState`;
//! 3. **queries** — read-only access to memory, threads, modules, and
//!    diagnostics.
//!
//! Shared cursor plumbing (`with_cursor`, `at_position`, `os_tid`, …) is in
//! [`super`].

use std::sync::Arc;

use crate::target::breakpoint::BreakpointKind;
use crate::target::{
    DebugError, DebugTarget, ModuleInfo, StopReason, TargetDiagnostics, TargetStats,
    ThreadExtraInfoData,
};
use crate::ttd::types::{DataAccessMask, TtdExceptionEvent, TtdMemRange, TtdPosition, TtdX64Regs};

use super::TtdProcess;

use crate::ttd::ReplayCursor;

/// Read up to `buf.len()` bytes into `buf`, returning how many were read.
fn read_into(cursor: &ReplayCursor, addr: u64, buf: &mut [u8]) -> usize {
    if buf.is_empty() {
        return 0;
    }
    match cursor.read_memory(addr, buf.len()) {
        Ok(bytes) => {
            let n = bytes.len().min(buf.len());
            buf[..n].copy_from_slice(&bytes[..n]);
            n
        }
        Err(_) => 0,
    }
}

/// Read memory and keep only the provenance runs that describe received
/// bytes.
fn read_ranges_into(
    cursor: &ReplayCursor,
    addr: u64,
    buf: &mut [u8],
    max_ranges: u32,
) -> (usize, Vec<TtdMemRange>) {
    if buf.is_empty() {
        return (0, Vec::new());
    }
    let Ok((bytes, runs)) = cursor.read_memory_ranges(
        addr,
        buf.len(),
        usize::try_from(max_ranges).unwrap_or(usize::MAX),
    ) else {
        return (0, Vec::new());
    };
    let n = bytes.len().min(buf.len());
    buf[..n].copy_from_slice(&bytes[..n]);
    let received = n as u64;
    let ranges = runs
        .into_iter()
        .filter(|run| {
            run.size() > 0
                && run
                    .Address
                    .checked_sub(addr)
                    .is_some_and(|offset| offset < received)
        })
        .collect();
    (n, ranges)
}

impl DebugTarget for TtdProcess {
    // ── Execution ───────────────────────────────────────────────

    fn step(&mut self) -> Result<StopReason, DebugError> {
        self.step_on_step_cursor(false)
    }

    fn step_back(&mut self) -> Result<StopReason, DebugError> {
        self.step_on_step_cursor(true)
    }

    /// Optimized range step: keep executing on the already-positioned step
    /// cursor and read the PC directly from that same cursor.
    ///
    /// The protocol-layer default would call `thread_state` after every
    /// instruction, which routes through the *persistent* query cursor and
    /// forces a `SetPosition` (possibly replaying from a keyframe) per step.
    /// Delve's `next` is a range step, so this path is the hot path for Go
    /// source-level stepping.
    ///
    /// Speculative-execution safety: this method deliberately does not use
    /// the watchpoint callback or its speculative-hit machinery. Each
    /// `step_on_step_cursor` call is a `StepCount` replay, and the PC is read
    /// from the exact stopped cursor position, so a speculatively observed
    /// future watchpoint can never influence the range decision.
    fn step_range(
        &mut self,
        thread_id: Option<u64>,
        start: u64,
        end: u64,
    ) -> Result<StopReason, DebugError> {
        if start >= end {
            return self.step_on_step_cursor(false);
        }
        // A tid that cannot name a trace thread (unknown, or too wide for
        // TTD's u32 namespace) falls back to the current thread; reading the
        // PC of a thread TTD has never seen would return 0 and make the range
        // look immediately exited.
        let tid = self.step_range_tid(thread_id);
        let mut previous = self.position();
        // A range step is many short `StepForward` calls, so the SDK-level
        // cursor interrupt cannot abort it; poll the interrupt epoch instead.
        // `TargetRunner` resets the epoch before every resume operation, so
        // any non-zero value here was requested during *this* operation —
        // including one that arrived while the worker thread was still
        // starting, which is why the loop tests it rather than a snapshot.
        loop {
            if self
                .interrupt_epoch
                .load(std::sync::atomic::Ordering::Acquire)
                != 0
            {
                return Ok(StopReason::Interrupted);
            }
            match self.step_on_step_cursor(false)? {
                StopReason::StepComplete => {}
                other => return Ok(other),
            }
            let pc = self
                .cursors
                .step()
                .cursor()
                .amd64_context(tid)
                .map_or(0, |c| c.Rip);
            if !(start..end).contains(&pc) {
                return Ok(StopReason::StepComplete);
            }
            let now = self.position();
            if now <= previous {
                // The trace ran out or the instruction made no progress;
                // stop instead of spinning forever in the range.
                return Ok(StopReason::StepComplete);
            }
            previous = now;
        }
    }

    fn continue_forward(&mut self) -> Result<StopReason, DebugError> {
        self.watchpoint_replay(self.lifetime.1, true)
    }

    fn continue_backward(&mut self) -> Result<StopReason, DebugError> {
        self.watchpoint_replay(self.lifetime.0, false)
    }

    fn goto(&mut self, pos: TtdPosition) -> Result<StopReason, DebugError> {
        // Clamp into the trace lifetime; TTD's SetPosition rounds to the
        // closest valid position anyway, but keep our bookkeeping exact.
        let pos = pos.clamp(self.lifetime.0, self.lifetime.1);
        self.position = pos;
        // Reposition the persistent cursor too: read queries only
        // re-position it lazily via `with_cursor`, so without this a `goto`
        // followed by an engine-level read (or a fresh session's first
        // `read_memory` before any other op) would still observe the old
        // position's memory.
        self.at_position(pos, |_| ());
        self.adopt_cursor_position();
        Ok(StopReason::PositionReached)
    }

    // ── Breakpoints ─────────────────────────────────────────────

    fn set_breakpoint(&mut self, addr: u64, thread_id: Option<u64>) -> Result<u64, DebugError> {
        self.breakpoints
            .set_breakpoint(addr, thread_id, self.cursors.persistent().cell())
    }

    fn set_data_breakpoint(
        &mut self,
        addr: u64,
        size: u64,
        kind: BreakpointKind,
    ) -> Result<u64, DebugError> {
        self.breakpoints
            .set_data_breakpoint(addr, size, kind, self.cursors.persistent().cell())
    }

    fn remove_breakpoint(&mut self, id: u64) -> bool {
        self.breakpoints
            .remove_breakpoint(id, self.cursors.persistent().cell())
    }

    fn remove_breakpoint_exact(&mut self, addr: u64, size: u64, access: DataAccessMask) -> bool {
        self.breakpoints.remove_breakpoint_exact(
            addr,
            size,
            access,
            self.cursors.persistent().cell(),
        )
    }

    // ── Queries ─────────────────────────────────────────────────

    fn read_memory(&self, addr: u64, buf: &mut [u8]) -> usize {
        self.with_cursor(|c| read_into(c, addr, buf))
    }

    fn read_memory_ranges(
        &self,
        addr: u64,
        buf: &mut [u8],
        max_ranges: u32,
    ) -> (usize, Vec<TtdMemRange>) {
        self.with_cursor(|c| read_ranges_into(c, addr, buf, max_ranges))
    }

    fn active_thread_ids(&self) -> Vec<u64> {
        self.with_cursor(|c| {
            let count = c.active_thread_count();
            let mut ids = Vec::with_capacity(count);
            for i in 0..count {
                if let Some(entry) = c.active_thread(i) {
                    let info = entry.thread;
                    // A hole in the engine's active list is reported as a
                    // zeroed entry. OS tid 0 is not a thread and cannot be
                    // advertised on the wire (`Tid` is non-zero).
                    if info.Id != 0 {
                        ids.push(u64::from(info.Id));
                    }
                }
            }
            ids
        })
    }

    fn thread_state(&self, thread_id: Option<u64>) -> Option<(TtdX64Regs, u64)> {
        let tid = self.os_tid(thread_id)?;
        // A tid that never appears in the trace cannot have a state: TTD
        // returns a zeroed context for threads it does not know, and serving
        // all-zero registers (RIP = 0, RSP = 0) to a client that asked about
        // a stale thread is worse than reporting "no such thread".
        if !self.metadata.threads().contains_os_tid(tid) {
            return None;
        }
        self.with_cursor(|c| {
            let regs = c.amd64_context(tid).unwrap_or_default();
            let teb = c.teb_address_of(tid);
            Some((regs, teb))
        })
    }

    fn thread_info(&self, thread_id: u64) -> Option<ThreadExtraInfoData> {
        // Only a thread that fits TTD's OS-tid space can be in the trace, so
        // the lookup and the narrowing happen together.
        let os_tid = u32::try_from(thread_id).ok()?;
        self.with_cursor(|c| {
            let count = c.active_thread_count();
            for i in 0..count {
                // Skip holes in the enumeration like `active_thread_ids`
                // does: one unreadable entry must not sink the whole query.
                let Some(entry) = c.active_thread(i) else {
                    continue;
                };
                let info = entry.thread;
                if info.Id == 0 {
                    // Null/hole entry: no thread to describe.
                    continue;
                }
                if u64::from(info.Id) == thread_id {
                    return Some(ThreadExtraInfoData {
                        unique_id: info.UniqueId,
                        current_position: entry.current_position,
                        teb: c.teb_address_of(os_tid),
                    });
                }
            }
            None
        })
    }

    fn current_thread_id(&self) -> Option<u64> {
        self.current_thread_id
    }

    fn set_current_thread(&mut self, id: u64) {
        self.current_thread_id = Some(id);
    }

    fn modules(&self) -> Vec<ModuleInfo> {
        self.metadata.modules().to_vec()
    }

    fn main_module(&self) -> Option<ModuleInfo> {
        // TTD reports the recorded executable first in the module list.
        self.metadata.modules().first().cloned()
    }

    fn exceptions(&self) -> Vec<TtdExceptionEvent> {
        self.metadata.exceptions().to_vec()
    }

    fn stats(&self) -> TargetStats {
        // Cursor counters live in `self.stats`; physical watchpoint counters
        // live in `BreakpointState` (atomics because the watchpoint callback
        // shares `&BreakpointState`). Expose one flat snapshot to monitor.
        let mut stats = self.stats.get();
        let watchpoints = self.breakpoints.watchpoint_stats();
        stats.watchpoint_adds = watchpoints.adds;
        stats.watchpoint_removes = watchpoints.removes;
        stats
    }

    fn diagnostics(&self) -> TargetDiagnostics {
        self.metadata.diagnostics(self.position)
    }

    fn position(&self) -> TtdPosition {
        self.position
    }

    fn lifetime(&self) -> (TtdPosition, TtdPosition) {
        self.lifetime
    }

    // ── Interrupt pipeline ──────────────────────────────────────

    fn begin_resume(&self) {
        // Range stepping polls this counter; a new operation must not inherit
        // an interrupt requested while the target was stopped.
        self.interrupt_epoch
            .store(0, std::sync::atomic::Ordering::Release);
    }

    fn interrupt_handle(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        let active = self.active_replay.clone();
        let epoch = Arc::clone(&self.interrupt_epoch);
        Some(Arc::new(move || {
            // Tell the range-step loop (which issues many short steps) before
            // poking the cursor slot (which aborts one long replay). Both
            // halves are load-bearing; see `InterruptSlot`.
            epoch.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            active.interrupt();
        }))
    }
}
