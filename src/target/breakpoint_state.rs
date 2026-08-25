//! Breakpoint/watchpoint domain state.
//!
//! One primary concern: translating the client's logical breakpoints
//! (`BreakpointManager`) into the physical TTD watchpoint set
//! (`WatchpointRegistry`) on the persistent cursor, including fallible
//! registration, address-keyed removal, per-thread OS-tid → `UniqueThreadId`
//! translation, and physical-operation counters.
//!
//! `TtdProcess` owns one of these and delegates every `DebugTarget`
//! breakpoint method here; the cursor is passed in because the state does not
//! own replay resources.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::ttd::ReplayCursor;

use crate::ttd::TtdError;
use crate::ttd::types::DataAccessMask;

use super::breakpoint::{Breakpoint, BreakpointId, BreakpointKind, BreakpointManager};
use super::error::DebugError;
use super::threads::ThreadTable;
use super::watchpoint::{WatchpointKey, WatchpointRegistry, valid_watchpoint_range};

/// Physical watchpoint counters (merged into `TargetStats` by `TtdProcess`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WatchpointStats {
    pub adds: u64,
    pub removes: u64,
}

/// The small slice of cursor functionality `BreakpointState` needs.
///
/// Abstracting it over the concrete [`ReplayCursor`] keeps the state machine
/// unit-testable with a fake sink that can fail adds/removes deterministically
/// instead of needing a live TTD trace.
pub(crate) trait WatchpointCursor {
    fn add_watchpoint(&self, key: WatchpointKey) -> Result<(), TtdError>;
    fn remove_watchpoint(&self, key: WatchpointKey) -> Result<(), TtdError>;
}

impl WatchpointCursor for RefCell<ReplayCursor> {
    fn add_watchpoint(&self, key: WatchpointKey) -> Result<(), TtdError> {
        if !valid_watchpoint_range(key.addr, key.size) {
            return Err(TtdError::InvalidWatchpoint {
                addr: key.addr,
                size: key.size,
            });
        }
        let accepted = self
            .borrow_mut()
            .watchpoint(key.addr, key.size, key.access, key.tid);
        if matches!(accepted, Ok(true)) {
            Ok(())
        } else {
            Err(TtdError::watchpoint(
                "add_watchpoint",
                key.addr,
                key.size,
                key.access,
            ))
        }
    }

    fn remove_watchpoint(&self, key: WatchpointKey) -> Result<(), TtdError> {
        if !valid_watchpoint_range(key.addr, key.size) {
            return Err(TtdError::InvalidWatchpoint {
                addr: key.addr,
                size: key.size,
            });
        }
        let accepted = self
            .borrow_mut()
            .remove_watchpoint(key.addr, key.size, key.access, key.tid);
        if matches!(accepted, Ok(true)) {
            Ok(())
        } else {
            Err(TtdError::watchpoint(
                "remove_watchpoint",
                key.addr,
                key.size,
                key.access,
            ))
        }
    }
}
/// Logical breakpoints + physical watchpoint synchronization state.
pub(crate) struct BreakpointState {
    manager: BreakpointManager,
    watchpoints: WatchpointRegistry,
    /// Shared immutable OS-tid → `UniqueThreadId` catalog.
    threads: Arc<ThreadTable>,
    /// Atomics rather than `Cell`: the watchpoint callback captures a
    /// `&BreakpointState` inside a `Send` closure, so the state must be
    /// `Sync`.
    watchpoint_adds: AtomicU64,
    watchpoint_removes: AtomicU64,
}

impl BreakpointState {
    pub(crate) fn new(threads: Arc<ThreadTable>) -> Self {
        Self {
            manager: BreakpointManager::new(),
            watchpoints: WatchpointRegistry::new(),
            threads,
            watchpoint_adds: AtomicU64::new(0),
            watchpoint_removes: AtomicU64::new(0),
        }
    }

