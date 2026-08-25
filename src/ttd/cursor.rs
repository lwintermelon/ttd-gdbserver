//! A replay cursor: the position and thread context every query resolves
//! against.

use std::sync::Arc;

use crate::ttd::callback::{call_guarded, callback_panic_guard};
use crate::ttd::engine::EngineInner;
use crate::ttd::error::{Result, TtdError};
use crate::ttd::types::{
    DataAccessMask, EventMask, ExceptionMask, TtdMemRange, TtdPosition, TtdReplayResult,
    TtdThreadInfo, TtdX64Regs, XmmSaveArea,
};
use crate::ttd_sys as sys;

/// A watchpoint callback: `(address, size, access mask, OS thread id)`,
/// returning whether replay should stop there.
pub(crate) type WatchpointFn<'a> = dyn FnMut(u64, u64, DataAccessMask, u32) -> bool + Send + 'a;

/// A thread live at the cursor's position.
#[derive(Clone, Copy, Debug, Default)]
pub struct ActiveThread {
    pub thread: TtdThreadInfo,
    pub current_position: TtdPosition,
}

/// A handle to an SDK cursor.
///
/// A cursor owns a reference to its engine, so it can outlive the
/// [`ReplayEngine`] value it was created from. It is `Send` (a debugger worker
/// owns it) but deliberately not `Sync`.
///
/// [`ReplayEngine`]: crate::ttd::ReplayEngine
pub struct ReplayCursor {
    cursor: sys::ttd_sys_Cursor,
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

impl ReplayCursor {
    pub(crate) fn new(engine: Arc<EngineInner>) -> Result<Self> {
        // SAFETY: the engine handle outlives the cursor (held by `engine`).
        let mut cursor = unsafe { sys::ttd_sys_Cursor::new(engine.raw_ptr()) };
        if unsafe { !cursor.IsValid() } {
            return Err(TtdError::CursorCreationFailed);
        }

        // New cursors start at the beginning of the trace.
        let start = unsafe { (*engine.engine().GetLifetime()).Min };
        unsafe { cursor.SetPosition(&raw const start) };
        Ok(Self {
            cursor,
            engine,
            watchpoint: None,
        })
    }

    /// The cursor's position.
    #[must_use]
    pub fn position(&self) -> TtdPosition {
        // SAFETY: the pointer names cursor-owned data valid for the call.
        unsafe { *self.cursor.GetPosition() }
    }

    pub fn set_position(&mut self, position: TtdPosition) {
        // SAFETY: the position pointer is valid for the call.
        unsafe { self.cursor.SetPosition(&raw const position) };
    }

    /// Replay forward, stopping at `limit` or on a subscribed event.
    ///
    /// # Errors
    /// [`TtdError::Ffi`] when the SDK rejects the replay.
    pub fn replay_forward(&mut self, limit: TtdPosition) -> Result<TtdReplayResult> {
        self.replay(limit, sys::TTD_Replay_StepCount_Max, true)
    }

