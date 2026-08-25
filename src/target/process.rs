use std::ffi::c_void;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::ttd::types::*;
use crate::ttd::TtdEngine;

use super::breakpoint::{BreakpointKind, BreakpointManager};
use super::error::DebugError;
use super::stop_reason::StopReason;
use super::{DebugTarget, ModuleInfo, TargetDiagnostics, ThreadDiagnostics, ThreadExtraInfoData};

// ─── Continue callback ─────────────────────────────────────────

/// Callback context for watchpoint-hit detection during continue.
struct ContinueContext<'a> {
    breakpoints: &'a BreakpointManager,
    hit_bp_id: Option<u64>,
}

unsafe extern "C" fn continue_watchpoint_callback(
    ctx: *mut c_void,
    addr: u64,
    size: u64,
    access: u8,
) -> bool {
    let ctx = &mut *(ctx as *mut ContinueContext<'_>);
    if let Some(bp) = ctx.breakpoints.find_by_watchpoint_hit(addr, size, access) {
        ctx.hit_bp_id = Some(bp.id);
        return true;
    }
    false
}

// ─── Interrupt support ───────────────────────────────────

/// Raw cursor handle shared across threads so a blocked replay can be
/// interrupted from the socket reader thread.
#[derive(Clone, Copy)]
struct CursorPtr(*mut crate::ttd::ffi::TtdCursor);

// Safety: the pointer is only ever dereferenced to call
// `ttd_cursor_interrupt`, which the wrapper documents as callable from any
// thread, and only while the owning TtdProcess (and engine) is alive.
unsafe impl Send for CursorPtr {}
unsafe impl Sync for CursorPtr {}

// ─── TtdProcess ──────────────────────────────────────────────

/// Instruction-level debug engine backed by TTD Replay.
///
/// Implements `DebugTarget` (the target-layer API consumed by protocol
/// frontends). Owns the TTD engine. Creates cursors on demand for each
/// operation.
pub struct TtdProcess {
    engine: TtdEngine,
    position: TtdPosition,
    lifetime: (TtdPosition, TtdPosition),
    current_thread_id: Option<u64>,
    breakpoints: BreakpointManager,
    /// Raw handle of the cursor currently in a blocking replay, if any.
    /// Used by `interrupt()` to abort a replay from another thread (^C).
    active_cursor: Arc<Mutex<Option<CursorPtr>>>,
}

impl TtdProcess {
    /// Open a trace file and create the debug engine.
    pub fn open(trace_path: &Path) -> Result<Self, DebugError> {
        let engine = TtdEngine::open(trace_path)?;
        let lifetime = engine.lifetime();
        let first = engine.first_position();
        let first_tid = engine.thread_list().first().map(|t| t.os_thread_id as u64);

        Ok(Self {
            engine,
            position: first,
            lifetime: (lifetime.min, lifetime.max),
            current_thread_id: first_tid,
            breakpoints: BreakpointManager::new(),
            active_cursor: Arc::new(Mutex::new(None)),
        })
    }

    /// Returns a thread-safe closure that interrupts an in-flight replay.
    /// Safe to call from any thread (e.g. a socket reader that saw ^C).
    /// Does nothing when no replay is running.
    pub fn interrupt(&self) -> Arc<dyn Fn() + Send + Sync> {
        let active = self.active_cursor.clone();
        Arc::new(move || {
            let ptr = *active.lock().unwrap();
            if let Some(CursorPtr(p)) = ptr {
                // Safety: the pointer is only ever observed while the owning
                // TtdProcess is alive and a replay is in flight; sessions must
                // stop interrupting before dropping the process.
                unsafe { crate::ttd::ffi::ttd_cursor_interrupt(p) };
            }
        })
    }

    // ─── Internal helpers ────────────────────────────────────

    fn with_cursor<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&mut crate::ttd::TtdCursor<'_>) -> R,
    {
        let mut cursor = self.engine.create_cursor().ok()?;
        cursor.set_position(self.position);
        Some(f(&mut cursor))
    }

    fn apply_breakpoints(&self, cursor: &mut crate::ttd::TtdCursor<'_>) {
        for bp in self.breakpoints.active() {
            let tid = bp.thread_id.unwrap_or(0) as u32; // 0 = any thread for TTD
            let _ = cursor.add_watchpoint(bp.addr, bp.size, bp.access_mask(), tid);
        }
    }

    /// Replay with watchpoints — the core pattern from TTD's `FilteredWatchpointQuery`.
    /// Creates a cursor, applies breakpoints as TTD watchpoints, sets the callback,
    /// replays to the given limit, and returns the stop reason.
    /// The cursor (and its C++ callbacks) are cleaned up on scope exit via Drop.
    fn watchpoint_replay(
        &mut self,
        limit: TtdPosition,
        forward: bool,
    ) -> Result<StopReason, DebugError> {
        let mut cursor = self.engine.create_cursor()?;
        cursor.set_position(self.position);
        self.apply_breakpoints(&mut cursor);

        // Don't stop on exceptions during continue: Delve expects to
        // "propagate" fault signals by resuming, so exception stops here
        // would just loop. Single-step is unaffected (no event mask needed).
        cursor.set_exception_mask(0);

        let mut ctx = ContinueContext {
            breakpoints: &self.breakpoints,
            hit_bp_id: None,
        };

        // Safety: ctx is pinned on the stack and outlives the cursor.
        unsafe {
            cursor.set_watchpoint_callback_raw(
                continue_watchpoint_callback,
                &mut ctx as *mut ContinueContext<'_> as *mut c_void,
            );
        }
        cursor.set_event_mask(TTD_Replay_EventMask_MemoryWatchpoint);

        // Register the cursor so another thread can interrupt the replay (^C).
        *self.active_cursor.lock().unwrap() = Some(CursorPtr(cursor.as_raw()));
        let result = if forward {
            cursor.replay_forward_to(limit)
        } else {
            cursor.replay_backward_to(limit)
        };
        *self.active_cursor.lock().unwrap() = None;
        let result = result?;

        self.position = cursor.position();
        // The thread that reached the stop event becomes the current thread.
        let tid = cursor.current_tid();
        if tid != 0 {
            self.current_thread_id = Some(tid as u64);
        }
        // cursor::Drop clears callbacks before destroying the C++ cursor.

        if let Some(bp_id) = ctx.hit_bp_id {
            let bp = self.breakpoints.get(bp_id);
            let addr = bp.map(|b| b.addr).unwrap_or(result.wp_address);
            match bp.map(|b| b.kind) {
                Some(BreakpointKind::Execute) => Ok(StopReason::Breakpoint { bp_id, addr }),
                _ => Ok(StopReason::Watchpoint { bp_id, addr }),
            }
        } else {
            // Hitting a trace boundary is a stronger signal than the generic
            // event TTD reports for it: Delve expects sig 0 at trace start
            // (backward) and its synthetic SIGKILL at trace end (forward).
            if !forward && self.position <= self.lifetime.0 {
                return Ok(StopReason::TraceStart);
            }
            if forward && self.position >= self.lifetime.1 {
                return Ok(StopReason::TraceEnd);
            }
            Ok(StopReason::from_replay_result(&result))
        }
    }
}

// ─── DebugTarget implementation ───────────────────────────────

impl DebugTarget for TtdProcess {
    fn step(&mut self) -> Result<StopReason, DebugError> {
        let mut cursor = self.engine.create_cursor()?;
        cursor.set_position(self.position);
        let result = cursor.step_forward(1)?;
        self.position = cursor.position();
        let tid = cursor.current_tid();
        if tid != 0 {
            self.current_thread_id = Some(tid as u64);
        }
        Ok(StopReason::from_replay_result(&result))
    }

    fn step_back(&mut self) -> Result<StopReason, DebugError> {
        let mut cursor = self.engine.create_cursor()?;
        cursor.set_position(self.position);
        let result = cursor.step_backward(1)?;
        self.position = cursor.position();
        let tid = cursor.current_tid();
        if tid != 0 {
            self.current_thread_id = Some(tid as u64);
        }
        Ok(StopReason::from_replay_result(&result))
    }

    fn continue_forward(&mut self) -> Result<StopReason, DebugError> {
        self.watchpoint_replay(self.lifetime.1, true)
    }

    fn continue_backward(&mut self) -> Result<StopReason, DebugError> {
        self.watchpoint_replay(self.lifetime.0, false)
    }

    fn goto(&mut self, mut pos: TtdPosition) -> Result<StopReason, DebugError> {
        // Clamp into the trace lifetime; TTD's SetPosition rounds to the
        // closest valid position anyway, but keep our bookkeeping exact.
        if pos < self.lifetime.0 {
            pos = self.lifetime.0;
        }
        if pos > self.lifetime.1 {
            pos = self.lifetime.1;
        }
        self.position = pos;
        Ok(StopReason::PositionReached)
    }

    fn read_memory(&self, addr: u64, buf: &mut [u8]) -> usize {
        self.with_cursor(|c| c.read_memory(addr, buf)).unwrap_or(0)
    }

    fn set_breakpoint(&mut self, addr: u64, thread_id: Option<u64>) -> u64 {
        self.breakpoints.add(
            addr,
            1,
            TTD_Replay_DataAccessMask_Execute,
            BreakpointKind::Execute,
            thread_id,
            false,
        )
    }

    fn set_data_breakpoint(&mut self, addr: u64, size: u64, kind: BreakpointKind) -> u64 {
        let access = match kind {
            BreakpointKind::Execute => TTD_Replay_DataAccessMask_Execute,
            BreakpointKind::Read => TTD_Replay_DataAccessMask_Read,
            BreakpointKind::Write => TTD_Replay_DataAccessMask_Write,
            BreakpointKind::Access => {
                TTD_Replay_DataAccessMask_Read | TTD_Replay_DataAccessMask_Write
            }
        };
        self.breakpoints.add(addr, size, access, kind, None, false)
    }

    fn remove_breakpoint(&mut self, id: u64) -> bool {
        self.breakpoints.remove(id)
    }

    fn active_thread_ids(&self) -> Vec<u64> {
        let mut cursor = match self.engine.create_cursor() {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        cursor.set_position(self.position);
        let count = cursor.thread_count();
        let mut ids = Vec::with_capacity(count as usize);
        for i in 0..count {
            if let Some(info) = cursor.active_thread_info(i) {
                ids.push(info.thread.os_thread_id as u64);
            }
        }
        ids
    }

    fn thread_state(&self, thread_id: Option<u64>) -> Option<(TtdX64Regs, u64)> {
        let tid = match thread_id {
            Some(id) => id as u32,
            None => self.current_thread_id? as u32,
        };
        let mut cursor = self.engine.create_cursor().ok()?;
        cursor.set_position(self.position);
        let regs = cursor.read_regs_thread(tid);
        let teb = cursor.teb_thread(tid);
        Some((regs, teb))
    }

    fn thread_info(&self, thread_id: u64) -> Option<ThreadExtraInfoData> {
        let mut cursor = self.engine.create_cursor().ok()?;
        cursor.set_position(self.position);
        let count = cursor.thread_count();
        for i in 0..count {
            let info = cursor.active_thread_info(i)?;
            if info.thread.os_thread_id as u64 == thread_id {
                return Some(ThreadExtraInfoData {
                    unique_id: info.thread.unique_id,
                    current_position: info.current_position,
                    teb: cursor.teb_thread(thread_id as u32),
                });
            }
        }
        None
    }

    fn current_thread_id(&self) -> Option<u64> {
        self.current_thread_id
    }

    fn set_current_thread(&mut self, id: u64) {
        self.current_thread_id = Some(id);
    }

    fn modules(&self) -> Vec<ModuleInfo> {
        let count = self.engine.module_count();
        let mut result = Vec::with_capacity(count as usize);
        for i in 0..count {
            if let Some((base_addr, size, name)) = self.engine.module_info(i) {
                result.push(ModuleInfo {
                    base_addr,
                    size,
                    name,
                });
            }
        }
        result
    }

    fn diagnostics(&self) -> TargetDiagnostics {
        let threads = self
            .engine
            .thread_list()
            .into_iter()
            .map(|t| ThreadDiagnostics {
                unique_id: t.unique_id,
                os_thread_id: t.os_thread_id,
                active_time: (t.active_time.min, t.active_time.max),
            })
            .collect();
        TargetDiagnostics {
            first_position: self.lifetime.0,
            last_position: self.lifetime.1,
            current_position: self.position,
            threads,
            exception_count: self.engine.exception_count(),
            module_count: self.engine.module_count(),
        }
    }

    fn position(&self) -> TtdPosition {
        self.position
    }

    fn lifetime(&self) -> (TtdPosition, TtdPosition) {
        self.lifetime
    }
}
