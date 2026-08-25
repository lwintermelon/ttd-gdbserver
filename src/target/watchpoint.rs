//! Physical TTD watchpoint registry and incremental synchronization.
//!
//! Client breakpoints are logical ids ([`super::breakpoint::BreakpointManager`]),
//! while TTD watches a physical `(addr, size, access, UniqueThreadId)` tuple.
//! Several logical ids can collapse onto one physical watchpoint: Delve
//! re-inserts its breakpoint ids after a restart while the stub still holds the
//! old ids, and one address may legitimately be registered more than once.
//!
//! This module owns the transition between those two sets: it turns logical
//! additions/removals into the minimal ordered sequence of physical
//! `AddMemoryWatchpoint`/`RemoveMemoryWatchpoint` calls. Keeping the state
//! machine independent of the FFI makes the deduplication contract directly
//! unit-testable and guarantees deterministic call order (the old
//! `HashSet`-based implementation drained in hash order).

use std::collections::BTreeSet;

use crate::ttd::TtdError;

/// Physical TTD watchpoint identity: the arguments of
/// `AddMemoryWatchpoint`/`RemoveMemoryWatchpoint`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct WatchpointKey {
    pub addr: u64,
    pub size: u64,
    pub access: u8,
    /// UniqueThreadId (TTD's filter namespace); 0 = any thread.
    pub tid: u32,
}

/// The first physical-add failure encountered while syncing.
#[derive(Debug)]
pub(crate) struct WatchpointSyncError {
    pub key: WatchpointKey,
    pub source: TtdError,
}

/// Successful physical operations performed by one [`WatchpointRegistry::sync`]
/// call. Used to update the target's observability counters without locking an
/// extra cell inside the FFI callbacks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WatchpointSyncReport {
    pub added: u64,
    pub removed: u64,
    pub add_failures: u64,
    pub remove_failures: u64,
}

/// The set of physical watchpoints currently registered on one cursor.
#[derive(Debug, Default)]
pub(crate) struct WatchpointRegistry {
    applied: BTreeSet<WatchpointKey>,
}

