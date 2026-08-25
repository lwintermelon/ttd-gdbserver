use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use crate::ttd::ReplayEngine;

use crate::ttd::error::TtdError;
use crate::ttd::types::{EventType, TtdIndexStatus, TtdPosition};
use crate::ttd::{InterruptSlot, ReplayCursor};

use super::breakpoint_state::BreakpointState;
use super::cursors::{CursorPair, current_tid, position_of, seek};
use super::error::DebugError;
use super::stats::TargetStats;
use super::threads::ThreadTable;
use super::trace_metadata::TraceMetadata;

mod execution;
mod target_impl;

pub(super) use execution::EXCEPTION_MASK;

// ─── TtdProcess ──────────────────────────────────────────────

/// Instruction-level debug engine backed by TTD Replay.
///
/// Implements `DebugTarget` (the target-layer API consumed by protocol
/// frontends). Owns two long-lived replay cursors — one persistent (carries
/// the client's watchpoints, serves every query) and one for stepping
/// (watchpoint-free) — both repositioned lazily only when the debug position
/// has actually moved. The cursors each own a strong reference to the replay
/// engine, so the target does not need a separate engine field. Cursor
/// creation is expensive (a step on a throwaway cursor costs ~150× a step on
/// a reused one) and the GDB layer issues several queries between every two
/// stops (`g`, `m`, thread listing), so a fresh-cursor-per-call design would
/// dominate latency.
///
/// The cursors live in `CursorPair` behind a `RefCell`: `ttd`'s navigation
/// operations take `&mut ReplayCursor` while the trait's query methods take
/// `&self`. This is race-free by construction: the protocol frontend reaches
/// the backend only through `TargetRunner`'s mutex. A replay worker holds that
/// mutex for the whole replay, while query handlers either answer "busy" or
/// take the non-blocking `try_lock`, so every target operation is exclusive at
/// runtime.
///
/// ## Module split
///
/// - `execution.rs` — the replay machinery behind the step/continue/`goto`
///   verbs (filtered continue, stop-reason resolution, the single-step path);
/// - `target_impl.rs` — the single [`crate::target::DebugTarget`] impl block
///   (Rust allows one per type+trait), grouped into Execution / Breakpoints /
///   Queries / Interrupt sections — the whole backend contract in one view.
///
/// This file keeps the struct, construction, and the shared cursor plumbing
/// every section builds on.
pub struct TtdProcess {
    /// Long-lived cursors: persistent/query (carries watchpoints) and step
    /// (watchpoint-free). Both cache their last position.
    cursors: CursorPair,
    /// Replay/query counters (see `DebugTarget::stats`).
    stats: Cell<TargetStats>,

    position: TtdPosition,
    lifetime: (TtdPosition, TtdPosition),
    /// Whether the recording ends because the process exited (TTD reports a
    /// `Process` event just past the last position). A continue that reaches
    /// the trace end then reports [`crate::target::StopReason::ProcessExit`]
    /// instead of a generic `TraceEnd`, which is what lets the GDB layer run
    /// rr's end sequence (`T09` first, `W00` on the next forward resume).
    ends_with_process_exit: bool,
    current_thread_id: Option<u64>,
    /// Immutable thread/module/exception tables plus the OS-tid → UTID
    /// catalog shared with the breakpoint layer.
    metadata: TraceMetadata,
    /// Logical breakpoints + their physical TTD watchpoint set and counters.
    breakpoints: BreakpointState,

    /// The cursor published for ^C while a blocking replay is in flight.
    /// Publishing is scoped to the replay call (see `InterruptSlot::publish`),
    /// so the slot is empty whenever the target is stopped.
    active_replay: Arc<InterruptSlot>,

    /// Bumped by every interrupt request. A blocking replay is aborted by
    /// poking [`Self::active_replay`]'s cursor; a range step instead issues
    /// one short `StepForward` per instruction, so there is no single call the
    /// SDK interrupt can abort. Its loop compares this epoch each iteration
    /// and stops on a change — an epoch rather than a flag so an interrupt
    /// from an earlier operation can never abort a later one.
    interrupt_epoch: Arc<AtomicU64>,
}