    /// Replay backward, stopping at `limit` or on a subscribed event.
    ///
    /// # Errors
    /// [`TtdError::Ffi`] when the SDK rejects the replay.
    pub fn replay_backward(&mut self, limit: TtdPosition) -> Result<TtdReplayResult> {
        self.replay(limit, sys::TTD_Replay_StepCount_Max, false)
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
        // SAFETY: the cursor and the position pointer are valid for the call.
        let raw = unsafe {
            if forward {
                sys::ttd_sys_Cursor::ReplayForward(&raw mut self.cursor, &raw const limit, steps)
            } else {
                sys::ttd_sys_Cursor::ReplayBackward(&raw mut self.cursor, &raw const limit, steps)
            }
        };
        if raw.StopReason == sys::TTD_Replay_EventType_Invalid {
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
        // SAFETY: the buffer is valid for `size` bytes.
        let read = unsafe {
            sys::ttd_sys_Cursor::QueryMemoryBuffer(
                &raw const self.cursor,
                address,
                buffer.as_mut_ptr().cast(),
                size,
            )
        };
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
        let mut ranges = vec![TtdMemRange::default(); max_ranges.max(1)];
        let mut count = 0;
        // SAFETY: both buffers are valid for their lengths.
        let read = unsafe {
            sys::ttd_sys_Cursor::QueryMemoryBufferWithRanges(
                &raw const self.cursor,
                address,
                buffer.as_mut_ptr().cast(),
                size,
                ranges.as_mut_ptr(),
                ranges.len(),
                &raw mut count,
            )
        };
        buffer.truncate(read);
        ranges.truncate(count);
        Ok((buffer, ranges))
    }

    /// The program counter of the cursor's current thread.
    #[must_use]
    pub fn pc(&self) -> u64 {
        // SAFETY: the handle is valid for the cursor's lifetime.
        unsafe { self.cursor.GetProgramCounter() }
    }

    /// The stack pointer of the cursor's current thread.
    #[must_use]
    pub fn sp(&self) -> u64 {
        // SAFETY: the handle is valid for the cursor's lifetime.
        unsafe { self.cursor.GetStackPointer() }
    }

    /// The cursor's current thread.
    #[must_use]
    pub fn thread_info(&self) -> TtdThreadInfo {
        // SAFETY: the pointer names cursor-owned data valid for the call.
        unsafe { *self.cursor.GetThreadInfo() }
    }

    /// TEB (GS base) of `thread`; `0` means the cursor's current thread.
    #[must_use]
    pub fn teb_address_of(&self, thread: u32) -> u64 {
        // SAFETY: the handle is valid for the cursor's lifetime.
        unsafe { self.cursor.GetTebAddress(thread) }
    }

    /// The AMD64 register context of `thread`; `0` means the cursor's thread.
    ///
    /// # Errors
    /// [`TtdError::UnsupportedEvent`] when the trace is not an x64 recording.
    pub fn amd64_context(&self, thread: u32) -> Result<TtdX64Regs> {
        // SAFETY: the handle is valid for the cursor's lifetime; the returned
        // context is an opaque blob owned by the caller.
        let context =
            unsafe { sys::ttd_sys_Cursor::GetRegisterContext(&raw const self.cursor, thread) };
        ArchContext::from_sdk(&context, self.engine.architecture())
    }

    /// The extended (AVX) register context of `thread`; `0` means the cursor's
    /// thread.
    ///
    /// # Errors
    /// [`TtdError::UnsupportedEvent`] when the trace is not an x64 recording.
    pub fn extended_context(&self, thread: u32) -> Result<XmmSaveArea> {
        // SAFETY: the handle is valid for the cursor's lifetime.
        let context = unsafe {
            sys::ttd_sys_Cursor::GetExtendedRegisterContext(&raw const self.cursor, thread)
        };
        ArchContext::extended_from_sdk(&context, self.engine.architecture())
    }

    /// Number of threads live at the cursor's position.
    #[must_use]
    pub fn active_thread_count(&self) -> usize {
        // SAFETY: the handle is valid for the cursor's lifetime.
        unsafe { self.cursor.GetThreadCount() }
    }

    /// One thread live at the cursor's position.
    #[must_use]
    pub fn active_thread(&self, index: usize) -> Option<ActiveThread> {
        if index >= self.active_thread_count() {
            return None;
        }
        // SAFETY: `index` is in range and the list is cursor-owned.
        unsafe {
            let entry = &*self.cursor.GetThreadList().add(index);
            let thread = entry.pThread.as_ref()?;
            Some(ActiveThread {
                thread: *thread,
                current_position: entry.CurrentPosition,
            })
        }
    }

    /// Select the event kinds that stop a replay.
    pub fn set_event_mask(&mut self, mask: EventMask) {
        // SAFETY: the handle is valid for the cursor's lifetime.
        unsafe { self.cursor.SetEventMask(mask.bits()) };
    }

    /// Select the exception classes that stop a replay.
    pub fn set_exception_mask(&mut self, mask: ExceptionMask) {
        // SAFETY: the handle is valid for the cursor's lifetime.
        unsafe { self.cursor.SetExceptionMask(mask.bits()) };
    }

    /// Stop an in-flight replay. Callable from any thread.
    pub fn interrupt_replay(&self) {
        // SAFETY: `InterruptReplay` is documented as callable from any thread.
        unsafe { self.cursor.InterruptReplay() };
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
        let data = sys::TTD_Replay_MemoryWatchpointData {
            Address: address,
            Size: size,
            AccessMask: access.bits(),
            ThreadId: thread,
        };
        // SAFETY: the data pointer is valid for the call.
        Ok(unsafe { self.cursor.AddMemoryWatchpoint(&raw const data) })
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
        let data = sys::TTD_Replay_MemoryWatchpointData {
            Address: address,
            Size: size,
            AccessMask: access.bits(),
            ThreadId: thread,
        };
        // SAFETY: the data pointer is valid for the call.
        Ok(unsafe { self.cursor.RemoveMemoryWatchpoint(&raw const data) })
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
        // callback, which happens before the borrow ends.
        // The lifetime is erased only to fit the SDK's untyped context. `as`
        // cannot do it (it requires equal lifetimes), so the pointers are
        // transmuted; they have the same layout.
        #[allow(clippy::transmute_ptr_to_ptr)]
        let erased: *mut WatchpointFn<'static> =
            unsafe { std::mem::transmute(std::ptr::from_mut::<WatchpointFn<'_>>(callback)) };
        let slot = Box::new(WatchpointSlot(erased));
        let context = (&raw const *slot) as usize;
        self.watchpoint = Some(slot);
        // SAFETY: the slot stays owned by this cursor until it is cleared.
        unsafe {
            self.cursor
                .SetMemoryWatchpointCallback(Some(watchpoint_shim), context);
        }
    }

    /// Remove the watchpoint callback.
    pub fn clear_watchpoint_callback(&mut self) {
        // SAFETY: the handle is valid for the cursor's lifetime.
        unsafe { self.cursor.SetMemoryWatchpointCallback(None, 0) };
        self.watchpoint = None;
    }
}

impl Drop for ReplayCursor {
    fn drop(&mut self) {
        // The SDK must not keep a pointer to a closure that is about to go.
        self.clear_watchpoint_callback();
        // SAFETY: the handle came from the SDK and is destroyed once.
        unsafe { self.cursor.destruct() };
    }
}

/// The SDK's "last position" sentinel.
fn max_position() -> TtdPosition {
    TtdPosition {
        Sequence: sys::TTD_SequenceId_Max,
        Steps: sys::TTD_Replay_StepCount_Max,
    }
}

/// The SDK's "first position" sentinel.
fn min_position() -> TtdPosition {
    TtdPosition {
        Sequence: sys::TTD_SequenceId_Min,
        Steps: sys::TTD_Replay_StepCount_Min,
    }
}

/// Bridge the SDK's hit callback to the borrowed closure, converting the
/// reported `DataAccessType` into an access mask.
unsafe extern "C" fn watchpoint_shim(
    context: usize,
    hit: *const sys::TTD_Replay_ICursorView_MemoryWatchpointResult,
    thread: *const sys::TTD_Replay_IThreadView,
) -> bool {
    // SAFETY: both pointers are the SDK's own arguments, and the SDK owns the
    // thread view for the duration of this callback. `ThreadView` is a
    // non-owning handle, so nothing here outlives the call.
    let (hit, thread) = unsafe { (&*hit, &*thread) };
    let slot = unsafe { &*(context as *const WatchpointSlot) };
    let callback = unsafe { &mut *slot.0 };
    let thread_id = unsafe {
        sys::ttd_sys_ThreadView::new(std::ptr::from_ref(thread))
            .GetThreadInfo()
            .as_ref()
            .map_or(0, |info| info.Id)
    };
    let access = crate::ttd::types::DataAccessType::from(hit.AccessType).mask();

    call_guarded(
        || callback(hit.Address, hit.Size, access, thread_id),
        || true,
    )
}

/// Interpretation of the SDK's architecture-agnostic register blobs.
struct ArchContext;

const _: () = assert!(
    std::mem::size_of::<TtdX64Regs>() <= std::mem::size_of::<sys::TTD_Replay_RegisterContext>()
);
const _: () = assert!(
    std::mem::size_of::<XmmSaveArea>()
        <= std::mem::size_of::<sys::TTD_Replay_ExtendedRegisterContext>()
);

impl ArchContext {
    /// Reinterpret a `RegisterContext` as the amd64 context it holds.
    fn from_sdk(
        context: &sys::TTD_Replay_RegisterContext,
        architecture: crate::ttd::engine::Architecture,
    ) -> Result<TtdX64Regs> {
        use crate::ttd::engine::Architecture;
        if architecture != Architecture::X64 {
            return Err(TtdError::UnsupportedArchitecture);
        }
        // SAFETY: for an x64 trace the blob starts with an `AMD64_CONTEXT`,
        // which is smaller than the blob (asserted below).
        Ok(unsafe { std::ptr::read_unaligned(context.Data.as_ptr().cast::<TtdX64Regs>()) })
    }

    /// Reinterpret an `ExtendedRegisterContext` as an AVX context.
    fn extended_from_sdk(
        context: &sys::TTD_Replay_ExtendedRegisterContext,
        architecture: crate::ttd::engine::Architecture,
    ) -> Result<XmmSaveArea> {
        use crate::ttd::engine::Architecture;
        if architecture != Architecture::X64 {
            return Err(TtdError::UnsupportedArchitecture);
        }
        // SAFETY: for an x64 trace the blob holds an `AMD64_CONTEXT`.
        let registers =
            unsafe { std::ptr::read_unaligned(context.Data.as_ptr().cast::<TtdX64Regs>()) };
        Ok(registers.flt_save())
    }
}
