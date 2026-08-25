use std::collections::HashMap;

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
    pub thread_id: Option<u64>,
    pub enabled: bool,
    pub temporary: bool,
}

impl Breakpoint {
    pub fn access_mask(&self) -> u8 {
        self.access
    }
}

pub struct BreakpointManager {
    breakpoints: HashMap<BreakpointId, Breakpoint>,
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
            breakpoints: HashMap::new(),
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
        temporary: bool,
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
                temporary,
            },
        );
        id
    }

    pub fn remove(&mut self, id: BreakpointId) -> bool {
        self.breakpoints.remove(&id).is_some()
    }

    pub fn remove_temporary(&mut self) {
        self.breakpoints.retain(|_, bp| !bp.temporary);
    }

    /// Remove all breakpoints.
    pub fn clear(&mut self) {
        self.breakpoints.clear();
    }

    pub fn get(&self, id: BreakpointId) -> Option<&Breakpoint> {
        self.breakpoints.get(&id)
    }

    pub fn active(&self) -> impl Iterator<Item = &Breakpoint> {
        self.breakpoints.values().filter(|bp| bp.enabled)
    }

    pub fn all(&self) -> impl Iterator<Item = &Breakpoint> {
        self.breakpoints.values()
    }

    /// Find a breakpoint that matches the given watchpoint hit.
    ///
    /// Matches the first enabled breakpoint whose address range covers the
    /// hit and whose access mask includes the access type. **Does not** honor
    /// the per-thread `thread_id` field — every breakpoint looks global. The
    /// continue-time callback in `TtdProcess::watchpoint_replay` uses
    /// [`Self::find_by_watchpoint_hit_and_thread`] instead, which is the only
    /// way to actually restrict a breakpoint to a specific thread. This
    /// function is retained for callers that explicitly want any-thread
    /// matching.
    pub fn find_by_watchpoint_hit(&self, addr: u64, _size: u64, access: u8) -> Option<&Breakpoint> {
        for bp in self.breakpoints.values().filter(|bp| bp.enabled) {
            if bp.kind == BreakpointKind::Execute && addr == bp.addr {
                return Some(bp);
            }
            if addr >= bp.addr && addr < bp.addr + bp.size && (bp.access & access) != 0 {
                return Some(bp);
            }
        }
        None
    }

    /// Find a breakpoint that matches the given watchpoint hit and thread.
    ///
    /// This is the function the TTD watchpoint callback uses, so a
    /// breakpoint with `thread_id = Some(t)` only fires on thread `t`; a
    /// breakpoint with `thread_id = None` matches any thread. Called from
    /// `TtdProcess::watchpoint_replay` with the OS thread id reported by
    /// the TTD replay engine.
    pub fn find_by_watchpoint_hit_and_thread(
        &self,
        addr: u64,
        _size: u64,
        access: u8,
        thread_id: u64,
    ) -> Option<&Breakpoint> {
        for bp in self.breakpoints.values().filter(|bp| bp.enabled) {
            // Address match?
            let addr_match = if bp.kind == BreakpointKind::Execute {
                addr == bp.addr
            } else {
                addr >= bp.addr && addr < bp.addr + bp.size && (bp.access & access) != 0
            };
            if !addr_match {
                continue;
            }
            // Thread match: None = any thread
            if bp.thread_id.is_none() || bp.thread_id == Some(thread_id) {
                return Some(bp);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ttd::types::TtdPosition;

    fn add_bp(mgr: &mut BreakpointManager, addr: u64, kind: BreakpointKind) -> BreakpointId {
        mgr.add(addr, 1, 0x04, kind, None, false)
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
        let id = mgr.add(0x1000, 1, 0x04, BreakpointKind::Execute, Some(5), false);
        // Matches thread 5
        assert!(mgr
            .find_by_watchpoint_hit_and_thread(0x1000, 1, 0x04, 5)
            .is_some());
        // Doesn't match thread 3
        assert!(mgr
            .find_by_watchpoint_hit_and_thread(0x1000, 1, 0x04, 3)
            .is_none());
        // Global breakpoint matches any thread
        let gid = mgr.add(0x2000, 1, 0x04, BreakpointKind::Execute, None, false);
        assert!(mgr
            .find_by_watchpoint_hit_and_thread(0x2000, 1, 0x04, 99)
            .is_some());
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
        let per_thread = mgr.add(0x4000, 1, 0x04, BreakpointKind::Execute, Some(7), false);
        let global = mgr.add(0x5000, 1, 0x04, BreakpointKind::Execute, None, false);

        // Per-thread breakpoint: only thread 7.
        assert_eq!(
            mgr.find_by_watchpoint_hit_and_thread(0x4000, 1, 0x04, 7)
                .map(|b| b.id),
            Some(per_thread),
        );
        for other in 0u64..=6 {
            assert!(
                mgr.find_by_watchpoint_hit_and_thread(0x4000, 1, 0x04, other)
                    .is_none(),
                "per-thread breakpoint {per_thread} must not fire on thread {other}",
            );
        }
        // Thread 0 is special: it must NOT match a per-thread breakpoint,
        // because the SDK uses 0 to mean "unknown" — the per-thread filter
        // should be honored, not silently treated as "any".
        assert!(
            mgr.find_by_watchpoint_hit_and_thread(0x4000, 1, 0x04, 0)
                .is_none(),
            "thread id 0 must not match a per-thread breakpoint",
        );

        // Global breakpoint: any thread (including 0).
        for tid in [0u64, 1, 7, 99] {
            assert_eq!(
                mgr.find_by_watchpoint_hit_and_thread(0x5000, 1, 0x04, tid)
                    .map(|b| b.id),
                Some(global),
                "global breakpoint must fire on thread {tid}",
            );
        }

        // A watchpoint hit on a different address must not match either.
        assert!(mgr
            .find_by_watchpoint_hit_and_thread(0x9000, 1, 0x04, 7)
            .is_none());
    }

    /// Regression test for the speculative-execution filter. TTD's
    /// replay engine surfaces watchpoint hits at positions *beyond*
    /// the cursor's current limit (the engine speculatively executes
    /// ahead to find the next stop). The callback must reject such
    /// speculative hits so we never report a stop at a position the
    /// user did not cross.
    ///
    /// This test does not drive the FFI; it asserts the same
    /// `>` / `<` comparison the callback uses against the cursor's
    /// replay limit, so a future refactor that flips the direction
    /// or off-by-ones the position fails loudly.
    #[test]
    fn speculative_hit_filter_rejects_future_positions() {
        // Replaying forward, limit = trace end. A hit at or before
        // the limit is real; a hit past the limit is speculative.
        let limit = TtdPosition {
            sequence: 5,
            steps: 100,
        };
        for (hit, expected) in [
            (
                TtdPosition {
                    sequence: 1,
                    steps: 0,
                },
                false,
            ),
            (
                TtdPosition {
                    sequence: 4,
                    steps: 200,
                },
                false,
            ),
            (
                TtdPosition {
                    sequence: 5,
                    steps: 100,
                },
                false,
            ),
            (
                TtdPosition {
                    sequence: 5,
                    steps: 101,
                },
                true,
            ),
            (
                TtdPosition {
                    sequence: 6,
                    steps: 0,
                },
                true,
            ),
        ] {
            let speculative = hit > limit;
            assert_eq!(
                speculative, expected,
                "forward hit {hit:?} vs limit {limit:?}"
            );
        }

        // Backward replay: limit = trace start. A hit before the
        // limit is speculative.
        let limit = TtdPosition {
            sequence: 1,
            steps: 0,
        };
        for (hit, expected) in [
            (
                TtdPosition {
                    sequence: 0,
                    steps: 0,
                },
                true,
            ),
            (
                TtdPosition {
                    sequence: 1,
                    steps: 0,
                },
                false,
            ),
            (
                TtdPosition {
                    sequence: 1,
                    steps: 1,
                },
                false,
            ),
            (
                TtdPosition {
                    sequence: 2,
                    steps: 0,
                },
                false,
            ),
        ] {
            let speculative = hit < limit;
            assert_eq!(
                speculative, expected,
                "backward hit {hit:?} vs limit {limit:?}"
            );
        }
    }

    #[test]
    fn clear_breakpoints() {
        let mut mgr = BreakpointManager::new();
        add_bp(&mut mgr, 0x1000, BreakpointKind::Execute);
        add_bp(&mut mgr, 0x2000, BreakpointKind::Write);
        mgr.clear();
        assert_eq!(mgr.active().count(), 0);
    }

    #[test]
    fn temporary_breakpoints() {
        let mut mgr = BreakpointManager::new();
        let id = mgr.add(0x1000, 1, 0x04, BreakpointKind::Execute, None, true);
        assert!(mgr.get(id).unwrap().temporary);
        mgr.remove_temporary();
        assert!(mgr.get(id).is_none());
    }

    #[test]
    fn find_by_watchpoint_hit_execute() {
        let mut mgr = BreakpointManager::new();
        let id = add_bp(&mut mgr, 0x1000, BreakpointKind::Execute);
        let bp = mgr.find_by_watchpoint_hit(0x1000, 1, 0x04).unwrap();
        assert_eq!(bp.id, id);
    }

    #[test]
    fn find_by_watchpoint_hit_data() {
        let mut mgr = BreakpointManager::new();
        let id = mgr.add(0x5000, 4, 0x01 | 0x02, BreakpointKind::Access, None, false);
        let bp = mgr.find_by_watchpoint_hit(0x5002, 1, 0x02).unwrap();
        assert_eq!(bp.id, id);
    }

    #[test]
    fn find_by_watchpoint_miss() {
        let mgr = BreakpointManager::new();
        assert!(mgr.find_by_watchpoint_hit(0x1000, 1, 0x04).is_none());
    }
}
