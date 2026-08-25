use std::collections::BTreeMap;

pub type BreakpointId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakpointKind {
    Execute,
    Read,
    Write,
    Access, // read or write
}

#[derive(Debug, Clone)]
pub struct Breakpoint {
    pub id: BreakpointId,
    pub addr: u64,
    pub size: u64,
    pub access: u8,
    pub kind: BreakpointKind,
    /// `None` = any thread; `Some(tid)` = only stop when that OS thread hits.
    pub thread_id: Option<u64>,
    pub enabled: bool,
}

impl Breakpoint {
    pub fn access_mask(&self) -> u8 {
        self.access
    }
}

pub struct BreakpointManager {
    /// Ordered by id, so duplicate-address matching is deterministic: the
    /// lowest matching id wins, and iteration never depends on hash order.
    breakpoints: BTreeMap<BreakpointId, Breakpoint>,
    next_id: BreakpointId,
}

impl Default for BreakpointManager {
    fn default() -> Self {
        Self::new()
    }
}

impl BreakpointManager {
    pub fn new() -> Self {
        Self {
            breakpoints: BTreeMap::new(),
            next_id: 1,
        }
    }

    pub fn add(
        &mut self,
        addr: u64,
        size: u64,
        access: u8,
        kind: BreakpointKind,
        thread_id: Option<u64>,
    ) -> BreakpointId {
        let id = self.next_id;
        self.next_id += 1;
        self.breakpoints.insert(
            id,
            Breakpoint {
                id,
                addr,
                size,
                access,
                kind,
                thread_id,
                enabled: true,
            },
        );
        id
    }

    pub fn remove(&mut self, id: BreakpointId) -> bool {
        self.breakpoints.remove(&id).is_some()
    }

    /// Remove every breakpoint at this exact `(addr, size, access)` triple
    /// (any thread filter). Returns true when something was removed. This is
    /// the wire-level removal contract: RSP `z` packets identify breakpoints
    /// by address, and clients legitimately accumulate several IDs for one
    /// address across restarts.
    pub fn remove_exact(&mut self, addr: u64, size: u64, access: u8) -> bool {
        let before = self.breakpoints.len();
        self.breakpoints
            .retain(|_, bp| !(bp.addr == addr && bp.size == size && bp.access == access));
        self.breakpoints.len() != before
    }

    pub fn get(&self, id: BreakpointId) -> Option<&Breakpoint> {
        self.breakpoints.get(&id)
    }

    pub fn active(&self) -> impl Iterator<Item = &Breakpoint> {
        self.breakpoints.values().filter(|bp| bp.enabled)
    }

