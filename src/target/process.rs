use std::cell::Cell;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::ttd::{TtdCursor, TtdEngine, types::*};

use super::breakpoint::BreakpointKind;
use super::breakpoint_state::BreakpointState;
use super::cursors::CursorPair;
use super::error::DebugError;
use super::stop_reason::StopReason;
use super::threads::ThreadTable;
use super::{
    DebugTarget, ModuleInfo, TargetDiagnostics, TargetStats, ThreadDiagnostics, ThreadExtraInfoData,
};

mod replay;

// ─── TtdProcess ──────────────────────────────────────────────

/// Instruction-level debug engine backed by TTD Replay.
///
/// Implements `DebugTarget` (the target-layer API consumed by protocol
/// frontends). Owns the TTD engine and two long-lived replay cursors — one
/// persistent (carries the client's watchpoints, serves every query) and one
/// for stepping (watchpoint-free) — both repositioned lazily only when the
/// debug position has actually moved. Cursor creation is expensive (a step on
/// a throwaway cursor costs ~150× a step on a reused one) and the GDB layer
/// issues several queries between every two stops (`g`, `m`, thread listing),
/// so a fresh-cursor-per-call design would dominate latency.
///
/// The cursors live in `CursorPair` as `Box<TtdCursor>` handles: a cursor is
/// C++ state, so its operations take `&self` (interior mutability across the
/// FFI boundary) and the trait's `&self` query methods need no `RefCell`.
/// (The boxes are not load-bearing for callback soundness: the C side holds
/// the callback slot's heap address, which is stable across cursor moves —
/// see `TtdCursor`.)
/// This is race-free by construction: protocol frontends only issue queries
/// while stopped, and while a replay is running the frontend's backend lock is
/// held by the replay worker, excluding all other callers.
pub struct TtdProcess {
    /// Long-lived cursors: persistent/query (carries watchpoints) and step
    /// (watchpoint-free). Both cache their last position.
    cursors: CursorPair,
    engine: TtdEngine,
    /// Replay/query counters (see `DebugTarget::stats`).
    stats: Cell<TargetStats>,

    position: TtdPosition,
    lifetime: (TtdPosition, TtdPosition),
    current_thread_id: Option<u64>,
    /// Immutable OS-tid → UniqueThreadId catalog shared with the breakpoint
    /// layer.
    threads: Arc<ThreadTable>,
    /// Logical breakpoints + their physical TTD watchpoint set and counters.
    breakpoints: BreakpointState,

    /// Raw handle of the cursor while a blocking replay is in flight (null
    /// otherwise). Read from any thread by the interrupt handle (^C).
    ///
    /// Safety: only ever dereferenced to call `ttd_cursor_interrupt`, which
    /// is documented thread-safe, and only while it is non-null — i.e.
    /// strictly between the store/clear pair around the replay call on the
    /// owning thread, which happens while the whole `TtdProcess` is alive.
    active_replay: Arc<AtomicPtr<crate::ttd::ffi::TtdCursor>>,
}

impl TtdProcess {
    /// Open a trace file and create the debug engine.
    pub fn open(trace_path: &Path) -> Result<Self, DebugError> {
        let engine = TtdEngine::open(trace_path)?;
        let lifetime = engine.lifetime();
        let first = engine.first_position();
        let thread_list = engine.thread_list();
        let first_tid = thread_list.first().map(|t| t.os_thread_id as u64);
        let threads = Arc::new(ThreadTable::from_engine_threads(&thread_list));

        // One persistent cursor for the whole process lifetime (it starts at
        // Position::Min), plus one dedicated to stepping. Both are
        // repositioned lazily before each use.
        let cursors = CursorPair::new(&engine)?;

        Ok(Self {
            cursors,
            engine,
            stats: Cell::new(TargetStats::default()),
            position: first,
            lifetime: (lifetime.min, lifetime.max),
            current_thread_id: first_tid,
            threads: threads.clone(),
            breakpoints: BreakpointState::new(threads),
            active_replay: Arc::new(AtomicPtr::new(std::ptr::null_mut())),
        })
    }

    // ─── Internal helpers ────────────────────────────────────

    /// Access the persistent cursor without repositioning it (internal
    /// bookkeeping around replay calls).
    fn persistent<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&TtdCursor) -> R,
    {
        f(self.cursors.persistent().cursor())
    }

    /// Position the persistent cursor at `pos` (unless it is already there)
    /// and run `f`.
    ///
    /// `PositionedCursor::prepare` reads the actual landing position back
    /// because TTD rounds `SetPosition` to the nearest valid position. That
    /// keeps the cache honest: a cursor that landed off-target is repositioned
    /// next time instead of silently serving a neighbouring position forever.
    fn at_position<F, R>(&self, pos: TtdPosition, f: F) -> R
    where
        F: FnOnce(&TtdCursor) -> R,
    {
        if self.cursors.persistent().prepare(pos) {
            self.bump(|s| s.cursor_seeks += 1);
        }
        f(self.cursors.persistent().cursor())
    }

    /// Position the persistent cursor at the current debug position and run
    /// `f`. Every query goes through here so callers never see a stale
    /// cursor.
    fn with_cursor<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&TtdCursor) -> R,
    {
        // Only real client reads count as queries: `goto` and
        // `watchpoint_replay` call `at_position` directly for bookkeeping, and
        // counting those would deflate the seeks-per-query health metric.
        self.bump(|s| s.queries += 1);
        self.at_position(self.position, f)
    }

    /// Update the counters without a get/modify/set dance at each call site.
    fn bump(&self, f: impl FnOnce(&mut TargetStats)) {
        let mut stats = self.stats.get();
        f(&mut stats);
        self.stats.set(stats);
    }
}