impl TtdProcess {
    /// Open a trace file and create the debug engine.
    ///
    /// # Errors
    /// [`DebugError::Ttd`] when the trace cannot be loaded, its index cannot
    /// be built, or the two long-lived cursors cannot be created.
    pub fn open(trace_path: &Path) -> Result<Self, DebugError> {
        let engine = open_engine(trace_path)?;
        // Read every trace-global table exactly once; queries and monitor
        // commands then run against plain Rust state instead of re-entering
        // the engine for immutable data.
        let metadata = TraceMetadata::from_engine(&engine);
        let lifetime = metadata.lifetime();
        let first_tid = metadata.first_os_tid();
        let threads = metadata.thread_table();

        // One persistent cursor for the whole process lifetime (it starts at
        // Position::Min), plus one dedicated to stepping. Both are
        // repositioned lazily before each use, and each owns a reference to
        // the engine, so `engine` can be dropped when this function returns.
        let cursors = CursorPair::new(&engine)?;
        let ends_with_process_exit = trace_ends_with_process_exit(&engine, lifetime);

        Ok(Self {
            cursors,
            stats: Cell::new(TargetStats::default()),
            position: lifetime.0,
            lifetime,
            ends_with_process_exit,
            current_thread_id: first_tid,
            metadata,
            breakpoints: BreakpointState::new(threads),
            active_replay: Arc::new(InterruptSlot::new()),
            interrupt_epoch: Arc::new(AtomicU64::new(0)),
        })
    }

    // ─── Shared cursor plumbing (used by every DebugTarget area) ──

    /// Access the persistent cursor without repositioning it (internal
    /// bookkeeping around replay calls).
    fn persistent<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&ReplayCursor) -> R,
    {
        f(&self.cursors.persistent().cursor())
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
        F: FnOnce(&ReplayCursor) -> R,
    {
        if self.cursors.persistent().prepare(pos) {
            self.bump(|s| s.cursor_seeks += 1);
        }
        f(&self.cursors.persistent().cursor())
    }

    /// Position the persistent cursor at the current debug position and run
    /// `f`. Every query goes through here so callers never see a stale
    /// cursor.
    fn with_cursor<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&ReplayCursor) -> R,
    {
        // Only real client reads count as queries: `goto` and
        // `watchpoint_replay` call `at_position` directly for bookkeeping, and
        // counting those would deflate the seeks-per-query health metric.
        self.bump(|s| s.queries += 1);
        self.at_position(self.position, f)
    }

    /// Narrow a client thread id to the Windows OS-tid space TTD speaks.
    ///
    /// `DebugTarget` uses `u64` ids because that is what GDB RSP carries, but
    /// TTD's `ThreadId` is a `u32` (as is the trace's thread catalog), so the
    /// conversion is where the two meet. Ids that do not fit cannot name a
    /// thread in the trace, and `None` is the honest answer — the callers
    /// below all already treat an unknown tid as "no such thread".
    fn os_tid(&self, thread_id: Option<u64>) -> Option<u32> {
        match thread_id {
            Some(id) => u32::try_from(id).ok(),
            None => self.current_thread_id.and_then(|id| u32::try_from(id).ok()),
        }
    }

    /// The OS tid whose PC a range step should inspect.
    ///
    /// A requested thread is used only when it actually appears in the trace's
    /// lifetime catalog; an unknown or too-wide id falls back to the current
    /// thread, and finally to `0` ("the cursor's current thread") when even
    /// that is unavailable. Reading the PC of a tid TTD has never seen would
    /// return 0, making the range look immediately exited.
    fn step_range_tid(&self, thread_id: Option<u64>) -> u32 {
        resolve_step_range_tid(thread_id, self.current_thread_id, self.metadata.threads())
    }

    /// Update the counters without a get/modify/set dance at each call site.
    fn bump(&self, f: impl FnOnce(&mut TargetStats)) {
        let mut stats = self.stats.get();
        f(&mut stats);
        self.stats.set(stats);
    }

    /// Adopt the persistent cursor's actual position and current thread as the
    /// debug state.
    ///
    /// TTD rounds `SetPosition` to the closest valid position, so the request
    /// and the landing spot can differ. Reading both back from the cursor
    /// keeps the cache honest — otherwise every later `with_cursor` would see
    /// request != cache and re-seek (landing at the same rounded spot again)
    /// on every single query. The thread comes along because the destination
    /// position has its own current thread: after a jump the previous one may
    /// not even exist yet, and a stale tid would make every register/memory
    /// query target the wrong thread. A 0 tid ("no current thread" — e.g. a
    /// position before any thread exists) clears the current thread instead of
    /// keeping that stale one; the frontend falls back to the active list.
    fn adopt_cursor_position(&mut self) {
        self.position = self.persistent(position_of);
        self.cursors.persistent().note_position(self.position);
        let tid = self.persistent(current_tid);
        self.current_thread_id = match tid {
            0 => None,
            tid => Some(u64::from(tid)),
        };
    }
}

