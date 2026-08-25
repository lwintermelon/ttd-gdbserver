//! Immutable, trace-global metadata cached at open time.
//!
//! The thread catalog, module list, and exception-event list are fixed once a
//! trace is loaded, but the target layer reads them repeatedly: `monitor ttd`
//! renders them, `thread_state` validates ids against the catalog, and every
//! exception stop searches the event list for its code/address. Querying the
//! engine and allocating a fresh `Vec` for each of those reads is pure
//! overhead.
//!
//! This type owns one snapshot of that data so the rest of `target/` treats
//! it as plain Rust state.
//! Building it from a [`ReplayEngine`] is the only FFI-touching part.

use std::sync::Arc;

use crate::ttd::ReplayEngine;

use crate::ttd::types::{TtdExceptionEvent, TtdIndexStatus, TtdPosition, TtdThreadInfo};

use super::diagnostics::{TargetDiagnostics, ThreadDiagnostics};
use super::module_info::ModuleInfo;
use super::threads::ThreadTable;

/// Every immutable fact about a loaded trace that the target layer queries.
pub(crate) struct TraceMetadata {
    first_position: TtdPosition,
    last_position: TtdPosition,
    threads: Arc<ThreadTable>,
    thread_diagnostics: Vec<ThreadDiagnostics>,
    modules: Vec<ModuleInfo>,
    exceptions: Vec<TtdExceptionEvent>,
    /// The index state at open time. `monitor ttd info` shows it: memory
    /// queries are only trustworthy against a complete index.
    index_status: TtdIndexStatus,
}

impl TraceMetadata {
    /// Snapshot the engine's trace-global tables.
    pub(crate) fn from_engine(engine: &ReplayEngine) -> Self {
        let threads = engine.threads();
        let modules = engine
            .modules()
            .into_iter()
            .map(|m| ModuleInfo {
                base_addr: m.address,
                size: m.size,
                name: m.name,
            })
            .collect();
        let exceptions = engine.exception_events();
        let lifetime = engine.lifetime();
        Self::new(
            &threads,
            modules,
            exceptions,
            TtdPosition::from(lifetime.Min),
            TtdPosition::from(lifetime.Max),
        )
        .with_index_status(engine.index_state())
    }

    /// Build from already-materialized tables. Exposed for unit tests, which
    /// can exercise the exception lookup and diagnostics without a live TTD
    /// engine.
    fn new(
        threads: &[TtdThreadInfo],
        modules: Vec<ModuleInfo>,
        mut exceptions: Vec<TtdExceptionEvent>,
        first_position: TtdPosition,
        last_position: TtdPosition,
    ) -> Self {
        // `exception_at` scans in position order; the engine reports
        // events in trace order, but do not rely on that at the FFI
        // boundary.
        exceptions.sort_by_key(|e| e.position);
        let thread_diagnostics = threads
            .iter()
            .map(|t| ThreadDiagnostics {
                unique_id: t.UniqueId,
                os_thread_id: t.Id,
                active_time: (
                    TtdPosition::from(t.ActiveTime.Min),
                    TtdPosition::from(t.ActiveTime.Max),
                ),
            })
            .collect();
        Self {
            first_position,
            last_position,
            threads: Arc::new(ThreadTable::from_engine_threads(threads)),
            thread_diagnostics,
            modules,
            exceptions,
            index_status: TtdIndexStatus::Loaded,
        }
    }

    /// Attach the index status measured at open time.
    fn with_index_status(mut self, status: TtdIndexStatus) -> Self {
        self.index_status = status;
        self
    }

    /// The trace's `(first, last)` positions.
    pub(crate) fn lifetime(&self) -> (TtdPosition, TtdPosition) {
        (self.first_position, self.last_position)
    }

    /// The OS-tid → `UniqueThreadId` catalog shared with the breakpoint layer.
    pub(crate) fn threads(&self) -> &ThreadTable {
        &self.threads
    }

    /// A strong reference to the catalog for `BreakpointState`.
    pub(crate) fn thread_table(&self) -> Arc<ThreadTable> {
        Arc::clone(&self.threads)
    }

    /// The first OS tid in TTD's lifetime order, used as the initial current
    /// thread.
    pub(crate) fn first_os_tid(&self) -> Option<u64> {
        self.thread_diagnostics
            .first()
            .map(|t| u64::from(t.os_thread_id))
    }

    pub(crate) fn modules(&self) -> &[ModuleInfo] {
        &self.modules
    }

    pub(crate) fn exceptions(&self) -> &[TtdExceptionEvent] {
        &self.exceptions
    }