// ─── DebugTarget implementation ───────────────────────────────

impl DebugTarget for TtdProcess {
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
        let tid = thread_id.or(self.current_thread_id).unwrap_or(0) as u32;
        let mut previous = self.position();
        loop {
            match self.step_on_step_cursor(false)? {
                StopReason::StepComplete => {}
                other => return Ok(other),
            }
            let pc = self.cursors.step().cursor().pc_thread(tid);
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
        // Reposition the persistent cursor too: read queries only
        // re-position it lazily via `with_cursor`, so without this a `goto`
        // followed by an engine-level read (or a fresh session's first
        // `read_memory` before any other op) would still observe the old
        // position's memory.
        self.at_position(pos, |_| ());
        // `SetPosition` rounds to the closest valid position and `at_position`
        // caches the *rounded* landing; adopt it as the debug position too,
        // or every later `with_cursor` sees request != cache and re-seeks
        // (landing at the same rounded spot again) on every single query.
        self.position = self.persistent(|c| c.position());
        // The destination position has its own thread: after a jump the
        // previous current thread may not even exist yet, and a stale tid
        // would make every register/memory query target the wrong thread.
        let tid = self.persistent(|c| c.current_tid());
        if tid != 0 {
            self.current_thread_id = Some(tid as u64);
        }
        Ok(StopReason::PositionReached)
    }

    fn read_memory(&self, addr: u64, buf: &mut [u8]) -> usize {
        self.with_cursor(|c| c.read_memory(addr, buf))
    }

    fn set_breakpoint(&mut self, addr: u64, thread_id: Option<u64>) -> Result<u64, DebugError> {
        self.breakpoints
            .set_breakpoint(addr, thread_id, self.cursors.persistent().cursor())
    }

    fn set_data_breakpoint(
        &mut self,
        addr: u64,
        size: u64,
        kind: BreakpointKind,
    ) -> Result<u64, DebugError> {
        self.breakpoints
            .set_data_breakpoint(addr, size, kind, self.cursors.persistent().cursor())
    }

    fn remove_breakpoint(&mut self, id: u64) -> bool {
        self.breakpoints
            .remove_breakpoint(id, self.cursors.persistent().cursor())
    }

    fn remove_breakpoint_exact(&mut self, addr: u64, size: u64, access: u8) -> bool {
        self.breakpoints.remove_breakpoint_exact(
            addr,
            size,
            access,
            self.cursors.persistent().cursor(),
        )
    }
    fn active_thread_ids(&self) -> Vec<u64> {
        self.with_cursor(|c| {
            let count = c.thread_count();
            let mut ids = Vec::with_capacity(count as usize);
            for i in 0..count {
                if let Some(info) = c.active_thread_info(i) {
                    ids.push(info.thread.os_thread_id as u64);
                }
            }
            ids
        })
    }

    fn thread_state(&self, thread_id: Option<u64>) -> Option<(TtdX64Regs, u64)> {
        let tid = match thread_id {
            Some(id) => id as u32,
            None => self.current_thread_id? as u32,
        };
        // A tid that never appears in the trace cannot have a state: TTD
        // returns a zeroed context for threads it does not know, and serving
        // all-zero registers (RIP = 0, RSP = 0) to a client that asked about
        // a stale thread is worse than reporting "no such thread".
        if !self.threads.contains_os_tid(tid) {
            return None;
        }
        self.with_cursor(|c| {
            let regs = c.read_regs_thread(tid);
            let teb = c.teb_thread(tid);
            Some((regs, teb))
        })
    }

    fn thread_info(&self, thread_id: u64) -> Option<ThreadExtraInfoData> {
        self.with_cursor(|c| {
            let count = c.thread_count();
            for i in 0..count {
                let info = c.active_thread_info(i)?;
                if info.thread.os_thread_id as u64 == thread_id {
                    return Some(ThreadExtraInfoData {
                        unique_id: info.thread.unique_id,
                        current_position: info.current_position,
                        teb: c.teb_thread(thread_id as u32),
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

    fn exceptions(&self) -> Vec<TtdExceptionEvent> {
        self.engine.exception_list()
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

    fn interrupt_handle(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        let active = self.active_replay.clone();
        Some(Arc::new(move || {
            // Safety: a non-null pointer here is guaranteed (store/clear pair
            // in `watchpoint_replay`) to point at the live C++ cursor of this
            // `TtdProcess`; `ttd_cursor_interrupt` is documented callable
            // from any thread.
            let ptr = active.load(Ordering::Acquire);
            if !ptr.is_null() {
                unsafe { crate::ttd::ffi::ttd_cursor_interrupt(ptr) };
            }
        }))
    }
}

impl Drop for TtdProcess {
    fn drop(&mut self) {
        // Clearing the interrupt slot is the only thing that must happen
        // before drop glue runs: the interrupt handle is an `Arc<dyn Fn()>`
        // that may outlive this process (a ^C from another thread), and a
        // non-null slot would make it poke a cursor that no longer exists.
        // Cursors keep the engine alive themselves, so no ordering is needed
        // for them.
        self.active_replay
            .store(std::ptr::null_mut(), Ordering::Release);
    }
}
