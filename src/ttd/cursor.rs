//! A replay cursor: the position and thread context every query resolves
//! against.

use std::sync::Arc;

use cxx::UniquePtr;

use crate::ttd::callback::{call_guarded, callback_panic_guard};
use crate::ttd::engine::{Architecture, EngineInner};
use crate::ttd::error::{Result, TtdError};
use crate::ttd::ffi;
use crate::ttd::types::{
    DataAccessMask, DataAccessType, EventMask, ExceptionMask, TtdMemRange, TtdPosition,
    TtdReplayResult, TtdThreadInfo, TtdX64Regs, XmmSaveArea, decode_amd64_context,
};

/// A watchpoint callback: `(address, size, access mask, OS thread id)`,
/// returning whether replay should stop there.
pub(crate) type WatchpointFn<'a> = dyn FnMut(u64, u64, DataAccessMask, u32) -> bool + Send + 'a;

pub use crate::ttd::ffi::ActiveThread;

/// A handle to an SDK cursor.
///
/// A cursor owns a reference to its engine, so it can outlive the
/// [`ReplayEngine`] value it was created from. It is `Send` (a debugger worker
/// owns it) but deliberately not `Sync`.
///
/// [`ReplayEngine`]: crate::ttd::ReplayEngine
pub struct ReplayCursor {
    cursor: UniquePtr<ffi::Cursor>,
    /// Keeps the engine alive: the SDK requires it to outlive its cursors.
    engine: Arc<EngineInner>,
    /// Heap slot for an installed borrow-based callback. Boxed so the address
    /// the SDK holds stays valid for the whole replay.
    watchpoint: Option<Box<WatchpointSlot>>,
}