    /// The recorded exception event for `position`, if one is there.
    ///
    /// Bounded to the same `sequence`: a replay stops *at* the exception's
    /// recorded position, and TTD rounds to the nearest valid one, so a
    /// neighbouring `steps` value within the same sequence is the same event.
    /// Bounding matters — an unbounded "last event at or before" search would
    /// attach a much later exception's code to an unrelated stop, and a
    /// first-chance C++ `throw` recorded in between would be attributed to a
    /// subsequent access violation.
    pub(crate) fn exception_at(&self, position: TtdPosition) -> Option<TtdExceptionEvent> {
        self.exceptions
            .iter()
            .take_while(|e| e.position <= position)
            .filter(|e| e.position.sequence() == position.sequence())
            .last()
            .copied()
    }

    /// The recorded exception code at `position` (see [`Self::exception_at`]).
    pub(crate) fn exception_code_at(&self, position: TtdPosition) -> Option<u32> {
        self.exception_at(position).map(|e| e.code)
    }

    /// Render the trace-level diagnostics for `current`.
    pub(crate) fn diagnostics(&self, current: TtdPosition) -> TargetDiagnostics {
        TargetDiagnostics {
            first_position: self.first_position,
            last_position: self.last_position,
            current_position: current,
            threads: self.thread_diagnostics.clone(),
            exception_count: count_u32(self.exceptions.len()),
            module_count: count_u32(self.modules.len()),
            index_status: self.index_status,
        }
    }
}

fn count_u32(len: usize) -> u32 {
    u32::try_from(len).expect("TTD reports counts as u32, so the list length fits")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ttd::types::TtdPositionRange;

    fn pos(sequence: u64, steps: u64) -> TtdPosition {
        TtdPosition::new(sequence, steps)
    }

    fn thread(unique_id: u32, os_tid: u32, first: TtdPosition, last: TtdPosition) -> TtdThreadInfo {
        TtdThreadInfo {
            UniqueId: unique_id,
            Id: os_tid,
            Lifetime: TtdPositionRange {
                Min: first,
                Max: last,
            },
            ActiveTime: TtdPositionRange {
                Min: first,
                Max: last,
            },
        }
    }

    fn event(code: u32, addr: u64, position: TtdPosition) -> TtdExceptionEvent {
        TtdExceptionEvent {
            code,
            record_address: addr,
            position,
            ..TtdExceptionEvent::default()
        }
    }

    fn metadata() -> TraceMetadata {
        TraceMetadata::new(
            &[
                thread(2, 0x20, pos(1, 0), pos(9, 9)),
                thread(3, 0x30, pos(2, 0), pos(8, 0)),
            ],
            vec![ModuleInfo {
                base_addr: 0x1_4000_0000,
                size: 0x1000,
                name: "sample.exe".to_string(),
            }],
            vec![event(0xC000_0005, 0xdead, pos(3, 4))],
            pos(1, 0),
            pos(9, 9),
        )
    }

    #[test]
    fn diagnostics_use_the_cached_tables() {
        let d = metadata().diagnostics(pos(5, 6));
        assert_eq!(d.current_position, pos(5, 6));
        assert_eq!(d.threads.len(), 2);
        assert_eq!(d.exception_count, 1);
        assert_eq!(d.module_count, 1);
    }

    #[test]
    fn exception_at_reads_the_event_recorded_there() {
        let m = metadata();
        assert_eq!(m.exception_at(pos(3, 4)).map(|e| e.code), Some(0xC000_0005));
        // TTD rounds the landing position, so a neighbour inside the same
        // sequence is the same event.
        assert_eq!(
            m.exception_at(pos(3, 5)).map(|e| e.record_address),
            Some(0xdead)
        );
        // A press earlier is a different event (none here), and an event from
        // an earlier sequence must never be attributed to a later stop: that
        // is what made a masked C++ `throw` leak into a later access-violation
        // stop.
        assert_eq!(m.exception_at(pos(3, 3)), None);
        assert_eq!(m.exception_at(pos(4, 0)), None);
    }

    #[test]
    fn exception_at_tolerates_unsorted_engine_output() {
        let m = TraceMetadata::new(
            &[],
            Vec::new(),
            vec![
                event(1, 0x10, pos(5, 0)),
                event(2, 0x20, pos(2, 0)),
                event(3, 0x30, pos(8, 0)),
            ],
            pos(0, 0),
            pos(9, 9),
        );
        assert_eq!(m.exception_code_at(pos(5, 3)), Some(1));
        assert_eq!(m.exception_code_at(pos(5, 0)), Some(1));
        assert_eq!(m.exception_code_at(pos(3, 0)), None);
    }

    #[test]
    fn thread_catalog_and_first_tid_are_exposed() {
        let m = metadata();
        assert_eq!(m.first_os_tid(), Some(0x20));
        assert!(m.threads().contains_os_tid(0x30));
        assert_eq!(m.threads().uid_for_os_tid(0x30), Some(3));
    }
}
