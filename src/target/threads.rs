//! Immutable thread catalog for a loaded trace.
//!
//! TTD exposes two thread-id namespaces:
//!
//! - OS thread ids (`ThreadId`) — what GDB RSP and `DebugTarget` speak.
//! - TTD `UniqueThreadId` — what watchpoint thread filters speak.
//!
//! The trace's thread table is fixed once the trace is loaded, so this map is
//! built once and shared (`Arc`) between the query layer (`TtdProcess`, which
//! validates requested OS tids) and the breakpoint layer (which translates
//! them for `AddMemoryWatchpoint`).

use std::collections::HashMap;

use crate::ttd::types::TtdThreadInfo;

/// OS tid → TTD UniqueThreadId, built from the trace's lifetime thread list.
#[derive(Debug, Default)]
pub(crate) struct ThreadTable {
    os_to_uid: HashMap<u32, u32>,
}

impl ThreadTable {
    pub(crate) fn from_engine_threads(threads: &[TtdThreadInfo]) -> Self {
        let mut os_to_uid = HashMap::with_capacity(threads.len());
        for t in threads {
            os_to_uid.insert(t.os_thread_id, t.unique_id);
        }
        Self { os_to_uid }
    }

    /// Whether `os_tid` ever appears in the trace.
    pub(crate) fn contains_os_tid(&self, os_tid: u32) -> bool {
        self.os_to_uid.contains_key(&os_tid)
    }

    /// TTD `UniqueThreadId` for `os_tid`, if the thread exists in the trace.
    pub(crate) fn uid_for_os_tid(&self, os_tid: u32) -> Option<u32> {
        self.os_to_uid.get(&os_tid).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(unique_id: u32, os_thread_id: u32) -> TtdThreadInfo {
        TtdThreadInfo {
            unique_id,
            os_thread_id,
            ..Default::default()
        }
    }

    #[test]
    fn maps_os_tid_to_unique_thread_id() {
        let table = ThreadTable::from_engine_threads(&[info(2, 0x3784), info(3, 0x42)]);
        assert!(table.contains_os_tid(0x3784));
        assert_eq!(table.uid_for_os_tid(0x3784), Some(2));
        assert_eq!(table.uid_for_os_tid(0x42), Some(3));
        assert!(!table.contains_os_tid(0xdead));
        assert_eq!(table.uid_for_os_tid(0xdead), None);
    }

    /// A recycled OS tid keeps the last lifetime mapping; this is a known
    /// ambiguity of the Windows OS-tid namespace, contained in one place so
    /// future policy changes only affect this catalog.
    #[test]
    fn duplicate_os_tid_is_deterministic_last_wins() {
        let table = ThreadTable::from_engine_threads(&[info(2, 0x100), info(5, 0x100)]);
        assert_eq!(table.uid_for_os_tid(0x100), Some(5));
    }
}