/// A callback slot: the SDK only carries a thin context, so the (fat) closure
/// pointer lives in a heap slot whose address is what the SDK sees.
struct WatchpointSlot(*mut WatchpointFn<'static>);

// SAFETY: the handle is moved between threads, never shared; the SDK cursor is
// only ever driven by whichever thread currently owns it.
unsafe impl Send for ReplayCursor {}

// Every handle unwrap here is infallible: `new` rejects an invalid cursor and
// the handle is owned for the cursor's lifetime. The public API therefore does
// not panic under normal use.
#[allow(clippy::missing_panics_doc)]
impl ReplayCursor {
    pub(crate) fn new(engine: Arc<EngineInner>) -> Result<Self> {
        let mut cursor = ffi::NewCursor(engine.engine());
        if cursor.is_null() || !cursor.as_ref().unwrap().IsValid() {
            return Err(TtdError::CursorCreationFailed);
        }

        // New cursors start at the beginning of the trace.
        let start = engine.engine().Lifetime().Min;
        cursor.as_mut().unwrap().SetPosition(ffi::Position {
            Sequence: start.Sequence,
            Steps: start.Steps,
        });
        Ok(Self {
            cursor,
            engine,
            watchpoint: None,
        })
    }

    /// The cursor's position.
    #[must_use]
    pub fn position(&self) -> TtdPosition {
        self.cursor.as_ref().unwrap().GetPosition()
    }

    pub fn set_position(&mut self, position: TtdPosition) {
        self.cursor.as_mut().unwrap().SetPosition(ffi::Position {
            Sequence: position.Sequence,
            Steps: position.Steps,
        });
    }

    /// Replay forward, stopping at `limit` or on a subscribed event.
    ///
    /// # Errors
    /// [`TtdError::Ffi`] when the SDK rejects the replay.
    pub fn replay_forward(&mut self, limit: TtdPosition) -> Result<TtdReplayResult> {
        self.replay(limit, STEP_COUNT_MAX, true)
    }

    /// Replay backward, stopping at `limit` or on a subscribed event.
    ///
    /// # Errors
    /// [`TtdError::Ffi`] when the SDK rejects the replay.
    pub fn replay_backward(&mut self, limit: TtdPosition) -> Result<TtdReplayResult> {
        self.replay(limit, STEP_COUNT_MAX, false)
    }

    /// Replay at most `steps` steps forward.
    ///
    /// # Errors
    /// [`TtdError::Ffi`] when the SDK rejects the replay.
    pub fn replay_forward_steps(&mut self, steps: u64) -> Result<TtdReplayResult> {
        self.replay(max_position(), steps, true)
    }

    /// Replay at most `steps` steps backward.
    ///
    /// # Errors
    /// [`TtdError::Ffi`] when the SDK rejects the replay.
    pub fn replay_backward_steps(&mut self, steps: u64) -> Result<TtdReplayResult> {
        self.replay(min_position(), steps, false)
    }

    fn replay(&mut self, limit: TtdPosition, steps: u64, forward: bool) -> Result<TtdReplayResult> {
        let _guard = callback_panic_guard();
        let limit = ffi::Position {
            Sequence: limit.Sequence,
            Steps: limit.Steps,
        };
        let cursor = self.cursor.as_mut().unwrap();
        let raw = if forward {
            cursor.ReplayForward(limit, steps)
        } else {
            cursor.ReplayBackward(limit, steps)
        };
        if raw.stop_reason == u8::MAX {
            return Err(TtdError::Ffi("replay"));
        }
        Ok(raw.into())
    }

    /// Read up to `size` bytes at `address`.
    ///
    /// # Errors
    /// [`TtdError::Ffi`] when the read fails outright.
    pub fn read_memory(&self, address: u64, size: usize) -> Result<Vec<u8>> {
        let mut buffer = vec![0; size];
        let read = self
            .cursor
            .as_ref()
            .unwrap()
            .QueryMemory(address, &mut buffer);
        buffer.truncate(read);
        Ok(buffer)
    }

    /// Read `size` bytes at `address` together with the runs they came from.
    ///
    /// # Errors
    /// [`TtdError::Ffi`] when the read fails outright.
    pub fn read_memory_ranges(
        &self,
        address: u64,
        size: usize,
        max_ranges: usize,
    ) -> Result<(Vec<u8>, Vec<TtdMemRange>)> {
        let mut buffer = vec![0; size];
        let count = max_ranges.max(1);
        let mut ranges: Vec<ffi::MemoryRange> = (0..count)
            .map(|_| ffi::MemoryRange {
                Address: 0,
                size: 0,
                Sequence: 0,
            })
            .collect();
        let result =
            self.cursor
                .as_ref()
                .unwrap()
                .QueryMemoryWithRanges(address, &mut buffer, &mut ranges);
        buffer.truncate(usize::try_from(result.read).unwrap_or(0));
        ranges.truncate(usize::try_from(result.range_count).unwrap_or(0));
        Ok((buffer, ranges))
    }

    /// The program counter of the cursor's current thread.
    #[must_use]
    pub fn pc(&self) -> u64 {
        self.cursor.as_ref().unwrap().ProgramCounter()
    }

    /// The stack pointer of the cursor's current thread.
    #[must_use]
    pub fn sp(&self) -> u64 {
        self.cursor.as_ref().unwrap().StackPointer()
    }

    /// The cursor's current thread.
    #[must_use]
    pub fn thread_info(&self) -> TtdThreadInfo {
        self.cursor.as_ref().unwrap().GetThreadInfo()
    }

    /// TEB (GS base) of `thread`; `0` means the cursor's current thread.
    #[must_use]
    pub fn teb_address_of(&self, thread: u32) -> u64 {
        self.cursor.as_ref().unwrap().TebAddress(thread)
    }

    /// The AMD64 register context of `thread`; `0` means the cursor's thread.
    ///
    /// # Errors
    /// [`TtdError::UnsupportedEvent`] when the trace is not an x64 recording.
    pub fn amd64_context(&self, thread: u32) -> Result<TtdX64Regs> {
        if self.engine.architecture() != Architecture::X64 {
            return Err(TtdError::UnsupportedArchitecture);
        }
        let bytes = self.cursor.as_ref().unwrap().GetAmd64Context(thread);
        decode_amd64_context(&bytes).ok_or(TtdError::Ffi("register context"))
    }

    /// The extended (AVX) register context of `thread`; `0` means the cursor's
    /// thread.
    ///
    /// # Errors
    /// [`TtdError::UnsupportedEvent`] when the trace is not an x64 recording.
    pub fn extended_context(&self, thread: u32) -> Result<XmmSaveArea> {
        self.amd64_context(thread).map(|regs| regs.flt_save())
    }

    /// Number of threads live at the cursor's position.
    #[must_use]
    pub fn active_thread_count(&self) -> usize {
        self.cursor.as_ref().unwrap().ActiveThreadCount()
    }

    /// One thread live at the cursor's position.
    #[must_use]
    pub fn active_thread(&self, index: usize) -> Option<ActiveThread> {
        if index >= self.active_thread_count() {
            return None;
        }
        Some(self.cursor.as_ref().unwrap().ActiveThreadAt(index))
    }

    /// Select the event kinds that stop a replay.
    pub fn set_event_mask(&mut self, mask: EventMask) {
        self.cursor.as_mut().unwrap().SetEventMask(mask.bits());
    }

    /// Select the exception classes that stop a replay.
    pub fn set_exception_mask(&mut self, mask: ExceptionMask) {
        self.cursor.as_mut().unwrap().SetExceptionMask(mask.bits());
    }

    /// Stop an in-flight replay. Callable from any thread.
    pub fn interrupt_replay(&self) {
        self.cursor.as_ref().unwrap().InterruptReplay();
    }

    /// Register one physical watchpoint.
    ///
    /// # Errors
    /// [`TtdError::Ffi`] when the SDK rejects it.
    pub fn watchpoint(
        &mut self,
        address: u64,
        size: u64,
        access: DataAccessMask,
        thread: u32,
    ) -> Result<bool> {
        Ok(self
            .cursor
            .as_mut()
            .unwrap()
            .AddMemoryWatchpoint(address, size, access.bits(), thread))
    }

    /// Remove one physical watchpoint.
    ///
    /// # Errors
    /// [`TtdError::Ffi`] when the SDK rejects it.
    pub fn remove_watchpoint(
        &mut self,
        address: u64,
        size: u64,
        access: DataAccessMask,
        thread: u32,
    ) -> Result<bool> {
        Ok(self.cursor.as_mut().unwrap().RemoveMemoryWatchpoint(
            address,
            size,
            access.bits(),
            thread,
        ))
    }

    /// Install a callback that may borrow local state.
    ///
    /// The borrow must cover every replay call made before
    /// [`Self::clear_watchpoint_callback`]; a panicking callback stops the
    /// replay and is re-thrown by that replay call.
    pub fn install_borrowed_callback(&mut self, callback: &mut WatchpointFn<'_>) {
        self.clear_watchpoint_callback();
        // SAFETY: the lifetime is erased only to fit the SDK's untyped
        // context; the caller keeps the closure alive until it clears the
        // callback, which happens before the borrow ends. The pointers have the
        // same layout.
        #[allow(clippy::transmute_ptr_to_ptr)]
        let erased: *mut WatchpointFn<'static> =
            unsafe { std::mem::transmute(std::ptr::from_mut::<WatchpointFn<'_>>(callback)) };
        let slot = Box::new(WatchpointSlot(erased));
        let context = (&raw const *slot) as usize;
        self.watchpoint = Some(slot);
        self.cursor
            .as_mut()
            .unwrap()
            .SetWatchpointCallback(watchpoint_shim, context);
    }

    /// Remove the watchpoint callback.
    pub fn clear_watchpoint_callback(&mut self) {
        self.cursor.as_mut().unwrap().ClearWatchpointCallback();
        self.watchpoint = None;
    }
}

impl Drop for ReplayCursor {
    fn drop(&mut self) {
        // The SDK must not keep a pointer to a closure that is about to go.
        self.clear_watchpoint_callback();
    }
}

/// `StepCount::Max` is `uint64_t(-2)`; `uint64_t(-1)` is the invalid sentinel.
const STEP_COUNT_MAX: u64 = u64::MAX - 1;

/// The SDK's "last position" sentinel.
fn max_position() -> TtdPosition {
    TtdPosition::new(u64::MAX, STEP_COUNT_MAX)
}

/// The SDK's "first position" sentinel.
fn min_position() -> TtdPosition {
    TtdPosition::new(0, 0)
}

/// Bridge the SDK's hit callback to the borrowed closure, converting the
/// reported `DataAccessType` into an access mask.
fn watchpoint_shim(context: usize, address: u64, size: u64, access: u8, thread: u32) -> bool {
    // SAFETY: the context is the address of the boxed slot owned by the
    // cursor; the slot outlives every replay that can call this shim.
    let slot = unsafe { &*(context as *const WatchpointSlot) };
    // SAFETY: the slot holds a pointer to the caller's live closure.
    let callback = unsafe { &mut *slot.0 };
    let access = DataAccessType::from(access).mask();

    call_guarded(|| callback(address, size, access, thread), || true)
}