/// Pick the OS tid whose PC a range step reads.
///
/// Split out from [`TtdProcess`] so the fallback policy is unit-testable
/// without a live trace: a requested id is honored only when the trace's
/// lifetime thread catalog knows it, otherwise the current thread is used,
/// and `0` means "whatever thread the cursor is currently on".
fn resolve_step_range_tid(
    thread_id: Option<u64>,
    current_thread_id: Option<u64>,
    threads: &ThreadTable,
) -> u32 {
    thread_id
        .and_then(|id| u32::try_from(id).ok())
        .filter(|os_tid| threads.contains_os_tid(*os_tid))
        .or_else(|| current_thread_id.and_then(|id| u32::try_from(id).ok()))
        .unwrap_or(0)
}

/// Whether the recording ends because the process exited, probed once at open
/// time.
///
/// TTD reports a plain trace boundary for the last recorded position, and only
/// surfaces the `Process` event once the cursor moves *past* it. A plain
/// continue always passes the lifetime end as its limit, so it stops at the
/// boundary and never sees that event; the fact has to be established here
/// instead. One step on a throwaway cursor positioned at the end is cheap (it
/// starts there, so there is nothing to replay) and leaves the persistent
/// cursors untouched.
fn trace_ends_with_process_exit(
    engine: &ReplayEngine,
    lifetime: (TtdPosition, TtdPosition),
) -> bool {
    let Ok(mut cursor) = engine.cursor() else {
        return false;
    };
    seek(&mut cursor, lifetime.1);
    cursor
        .replay_forward_steps(1)
        .is_ok_and(|r| r.stop_reason == EventType::Process)
}

/// Open a trace and make sure its memory index is usable.
fn open_engine(trace: &Path) -> Result<ReplayEngine, DebugError> {
    if !trace.exists() {
        return Err(TtdError::TraceNotFound {
            path: trace.display().to_string(),
        }
        .into());
    }
    let mut engine = ReplayEngine::open(trace).map_err(|e| {
        log::debug!("[ttd] failed to open {}: {e}", trace.display());
        TtdError::TraceLoadFailed {
            path: trace.display().to_string(),
        }
    })?;
    ensure_index(&mut engine, trace)?;
    Ok(engine)
}

/// Make sure the index next to `trace` is complete, rebuilding it when it is
/// not.
///
/// An index that reports [`TtdIndexStatus::NotPresent`] or
/// [`TtdIndexStatus::Unloadable`] is rebuilt by `build_index` itself. The
/// dangerous case is a build that was *killed* part-way: the engine can still
/// report the truncated file as `Loaded` and then answer memory queries from
/// it, which silently corrupts every later read (a replay that showed
/// `main.hits == 0` where the trace recorded 54, and goroutine listings that
/// resolved to the wrong thread). TTD writes the in-progress index to
/// `<trace>.idxt` and deletes that file only when the build completes, so a
/// leftover `.idxt` is the one reliable signal that the sibling `.idx` cannot
/// be trusted. Both are derived data, so they are removed and rebuilt.
fn ensure_index(engine: &mut ReplayEngine, trace: &Path) -> Result<(), DebugError> {
    let interrupted = partial_index_path(trace).exists();
    let status = engine.index_state();
    if !index_needs_build(status, interrupted) {
        log::debug!("[ttd] using the existing trace index ({status:?})");
        return Ok(());
    }
    if interrupted {
        log::warn!(
            "[ttd] a previous index build did not finish; rebuilding the trace index \
             (this can take a few minutes)"
        );
        remove_index_files(trace)?;
    } else {
        log::info!("[ttd] building the trace index (this can take a few minutes)");
    }
    engine
        .build_index_with_progress(|processed, total| {
            log::info!("[ttd] index build {processed}/{total} keyframes");
        })
        .map_err(|_| TtdError::IndexBuildFailed)?;
    Ok(())
}

/// Whether the index next to `trace` has to be (re)built.
///
/// Split out so the policy is unit-testable without an engine: a leftover
/// `.idxt` means the last build did not finish, which the status alone cannot
/// reveal.
fn index_needs_build(status: TtdIndexStatus, interrupted: bool) -> bool {
    interrupted || status != TtdIndexStatus::Loaded
}

