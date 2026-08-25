use std::collections::BTreeMap;

use crate::ttd::types::DataAccessMask;

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
    /// The accesses that fire this breakpoint; for `Execute` breakpoints this
    /// is the execute bit alone.
    pub access: DataAccessMask,
    pub kind: BreakpointKind,
    /// `None` = any thread; `Some(tid)` = only stop when that OS thread hits.
    pub thread_id: Option<u64>,
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
    #[must_use]
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
        access: DataAccessMask,
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
    pub fn remove_exact(&mut self, addr: u64, size: u64, access: DataAccessMask) -> bool {
        let before = self.breakpoints.len();
        self.breakpoints
            .retain(|_, bp| !(bp.addr == addr && bp.size == size && bp.access == access));
        self.breakpoints.len() != before
    }

    #[must_use]
    pub fn get(&self, id: BreakpointId) -> Option<&Breakpoint> {
        self.breakpoints.get(&id)
    }

    pub fn active(&self) -> impl Iterator<Item = &Breakpoint> {
        self.breakpoints.values()
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
    /// The hit's access mask must overlap the breakpoint's, so an execution
    /// breakpoint and a data watchpoint at one address stay distinguishable.
    #[must_use]
    pub fn find_by_watchpoint_hit_and_thread(
        &self,
        addr: u64,
        size: u64,
        access: DataAccessMask,
        thread_id: u64,
    ) -> Option<&Breakpoint> {
        let matches_addr = |bp: &&Breakpoint| {
            // The access mask is part of the identity for every kind. In
            // particular, an execution breakpoint at `addr` must not swallow a
            // *data* hit at the same address: TTD reports the hit's real
            // access type, and an execute watchpoint only fires for Execute.
            // (Data watchpoints additionally match by byte-range intersection:
            // a hit that starts just before the watched range and crosses into
            // it must still fire.)
            if !bp.access.intersects(access) {
                return false;
            }
            if bp.kind == BreakpointKind::Execute {
                addr == bp.addr
            } else {
                let bp_end = bp.addr.saturating_add(bp.size);
                let hit_end = addr.saturating_add(size.max(1));
                addr < bp_end && bp.addr < hit_end
            }
        };
        let candidates = self.breakpoints.values();
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

    const EXEC: DataAccessMask = DataAccessMask::EXECUTE;

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

    /// The contract used by the TTD watchpoint callback: a per-thread
    /// breakpoint registered via `DebugTarget::set_breakpoint` must NOT fire
    /// on a hit from a different thread, and a global breakpoint must fire
    /// regardless of thread. Routing the callback through
    /// `find_by_watchpoint_hit` would silently match every thread.
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
        let read = DataAccessMask::READ;
        let write = DataAccessMask::WRITE;

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

    /// An execution breakpoint and a data watchpoint can share an address.
    /// A data hit there must report the data watchpoint; only an Execute hit
    /// may report the execution breakpoint.
    #[test]
    fn access_mask_distinguishes_execute_and_data_breakpoints_at_one_address() {
        let read = DataAccessMask::READ;
        let write = DataAccessMask::WRITE;
        let execute = DataAccessMask::EXECUTE;

        let mut mgr = BreakpointManager::new();
        let exec = mgr.add(0x1000, 1, execute, BreakpointKind::Execute, None);
        let data = mgr.add(0x1000, 8, read | write, BreakpointKind::Access, None);

        assert_eq!(
            mgr.find_by_watchpoint_hit_and_thread(0x1000, 8, write, 1)
                .map(|b| b.id),
            Some(data),
            "a write hit must select the data watchpoint, not the execute breakpoint"
        );
        assert_eq!(
            mgr.find_by_watchpoint_hit_and_thread(0x1000, 8, read, 1)
                .map(|b| b.id),
            Some(data),
            "a read hit must select the data watchpoint"
        );
        assert_eq!(
            mgr.find_by_watchpoint_hit_and_thread(0x1000, 1, execute, 1)
                .map(|b| b.id),
            Some(exec),
            "an execute hit must select the execution breakpoint"
        );

        // A data breakpoint with no matching access bit must not match either:
        // e.g. an execute hit when only a write watchpoint lives at the address.
        let mut only_write = BreakpointManager::new();
        let write_id = only_write.add(0x2000, 4, write, BreakpointKind::Write, None);
        assert_eq!(
            only_write
                .find_by_watchpoint_hit_and_thread(0x2000, 4, execute, 1)
                .map(|b| b.id),
            None,
            "an execute hit must not fire a write watchpoint"
        );
        assert_eq!(
            only_write
                .find_by_watchpoint_hit_and_thread(0x2000, 4, write, 1)
                .map(|b| b.id),
            Some(write_id)
        );
    }

    /// A data watchpoint owns a byte range: a hit that starts before the
    /// range but crosses into it must match, and a hit that ends before the
    /// range must not. This is the hardware-watchpoint semantic TTD's
    /// `MemoryWatchpointResult` provides (`Address` + `Size`).
    #[test]
    fn data_watchpoint_matches_overlapping_hit_ranges() {
        let read = DataAccessMask::READ;
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
            DataAccessMask::WRITE,
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