impl WatchpointRegistry {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Bring the physical set in line with `desired`.
    ///
    /// Removals happen first (release capacity before adding), both
    /// differences are computed on ordered sets and applied in ascending key
    /// order, and each successful call updates `applied` immediately. A failed
    /// add is not recorded, so the next sync retries it. A failed remove keeps
    /// the key recorded, so the next sync retries that too — this is what makes
    /// the operation convergent even when a single TTD call fails.
    ///
    /// Returns the first add failure separately because the caller must
    /// surface it as `DebugError::BreakpointRegister` (the RSP `E16` path).
    /// Remove failures cannot be reported on the address-keyed RSP `z` path,
    /// so they are counted/logged and retried by the next sync.
    pub(crate) fn sync<Add, Remove>(
        &mut self,
        desired: &BTreeSet<WatchpointKey>,
        mut add: Add,
        mut remove: Remove,
    ) -> (WatchpointSyncReport, Option<WatchpointSyncError>)
    where
        Add: FnMut(WatchpointKey) -> Result<(), TtdError>,
        Remove: FnMut(WatchpointKey) -> Result<(), TtdError>,
    {
        let stale: Vec<_> = self.applied.difference(desired).copied().collect();
        let missing: Vec<_> = desired.difference(&self.applied).copied().collect();

        let mut report = WatchpointSyncReport::default();
        let mut first_add_error = None;

        for key in stale {
            match remove(key) {
                Ok(()) => {
                    self.applied.remove(&key);
                    report.removed += 1;
                }
                Err(source) => {
                    report.remove_failures += 1;
                    log::warn!(
                        "remove_watchpoint {key:?} failed: {source}; keeping it applied for retry"
                    );
                }
            }
        }

        for key in missing {
            match add(key) {
                Ok(()) => {
                    self.applied.insert(key);
                    report.added += 1;
                }
                Err(source) => {
                    report.add_failures += 1;
                    log::warn!("add_watchpoint {key:?} failed: {source}");
                    if first_add_error.is_none() {
                        first_add_error = Some(WatchpointSyncError { key, source });
                    }
                }
            }
        }

        (report, first_add_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(addr: u64, tid: u32) -> WatchpointKey {
        WatchpointKey {
            addr,
            size: 1,
            access: 4,
            tid,
        }
    }

    fn desired(keys: &[WatchpointKey]) -> BTreeSet<WatchpointKey> {
        keys.iter().copied().collect()
    }

    #[test]
    fn duplicate_physical_key_is_added_once_and_removed_once() {
        let k = key(0x1000, 0);
        let mut registry = WatchpointRegistry::new();
        let mut adds = Vec::new();
        let mut removes = Vec::new();

        // Two logical breakpoints at the same physical key collapse to one
        // desired-key; only one AddMemoryWatchpoint may be issued.
        let (report, err) = registry.sync(
            &desired(&[k, k]),
            |k| {
                adds.push(k);
                Ok(())
            },
            |k| {
                removes.push(k);
                Ok(())
            },
        );
        assert!(err.is_none());
        assert_eq!(adds, vec![k]);
        assert_eq!(removes, Vec::<WatchpointKey>::new());
        assert_eq!(report.added, 1);
        assert_eq!(report.removed, 0);

        // Removing one of the two logical ids still leaves the desired key:
        // the physical watchpoint must survive.
        let (report, _) = registry.sync(
            &desired(&[k]),
            |k| {
                adds.push(k);
                Ok(())
            },
            |k| {
                removes.push(k);
                Ok(())
            },
        );
        assert_eq!(adds, vec![k], "already-applied key must not be re-added");
        assert_eq!(removes, Vec::<WatchpointKey>::new());
        assert_eq!(report, WatchpointSyncReport::default());

        // Removing both logical ids drops the desired key: exactly one
        // physical remove.
        let (report, _) = registry.sync(
            &desired(&[]),
            |k| {
                adds.push(k);
                Ok(())
            },
            |k| {
                removes.push(k);
                Ok(())
            },
        );
        assert_eq!(adds, vec![k]);
        assert_eq!(removes, vec![k]);
        assert_eq!(report.removed, 1);
    }

    #[test]
    fn failed_add_is_not_recorded_and_is_retried_next_sync() {
        let k = key(0x2000, 0);
        let mut registry = WatchpointRegistry::new();
        let mut calls = 0;

        let (report, err) = registry.sync(
            &desired(&[k]),
            |_| {
                calls += 1;
                Err(TtdError::Ffi("add failed"))
            },
            |_| Ok(()),
        );
        assert_eq!(calls, 1);
        assert_eq!(report.add_failures, 1);
        assert_eq!(report.added, 0);
        assert_eq!(err.as_ref().map(|e| e.key), Some(k));

        // The failed key was not recorded: the next sync retries the add.
        let (report, err) = registry.sync(
            &desired(&[k]),
            |_| {
                calls += 1;
                Ok(())
            },
            |_| Ok(()),
        );
        assert_eq!(calls, 2);
        assert_eq!(report.added, 1);
        assert!(err.is_none());

        // Now it is recorded: no third call.
        let (report, _) = registry.sync(
            &desired(&[k]),
            |_| {
                calls += 1;
                Ok(())
            },
            |_| Ok(()),
        );
        assert_eq!(calls, 2);
        assert_eq!(report, WatchpointSyncReport::default());
    }

    #[test]
    fn failed_remove_keeps_key_applied_and_is_retried() {
        let k = key(0x3000, 0);
        let mut registry = WatchpointRegistry::new();

        let _ = registry.sync(&desired(&[k]), |_| Ok(()), |_| Ok(()));
        let (report, err) = registry.sync(
            &desired(&[]),
            |_| panic!("no add expected"),
            |_| Err(TtdError::Ffi("remove failed")),
        );
        assert_eq!(report.remove_failures, 1);
        assert_eq!(report.removed, 0);
        assert!(err.is_none());

        // Still applied: a later sync retries the remove.
        let mut removed = false;
        let (report, _) = registry.sync(
            &desired(&[]),
            |_| panic!("no add expected"),
            |_| {
                removed = true;
                Ok(())
            },
        );
        assert!(removed);
        assert_eq!(report.removed, 1);
    }

    #[test]
    fn sync_order_is_deterministic_by_key() {
        let low = key(0x1000, 0);
        let high = key(0x2000, 0);
        let mid = key(0x1500, 0);
        let mut registry = WatchpointRegistry::new();
        let mut adds = Vec::new();

        let _ = registry.sync(
            &desired(&[high, low, mid]),
            |k| {
                adds.push(k.addr);
                Ok(())
            },
            |_| Ok(()),
        );
        assert_eq!(adds, vec![0x1000, 0x1500, 0x2000]);

        let mut removes = Vec::new();
        let _ = registry.sync(
            &desired(&[]),
            |_| panic!("no add expected"),
            |k| {
                removes.push(k.addr);
                Ok(())
            },
        );
        assert_eq!(removes, vec![0x1000, 0x1500, 0x2000]);
    }

    /// Thread-filtered keys are distinct physical watchpoints even when the
    /// address/size/access are identical.
    #[test]
    fn different_thread_filter_is_a_distinct_key() {
        let global = key(0x4000, 0);
        let per_thread = key(0x4000, 7);
        let mut registry = WatchpointRegistry::new();
        let mut adds = Vec::new();

        let _ = registry.sync(
            &desired(&[global, per_thread]),
            |k| {
                adds.push(k);
                Ok(())
            },
            |_| Ok(()),
        );
        assert_eq!(adds.len(), 2, "different tid filters must not dedupe");
        assert!(adds.contains(&global));
        assert!(adds.contains(&per_thread));
    }
}