/// `<trace>.idx` — the engine's keyframe table.
fn index_path(trace: &Path) -> PathBuf {
    trace.with_extension("idx")
}

/// `<trace>.idxt` — the temporary file a build in progress writes.
fn partial_index_path(trace: &Path) -> PathBuf {
    trace.with_extension("idxt")
}

/// Delete the index and any interrupted-build marker.
fn remove_index_files(trace: &Path) -> Result<(), DebugError> {
    for path in [partial_index_path(trace), index_path(trace)] {
        match std::fs::remove_file(&path) {
            Ok(()) => log::warn!("[ttd] removed {}", path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(TtdError::IndexFileAccess {
                    path: path.display().to_string(),
                    message: e.to_string(),
                }
                .into());
            }
        }
    }
    Ok(())
}

impl Drop for TtdProcess {
    fn drop(&mut self) {
        // Clear the interrupt slot before drop glue runs: the `Arc<dyn Fn()>`
        // handle may outlive this process, and a non-null slot would make it
        // poke a cursor that no longer exists. This ordering is guaranteed
        // under the crate's calling convention — the handle is only fired by
        // `TargetRunner`/`session` teardown on the protocol thread, which
        // never runs concurrently with dropping the backend (teardown hands
        // the backend back first). It is NOT safe against a fire from another
        // thread racing this drop; do not add such a caller without a
        // happens-before that joins the firer first.
        // Cursors own the engine themselves, so no ordering is needed for it.
        self.active_replay.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ttd::types::{TtdPositionRange, TtdThreadInfo};

    fn pos(sequence: u64, steps: u64) -> TtdPosition {
        TtdPosition::new(sequence, steps)
    }

    fn table(ids: &[u32]) -> ThreadTable {
        let threads: Vec<TtdThreadInfo> = ids
            .iter()
            .enumerate()
            .map(|(i, os_tid)| TtdThreadInfo {
                UniqueId: u32::try_from(i + 1).expect("test thread index"),
                Id: *os_tid,
                ..Default::default()
            })
            .collect();
        ThreadTable::from_engine_threads(&threads)
    }

    #[test]
    fn range_step_uses_a_known_requested_thread() {
        let threads = table(&[0x10, 0x20]);
        assert_eq!(
            resolve_step_range_tid(Some(0x20), Some(0x10), &threads),
            0x20
        );
    }

    #[test]
    fn range_step_falls_back_from_unknown_or_unrepresentable_threads() {
        let threads = table(&[0x10]);
        assert_eq!(
            resolve_step_range_tid(Some(0x99), Some(0x10), &threads),
            0x10,
            "an id absent from the trace must not be queried"
        );
        assert_eq!(
            resolve_step_range_tid(Some(u64::from(u32::MAX) + 1), Some(0x10), &threads),
            0x10,
            "an id outside TTD's u32 space must fall back"
        );
        assert_eq!(
            resolve_step_range_tid(None, Some(0x10), &threads),
            0x10,
            "no requested id means the current thread"
        );
        assert_eq!(
            resolve_step_range_tid(Some(0x99), None, &threads),
            0,
            "no usable thread at all leaves it to the cursor"
        );
    }

    #[test]
    fn index_is_rebuilt_only_when_unusable() {
        assert!(!index_needs_build(TtdIndexStatus::Loaded, false));
        assert!(index_needs_build(TtdIndexStatus::NotPresent, false));
        assert!(index_needs_build(TtdIndexStatus::Unloadable, false));
        assert!(index_needs_build(TtdIndexStatus::Unknown, false));
    }

    /// A killed build leaves `.idxt` behind while the `.idx` may still report
    /// `Loaded`; the leftover is the only reliable signal.
    #[test]
    fn interrupted_build_forces_rebuild_even_when_loaded() {
        assert!(index_needs_build(TtdIndexStatus::Loaded, true));
    }

    #[test]
    fn index_paths_are_siblings_of_the_trace() {
        let trace = Path::new("C:/traces/app.run");
        assert_eq!(index_path(trace), Path::new("C:/traces/app.idx"));
        assert_eq!(partial_index_path(trace), Path::new("C:/traces/app.idxt"));
    }

    #[test]
    fn lifetime_range_orders_endpoints() {
        let range = TtdPositionRange {
            Min: pos(1, 0),
            Max: pos(2, 0),
        };
        assert!(range.Min < range.Max);
    }
}