    /// Find the logical breakpoint a TTD watchpoint hit belongs to.
    pub(crate) fn find_by_watchpoint_hit_and_thread(
        &self,
        addr: u64,
        size: u64,
        access: DataAccessMask,
        thread_id: u64,
    ) -> Option<&Breakpoint> {
        self.manager
            .find_by_watchpoint_hit_and_thread(addr, size, access, thread_id)
    }

    /// Look up a logical breakpoint by id.
    pub(crate) fn get(&self, id: BreakpointId) -> Option<&Breakpoint> {
        self.manager.get(id)
    }
    pub(crate) fn watchpoint_stats(&self) -> WatchpointStats {
        WatchpointStats {
            adds: self.watchpoint_adds.load(Ordering::Relaxed),
            removes: self.watchpoint_removes.load(Ordering::Relaxed),
        }
    }

    /// The physical watchpoint set implied by the current breakpoint table.
    fn desired_watchpoints(&self) -> BTreeSet<WatchpointKey> {
        self.manager
            .active()
            .map(|bp| WatchpointKey {
                addr: bp.addr,
                size: bp.size,
                access: bp.access,
                // Translate OS tid → UniqueThreadId: TTD's watchpoint filter
                // uses the UniqueThreadId namespace. Unknown OS tids — and,
                // defensively, ids too wide for Windows' u32 tid space —
                // degrade to "any thread" (0); the callback still applies the
                // per-thread logical filter, so such a breakpoint never fires
                // for a thread the client did not request.
                tid: bp
                    .thread_id
                    .and_then(|os_tid| u32::try_from(os_tid).ok())
                    .and_then(|os_tid| self.threads.uid_for_os_tid(os_tid))
                    .unwrap_or(0),
            })
            .collect()
    }

    /// Reconcile the persistent cursor's physical watchpoints with the logical
    /// table. Failed adds are retried next time; failed removes stay tracked
    /// for retry. Returns the first add failure so registration can surface.
    fn sync<C: WatchpointCursor + ?Sized>(&mut self, cursor: &C) -> Result<(), DebugError> {
        let desired = self.desired_watchpoints();
        let (report, first_error) = self.watchpoints.sync(
            &desired,
            |key| cursor.add_watchpoint(key),
            |key| cursor.remove_watchpoint(key),
        );
        self.watchpoint_adds
            .fetch_add(report.added, Ordering::Relaxed);
        self.watchpoint_removes
            .fetch_add(report.removed, Ordering::Relaxed);
        match first_error {
            Some(err) => Err(DebugError::BreakpointRegister {
                addr: err.key.addr,
                source: err.source,
            }),
            None => Ok(()),
        }
    }

    /// Add an execution breakpoint and register its physical watchpoint.
    pub(crate) fn set_breakpoint(
        &mut self,
        addr: u64,
        thread_id: Option<u64>,
        cursor: &(impl WatchpointCursor + ?Sized),
    ) -> Result<BreakpointId, DebugError> {
        let id = self.manager.add(
            addr,
            1,
            DataAccessMask::EXECUTE,
            BreakpointKind::Execute,
            thread_id,
        );
        match self.sync(cursor) {
            Ok(()) => Ok(id),
            Err(e) => {
                self.manager.remove(id);
                // A failed multi-add may still have applied other missing
                // keys; removing the logical entry and syncing once more
                // drops any orphan before returning the registration error.
                let _ = self.sync(cursor);
                Err(e)
            }
        }
    }

    /// Add a data watchpoint and register its physical watchpoint.
    pub(crate) fn set_data_breakpoint(
        &mut self,
        addr: u64,
        size: u64,
        kind: BreakpointKind,
        cursor: &(impl WatchpointCursor + ?Sized),
    ) -> Result<BreakpointId, DebugError> {
        let access = match kind {
            BreakpointKind::Execute => DataAccessMask::EXECUTE,
            BreakpointKind::Read => DataAccessMask::READ,
            BreakpointKind::Write => DataAccessMask::WRITE,
            BreakpointKind::Access => DataAccessMask::READ_WRITE,
        };
        let id = self.manager.add(addr, size, access, kind, None);
        match self.sync(cursor) {
            Ok(()) => Ok(id),
            Err(e) => {
                self.manager.remove(id);
                let _ = self.sync(cursor);
                Err(e)
            }
        }
    }