    /// Find the breakpoint a watchpoint hit belongs to.
    ///
    /// This is the function the TTD watchpoint callback uses, so a
    /// breakpoint with `thread_id = Some(t)` only fires on thread `t`; a
    /// breakpoint with `thread_id = None` matches any thread. Called from
    /// `TtdProcess::watchpoint_replay` with the OS thread id reported by
    /// the TTD replay engine.
    ///
    /// When several breakpoints cover the same address the match is
    /// deterministic: a breakpoint restricted to the hitting thread wins over
    /// a global one (a per-thread breakpoint is the more specific request).
    pub fn find_by_watchpoint_hit_and_thread(
        &self,
        addr: u64,
        size: u64,
        access: u8,
        thread_id: u64,
    ) -> Option<&Breakpoint> {
        let matches_addr = |bp: &&Breakpoint| {
            if bp.kind == BreakpointKind::Execute {
                addr == bp.addr
            } else {
                // A data watchpoint stops when any byte in its range is
                // touched, so match the two byte ranges by intersection
                // rather than testing only the hit start address. A hit
                // starting just before the watched range and crossing into
                // it must still fire.
                let bp_end = bp.addr.saturating_add(bp.size);
                let hit_end = addr.saturating_add(size.max(1));
                addr < bp_end && bp.addr < hit_end && (bp.access & access) != 0
            }
        };
        let candidates = self.breakpoints.values().filter(|bp| bp.enabled);
        // Two passes over the id-ordered map: the lower matching id wins.
        candidates
            .clone()
            .filter(matches_addr)
            .find(|bp| bp.thread_id == Some(thread_id))
            .or_else(|| {
                candidates
                    .filter(matches_addr)
                    .find(|bp| bp.thread_id.is_none())
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ttd::types::{TTD_Replay_DataAccessMask_Read, TTD_Replay_DataAccessMask_Write};

    /// Execute = 1<<2 in the `DataAccessMask` bitmask convention.
    const EXEC: u8 = 0x04;

    fn add_bp(mgr: &mut BreakpointManager, addr: u64, kind: BreakpointKind) -> BreakpointId {
        mgr.add(addr, 1, EXEC, kind, None)
    }

    #[test]
    fn add_and_remove() {
        let mut mgr = BreakpointManager::new();
        let id = add_bp(&mut mgr, 0x1000, BreakpointKind::Execute);
        assert!(mgr.get(id).is_some());
        assert!(mgr.remove(id));
        assert!(mgr.get(id).is_none());
    }

    #[test]
    fn active_breakpoints() {
        let mut mgr = BreakpointManager::new();
        let id1 = add_bp(&mut mgr, 0x1000, BreakpointKind::Execute);
        let _id2 = add_bp(&mut mgr, 0x2000, BreakpointKind::Read);
        assert_eq!(mgr.active().count(), 2);
        mgr.remove(id1);
        assert_eq!(mgr.active().count(), 1);
    }

    #[test]
    fn thread_specific_breakpoint() {
        let mut mgr = BreakpointManager::new();
        let id = mgr.add(0x1000, 1, EXEC, BreakpointKind::Execute, Some(5));
        // Matches thread 5
        assert!(
            mgr.find_by_watchpoint_hit_and_thread(0x1000, 1, EXEC, 5)
                .is_some()
        );
        // Doesn't match thread 3
        assert!(
            mgr.find_by_watchpoint_hit_and_thread(0x1000, 1, EXEC, 3)
                .is_none()
        );
        // Global breakpoint matches any thread
        let gid = mgr.add(0x2000, 1, EXEC, BreakpointKind::Execute, None);
        assert!(
            mgr.find_by_watchpoint_hit_and_thread(0x2000, 1, EXEC, 99)
                .is_some()
        );
        assert!(mgr.remove(id));
        assert!(mgr.remove(gid));
    }

    /// Regression test for the contract used by the TTD watchpoint callback:
    /// a per-thread breakpoint registered via `DebugTarget::set_breakpoint`
    /// must NOT fire on a hit from a different thread, and a global
    /// breakpoint must fire regardless of thread. The old code routed the
    /// callback through `find_by_watchpoint_hit`, which silently matched
    /// every thread.
    #[test]
    fn callback_path_filters_by_thread() {
        let mut mgr = BreakpointManager::new();
        let per_thread = mgr.add(0x4000, 1, EXEC, BreakpointKind::Execute, Some(7));
        let global = mgr.add(0x5000, 1, EXEC, BreakpointKind::Execute, None);

        // Per-thread breakpoint: only thread 7.
        assert_eq!(
            mgr.find_by_watchpoint_hit_and_thread(0x4000, 1, EXEC, 7)
                .map(|b| b.id),
            Some(per_thread),
        );
        for other in 0u64..=6 {
            assert!(
                mgr.find_by_watchpoint_hit_and_thread(0x4000, 1, EXEC, other)
                    .is_none(),
                "per-thread breakpoint {per_thread} must not fire on thread {other}",
            );
        }
        // Thread 0 is special: it must NOT match a per-thread breakpoint,
        // because the SDK uses 0 to mean "unknown" — the per-thread filter
        // should be honored, not silently treated as "any".
        assert!(
            mgr.find_by_watchpoint_hit_and_thread(0x4000, 1, EXEC, 0)
                .is_none(),
            "thread id 0 must not match a per-thread breakpoint",
        );

        // Global breakpoint: any thread (including 0).
        for tid in [0u64, 1, 7, 99] {
            assert_eq!(
                mgr.find_by_watchpoint_hit_and_thread(0x5000, 1, EXEC, tid)
                    .map(|b| b.id),
                Some(global),
                "global breakpoint must fire on thread {tid}",
            );
        }

        // A watchpoint hit on a different address must not match either.
        assert!(
            mgr.find_by_watchpoint_hit_and_thread(0x9000, 1, EXEC, 7)
                .is_none()
        );
    }

    /// Two breakpoints can cover the same address (Delve re-inserts ids after
    /// a restart). The match must not depend on hash order, so a
    /// per-thread breakpoint always wins over a global one at that address.
    #[test]
    fn per_thread_breakpoint_wins_over_global_at_same_address() {
        for _ in 0..32 {
            let mut mgr = BreakpointManager::new();
            let global = mgr.add(0x4000, 1, EXEC, BreakpointKind::Execute, None);
            let per_thread = mgr.add(0x4000, 1, EXEC, BreakpointKind::Execute, Some(7));

            // On thread 7 the specific breakpoint is reported...
            assert_eq!(
                mgr.find_by_watchpoint_hit_and_thread(0x4000, 1, EXEC, 7)
                    .map(|b| b.id),
                Some(per_thread),
            );
            // ...and on every other thread the global one still fires.
            for tid in [0u64, 1, 8, 999] {
                assert_eq!(
                    mgr.find_by_watchpoint_hit_and_thread(0x4000, 1, EXEC, tid)
                        .map(|b| b.id),
                    Some(global),
                );
            }
        }
    }

    /// Data watchpoints match a *range* and only when the access kinds
    /// overlap — a read watchpoint must not stop on a pure write.
    #[test]
    fn data_watchpoint_matches_range_and_access_mask() {
        let read = TTD_Replay_DataAccessMask_Read;
        let write = TTD_Replay_DataAccessMask_Write;

        let mut mgr = BreakpointManager::new();
        let id = mgr.add(0x1000, 8, read, BreakpointKind::Read, None);

        // Inside the range, read access.
        assert_eq!(
            mgr.find_by_watchpoint_hit_and_thread(0x1004, 1, read, 1)
                .map(|b| b.id),
            Some(id)
        );
        // Inside the range, but write-only access: no overlap.
        assert!(
            mgr.find_by_watchpoint_hit_and_thread(0x1004, 1, write, 1)
                .is_none()
        );
        // Just past the end of the range.
        assert!(
            mgr.find_by_watchpoint_hit_and_thread(0x1008, 1, read, 1)
                .is_none()
        );
    }

    /// A data watchpoint owns a byte range: a hit that starts before the
    /// range but crosses into it must match, and a hit that ends before the
    /// range must not. This is the hardware-watchpoint semantic TTD's
    /// `MemoryWatchpointResult` provides (`Address` + `Size`).
    #[test]
    fn data_watchpoint_matches_overlapping_hit_ranges() {
        let read = TTD_Replay_DataAccessMask_Read;
        let mut mgr = BreakpointManager::new();
        let id = mgr.add(0x1000, 8, read, BreakpointKind::Read, None);

        // Hit [0x0ffc, 0x1004) overlaps the watched [0x1000, 0x1008).
        assert_eq!(
            mgr.find_by_watchpoint_hit_and_thread(0x0ffc, 8, read, 1)
                .map(|b| b.id),
            Some(id)
        );
        // Hit [0x0ff8, 0x1000) only touches the byte before the range.
        assert!(
            mgr.find_by_watchpoint_hit_and_thread(0x0ff8, 8, read, 1)
                .is_none()
        );
        // Zero-sized hit is treated as a single-byte access at `addr`.
        assert!(
            mgr.find_by_watchpoint_hit_and_thread(0x1008, 0, read, 1)
                .is_none()
        );
        assert_eq!(
            mgr.find_by_watchpoint_hit_and_thread(0x1007, 0, read, 1)
                .map(|b| b.id),
            Some(id)
        );
    }
    /// Removal is address-keyed (see `remove_exact`): clients legitimately
    /// hold several ids for one address across restarts, and one `z` packet
    /// must clear all of them.
    #[test]
    fn remove_exact_clears_every_id_at_the_triple() {
        let mut mgr = BreakpointManager::new();
        add_bp(&mut mgr, 0x1000, BreakpointKind::Execute);
        add_bp(&mut mgr, 0x1000, BreakpointKind::Execute);
        // Same address, different access: a different triple, must survive.
        let other = mgr.add(
            0x1000,
            1,
            TTD_Replay_DataAccessMask_Write,
            BreakpointKind::Write,
            None,
        );
        assert_eq!(mgr.active().count(), 3);

        assert!(mgr.remove_exact(0x1000, 1, EXEC));
        assert_eq!(mgr.active().count(), 1, "only the other triple survives");
        assert!(mgr.get(other).is_some());

        // Nothing left at that triple: removal reports false.
        assert!(!mgr.remove_exact(0x1000, 1, EXEC));
        assert!(!mgr.remove_exact(0x9999, 1, EXEC));
    }
}