    /// Remove one logical breakpoint, syncing physical state if it existed.
    pub(crate) fn remove_breakpoint(
        &mut self,
        id: BreakpointId,
        cursor: &(impl WatchpointCursor + ?Sized),
    ) -> bool {
        let removed = self.manager.remove(id);
        if removed {
            let _ = self.sync(cursor);
        }
        removed
    }

    /// Address-keyed removal: clear every logical id at `(addr, size, access)`.
    pub(crate) fn remove_breakpoint_exact(
        &mut self,
        addr: u64,
        size: u64,
        access: DataAccessMask,
        cursor: &(impl WatchpointCursor + ?Sized),
    ) -> bool {
        let removed = self.manager.remove_exact(addr, size, access);
        if removed {
            let _ = self.sync(cursor);
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;
    use crate::ttd::types::TtdThreadInfo;

    #[derive(Default)]
    struct FakeCursor {
        adds: RefCell<Vec<WatchpointKey>>,
        removes: RefCell<Vec<WatchpointKey>>,
        fail_next_add: Cell<bool>,
        fail_next_remove: Cell<bool>,
    }

    impl WatchpointCursor for FakeCursor {
        fn add_watchpoint(&self, key: WatchpointKey) -> Result<(), TtdError> {
            if self.fail_next_add.replace(false) {
                return Err(TtdError::Ffi("add failed"));
            }
            self.adds.borrow_mut().push(key);
            Ok(())
        }

        fn remove_watchpoint(&self, key: WatchpointKey) -> Result<(), TtdError> {
            if self.fail_next_remove.replace(false) {
                return Err(TtdError::Ffi("remove failed"));
            }
            self.removes.borrow_mut().push(key);
            Ok(())
        }
    }

    fn state_with_threads(infos: &[TtdThreadInfo]) -> BreakpointState {
        BreakpointState::new(Arc::new(ThreadTable::from_engine_threads(infos)))
    }

    fn thread_info(unique_id: u32, os_thread_id: u32) -> TtdThreadInfo {
        TtdThreadInfo {
            UniqueId: unique_id,
            Id: os_thread_id,
            ..Default::default()
        }
    }

    #[test]
    fn duplicate_ids_collapse_to_one_physical_watchpoint() {
        let mut state = state_with_threads(&[]);
        let cursor = FakeCursor::default();

        let a = state
            .set_data_breakpoint(0x1000, 8, BreakpointKind::Write, &cursor)
            .unwrap();
        let b = state
            .set_data_breakpoint(0x1000, 8, BreakpointKind::Write, &cursor)
            .unwrap();
        assert_ne!(a, b, "logical ids stay distinct");
        assert_eq!(cursor.adds.borrow().len(), 1, "same key must add once");

        // Removing one of two equal logical keys keeps the physical entry.
        assert!(state.remove_breakpoint(a, &cursor));
        assert_eq!(cursor.removes.borrow().len(), 0);

        // Removing the last one removes it exactly once.
        assert!(state.remove_breakpoint(b, &cursor));
        assert_eq!(cursor.removes.borrow().len(), 1);
        assert_eq!(
            state.watchpoint_stats(),
            WatchpointStats {
                adds: 1,
                removes: 1
            }
        );
    }

    #[test]
    fn failed_add_rolls_back_logical_entry_and_is_retried() {
        let mut state = state_with_threads(&[]);
        let cursor = FakeCursor::default();
        cursor.fail_next_add.set(true);

        assert!(state.set_breakpoint(0x2000, None, &cursor).is_err());
        assert!(cursor.adds.borrow().is_empty());
        assert_eq!(state.watchpoint_stats().adds, 0);

        // The failed logical entry was rolled back; the next call retries
        // registration and succeeds.
        let id = state.set_breakpoint(0x2000, None, &cursor).unwrap();
        assert!(state.get(id).is_some());
        assert_eq!(cursor.adds.borrow().len(), 1);
        assert_eq!(state.watchpoint_stats().adds, 1);
    }

    #[test]
    fn failed_remove_stays_tracked_and_a_later_sync_retries_it() {
        let mut state = state_with_threads(&[]);
        let cursor = FakeCursor::default();

        let id = state
            .set_data_breakpoint(0x3000, 4, BreakpointKind::Read, &cursor)
            .unwrap();
        cursor.fail_next_remove.set(true);
        assert!(state.remove_breakpoint(id, &cursor));
        assert!(cursor.removes.borrow().is_empty());
        assert_eq!(state.watchpoint_stats().removes, 0);

        // Adding a new logical key forces another sync; the stale physical
        // key must still be tracked and now be removed successfully.
        let second = state
            .set_data_breakpoint(0x4000, 4, BreakpointKind::Read, &cursor)
            .unwrap();
        assert_eq!(
            cursor.removes.borrow().len(),
            1,
            "the stale physical key must be retried by the next sync"
        );
        assert_eq!(state.watchpoint_stats().removes, 1);

        assert!(state.remove_breakpoint(second, &cursor));
        assert_eq!(cursor.removes.borrow().len(), 2);
        assert_eq!(state.watchpoint_stats().removes, 2);
    }

    #[test]
    fn per_thread_filter_translates_os_tid_to_unique_thread_id() {
        let mut state = state_with_threads(&[thread_info(7, 0x42)]);
        let cursor = FakeCursor::default();

        state.set_breakpoint(0x5000, Some(0x42), &cursor).unwrap();
        state.set_breakpoint(0x5001, Some(0x99), &cursor).unwrap();

        let adds = cursor.adds.borrow();
        assert_eq!(adds.len(), 2);
        assert_eq!(adds[0].tid, 7, "known OS tid maps to its UniqueThreadId");
        assert_eq!(adds[1].tid, 0, "unknown tid degrades to any-thread");
    }

    /// The wire-level `z` contract: one packet clears *every* logical id at
    /// the triple (Delve re-inserts ids across restarts) and drops the
    /// deduped physical watchpoint exactly once, leaving other triples alone.
    #[test]
    fn remove_breakpoint_exact_clears_all_ids_and_the_physical_watchpoint() {
        let mut state = state_with_threads(&[]);
        let cursor = FakeCursor::default();

        let a = state.set_breakpoint(0x1000, None, &cursor).unwrap();
        let b = state.set_breakpoint(0x1000, None, &cursor).unwrap();
        // Same address, different triple: must survive the removal.
        let other = state
            .set_data_breakpoint(0x1000, 4, BreakpointKind::Write, &cursor)
            .unwrap();
        assert_eq!(cursor.adds.borrow().len(), 2, "two distinct physical keys");

        assert!(state.remove_breakpoint_exact(0x1000, 1, DataAccessMask::EXECUTE, &cursor));
        assert!(state.get(a).is_none(), "id a is gone");
        assert!(state.get(b).is_none(), "id b is gone too");
        assert!(
            state.get(other).is_some(),
            "a different triple at the same address survives"
        );
        assert_eq!(
            cursor.removes.borrow().len(),
            1,
            "the deduped physical key is removed exactly once"
        );
        assert_eq!(state.watchpoint_stats().removes, 1);

        // Nothing left at that triple: a second packet reports false.
        assert!(!state.remove_breakpoint_exact(0x1000, 1, DataAccessMask::EXECUTE, &cursor));
        assert_eq!(cursor.removes.borrow().len(), 1, "no new physical call");
    }
}
