use std::ffi::c_void;
use std::sync::Arc;

use crate::ttd::engine::EngineInner;
use crate::ttd::error::TtdError;
use crate::ttd::ffi;
use crate::ttd::types::*;

/// Watchpoint-hit callback: `(addr, size, access_mask, thread_id) -> stop`.
///
/// `access_mask` is a `DataAccessMask` bitmask; `thread_id` is the OS thread
/// id of the hitting thread (0 when unknown). Return `true` to stop replay,
/// `false` to continue.
type WatchpointFn = dyn FnMut(u64, u64, u8, u32) -> bool + Send;

/// Replay-progress callback: current trace position.
type ProgressFn = dyn FnMut(TtdPosition) + Send;

// Shims: the C side hands us a thin pointer to a memory cell holding a fat
// `&mut dyn …` reference (either a heap `Box<dyn …>` or a stack
// `&mut &mut dyn …`); reconstruct and call it.
//
// Safety: ctx must have been installed by `set_watchpoint_callback`,
// `with_watchpoint_callback`, or the progress counterpart, and must be
// uninstalled (or still owned by this cursor) whenever C can invoke it.
unsafe extern "C" fn watchpoint_shim(
    ctx: *mut c_void,
    addr: u64,
    size: u64,
    access: u8,
    thread_id: u32,
) -> bool {
    // Safety: ctx was installed by `set_watchpoint_callback` /
    // `with_watchpoint_callback` and is uninstalled before the backing
    // closure is freed (see the install sites).
    let cb = unsafe { &mut *(ctx as *mut Box<WatchpointFn>) };
    cb(addr, size, access, thread_id)
}

unsafe extern "C" fn scoped_watchpoint_shim(
    ctx: *mut c_void,
    addr: u64,
    size: u64,
    access: u8,
    thread_id: u32,
) -> bool {
    // Written out without the `'static`-defaulted alias: the scoped callback
    // may borrow for an arbitrary lifetime.
    // Safety: ctx is the stack cell of a live `with_watchpoint_callback` call.
    let cb = unsafe { &mut *(ctx as *mut &mut (dyn FnMut(u64, u64, u8, u32) -> bool + Send)) };
    cb(addr, size, access, thread_id)
}

unsafe extern "C" fn progress_shim(ctx: *mut c_void, pos: ffi::TtdPosition) {
    // Safety: ctx was installed by `set_progress_callback` and outlives every
    // C-side invocation (cleared in Drop before the closure is freed).
    let cb = unsafe { &mut *(ctx as *mut Box<ProgressFn>) };
    cb(pos)
}

/// Safe wrapper around TTD Cursor.
///
/// A cursor is a handle to C++ state: it keeps its engine alive (via
/// [`Arc<EngineInner>`], so there is no lifetime parameter) and every
/// operation is a C call that mutates the C++ object. That is interior
/// mutability, so the operations below take `&self`; only the callback slots —
/// which are *Rust* state the C side holds a pointer to — need `&mut self`.
///
/// The type is `Send` but deliberately not `Sync`: a cursor may be moved
/// between threads, never shared.
///
/// Note on callbacks: the C side never sees the cursor's own address — the
/// context pointer is the *inner* heap box of the callback slot (or a stack
/// cell for the scoped variant), both stable across moves of this struct.
/// Moving a cursor with an installed callback is therefore safe; the boxed
/// cursors in `TtdProcess` are belt-and-braces, not a soundness requirement.
pub struct TtdCursor {
    ptr: *mut ffi::TtdCursor,
    /// Owned watchpoint callback (`Box<Box<dyn …>>`: the outer box keeps the
    /// inner at a stable heap address, which is what the C side holds).
    wp_slot: Option<Box<Box<WatchpointFn>>>,
    /// Owned progress callback, same layout as `wp_slot`.
    prog_slot: Option<Box<Box<ProgressFn>>>,
    /// Keeps the backing engine alive for as long as this cursor exists.
    _engine: Arc<EngineInner>,
}

// Safety: TtdCursor accesses C++ state that is not thread-safe.
// Marking Send to allow moving between threads, but not Sync.
unsafe impl Send for TtdCursor {}

impl TtdCursor {
    /// Create a new cursor positioned at the start of the trace.
    ///
    /// Called by [`TtdEngine::create_cursor`]; the `Arc` is what ties the
    /// cursor's lifetime to the engine's.
    pub(crate) fn new(engine: Arc<EngineInner>) -> Result<Self, TtdError> {
        let ptr = unsafe { ffi::ttd_cursor_create(engine.as_raw()) };
        if ptr.is_null() {
            return Err(TtdError::CursorCreationFailed);
        }
        Ok(Self {
            ptr,
            wp_slot: None,
            prog_slot: None,
            _engine: engine,
        })
    }

    /// Get current position.
    pub fn position(&self) -> TtdPosition {
        unsafe { ffi::ttd_cursor_get_pos(self.ptr) }
    }

    /// Set cursor position (jumps to closest valid position).
    pub fn set_position(&self, pos: TtdPosition) {
        unsafe { ffi::ttd_cursor_set_pos(self.ptr, pos) }
    }

    /// Step forward N instructions. Returns the replay result.
    pub fn step_forward(&self, steps: u64) -> Result<TtdReplayResult, TtdError> {
        let mut result = TtdReplayResult::default();
        let r = unsafe { ffi::ttd_cursor_step_forward(self.ptr, steps, &mut result) };
        if r != 0 {
            return Err(TtdError::Ffi("step_forward failed"));
        }
        Ok(result)
    }

    /// Step backward N instructions.
    pub fn step_backward(&self, steps: u64) -> Result<TtdReplayResult, TtdError> {
        let mut result = TtdReplayResult::default();
        let r = unsafe { ffi::ttd_cursor_step_backward(self.ptr, steps, &mut result) };
        if r != 0 {
            return Err(TtdError::Ffi("step_backward failed"));
        }
        Ok(result)
    }

    /// Replay forward to a position limit (stops on breakpoints/watchpoints).
    pub fn replay_forward_to(&self, limit: TtdPosition) -> Result<TtdReplayResult, TtdError> {
        let mut result = TtdReplayResult::default();
        let r = unsafe { ffi::ttd_cursor_replay_forward_to(self.ptr, limit, &mut result) };
        if r != 0 {
            return Err(TtdError::Ffi("replay_forward_to failed"));
        }
        Ok(result)
    }

    /// Replay backward to a position limit.
    pub fn replay_backward_to(&self, limit: TtdPosition) -> Result<TtdReplayResult, TtdError> {
        let mut result = TtdReplayResult::default();
        let r = unsafe { ffi::ttd_cursor_replay_backward_to(self.ptr, limit, &mut result) };
        if r != 0 {
            return Err(TtdError::Ffi("replay_backward_to failed"));
        }
        Ok(result)
    }

    /// Read memory at the given address. Returns bytes actually read.
    pub fn read_memory(&self, addr: u64, buf: &mut [u8]) -> usize {
        if buf.is_empty() {
            return 0;
        }
        let n =
            unsafe { ffi::ttd_cursor_read_mem(self.ptr, addr, buf.as_mut_ptr(), buf.len() as u32) };
        n as usize
    }

    /// Read all general-purpose registers.
    pub fn read_registers(&self) -> TtdX64Regs {
        unsafe { ffi::ttd_cursor_read_regs(self.ptr) }
    }

    /// Get current program counter (RIP).
    pub fn pc(&self) -> u64 {
        unsafe { ffi::ttd_cursor_get_pc(self.ptr) }
    }

    /// OS thread id of the cursor's current thread (0 if none).
    pub fn current_tid(&self) -> u32 {
        unsafe { ffi::ttd_cursor_get_current_tid(self.ptr) }
    }

    /// Raw FFI handle. Only for registering the cursor with an
    /// interrupt handle while a replay is in flight.
    pub(crate) fn as_raw(&self) -> *mut ffi::TtdCursor {
        self.ptr
    }

    /// Get number of active threads at current position.
    pub fn thread_count(&self) -> u32 {
        unsafe { ffi::ttd_cursor_thread_count(self.ptr) }
    }

    /// Get active thread info by index at current cursor position.
    pub fn active_thread_info(&self, idx: u32) -> Option<TtdActiveThreadInfo> {
        if idx >= self.thread_count() {
            return None;
        }
        let info = unsafe { ffi::ttd_cursor_active_thread_info(self.ptr, idx) };
        Some(info)
    }

    /// Read registers of a specific thread (thread_id=0 for current thread).
    pub fn read_regs_thread(&self, thread_id: u32) -> TtdX64Regs {
        unsafe { ffi::ttd_cursor_read_regs_thread(self.ptr, thread_id) }
    }

    /// Get PC of a specific thread (thread_id=0 for current thread).
    pub fn pc_thread(&self, thread_id: u32) -> u64 {
        unsafe { ffi::ttd_cursor_get_pc_thread(self.ptr, thread_id) }
    }

    /// Get position of a specific thread (thread_id=0 for current thread).
    pub fn pos_thread(&self, thread_id: u32) -> TtdPosition {
        unsafe { ffi::ttd_cursor_get_pos_thread(self.ptr, thread_id) }
    }

    /// Get the TEB address of a specific thread (thread_id=0 for current
    /// thread). On Windows x64 this is the GS base — Delve uses it to locate
    /// the goroutine `g` without executing code in the tracee.
    pub fn teb_thread(&self, thread_id: u32) -> u64 {
        unsafe { ffi::ttd_cursor_get_teb_thread(self.ptr, thread_id) }
    }

    /// Add a memory watchpoint.
    pub fn add_watchpoint(
        &self,
        addr: u64,
        size: u64,
        access_mask: u8,
        thread_id: u32,
    ) -> Result<(), TtdError> {
        let r =
            unsafe { ffi::ttd_cursor_add_watchpoint(self.ptr, addr, size, access_mask, thread_id) };
        if r != 0 {
            return Err(TtdError::watchpoint(
                "add_watchpoint",
                addr,
                size,
                access_mask,
            ));
        }
        Ok(())
    }

    /// Remove a memory watchpoint.
    pub fn remove_watchpoint(
        &self,
        addr: u64,
        size: u64,
        access_mask: u8,
        thread_id: u32,
    ) -> Result<(), TtdError> {
        let r = unsafe {
            ffi::ttd_cursor_remove_watchpoint(self.ptr, addr, size, access_mask, thread_id)
        };
        if r != 0 {
            return Err(TtdError::watchpoint(
                "remove_watchpoint",
                addr,
                size,
                access_mask,
            ));
        }
        Ok(())
    }

    /// Interrupt ongoing replay (callable from any thread).
    pub fn interrupt(&self) {
        unsafe { ffi::ttd_cursor_interrupt(self.ptr) }
    }

    /// Set the watchpoint callback (owned).
    ///
    /// The callback receives `(addr, size, access_mask, thread_id)` and
    /// returns whether replay should stop. It is invoked from the thread that
    /// runs the replay, synchronously during `replay_forward_to` /
    /// `replay_backward_to` / step calls. Installing a new callback replaces
    /// (and frees) the previous one; see [`Self::clear_watchpoint_callback`].
    pub fn set_watchpoint_callback(
        &mut self,
        cb: impl FnMut(u64, u64, u8, u32) -> bool + Send + 'static,
    ) {
        self.clear_watchpoint_callback();
        let mut slot = Box::new(Box::new(cb) as Box<WatchpointFn>);
        // Safety: slot stays owned by this cursor (stable heap address) until
        // clear_watchpoint_callback/Drop clears the C-side registration first.
        unsafe {
            ffi::ttd_cursor_set_watchpoint_cb(
                self.ptr,
                Some(watchpoint_shim),
                &mut *slot as *mut Box<WatchpointFn> as *mut c_void,
            );
        }
        self.wp_slot = Some(slot);
    }

    /// Run `f` with a temporary watchpoint callback.
    ///
    /// Unlike [`Self::set_watchpoint_callback`] the callback may borrow local
    /// state (no `'static` bound); it is only reachable while `f` runs and is
    /// uninstalled before this method returns — even if `f` panics. A callback
    /// installed with `set_watchpoint_callback` is re-armed afterwards.
    pub fn with_watchpoint_callback<R>(
        &mut self,
        mut cb: &mut (dyn FnMut(u64, u64, u8, u32) -> bool + Send),
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        // Re-arm the owned callback after the scope if one was installed.
        struct UninstallOnDrop<'cursor> {
            cursor: &'cursor mut TtdCursor,
        }
        impl Drop for UninstallOnDrop<'_> {
            fn drop(&mut self) {
                unsafe {
                    ffi::ttd_cursor_set_watchpoint_cb(self.cursor.ptr, None, std::ptr::null_mut())
                };
                if let Some(slot) = &mut self.cursor.wp_slot {
                    // Owned callback was shadowed by the scoped one: re-arm it.
                    // (`slot` is `&mut Box<Box<…>>`; two derefs reach the inner
                    // box whose heap address is what the C side holds.)
                    unsafe {
                        ffi::ttd_cursor_set_watchpoint_cb(
                            self.cursor.ptr,
                            Some(watchpoint_shim),
                            &mut **slot as *mut Box<WatchpointFn> as *mut c_void,
                        );
                    }
                }
            }
        }
        let guard = UninstallOnDrop { cursor: self };
        // A `&mut &mut dyn FnMut…` is a thin pointer — safe to hand to C.
        // The cell lives on this call's stack and outlives every C-side
        // invocation inside `f`; the guard uninstalls before returning.
        let ctx = &mut cb as *mut &mut (dyn FnMut(u64, u64, u8, u32) -> bool + Send) as *mut c_void;
        unsafe {
            ffi::ttd_cursor_set_watchpoint_cb(guard.cursor.ptr, Some(scoped_watchpoint_shim), ctx);
        }
        let out = f(guard.cursor);
        drop(guard);
        out
    }

    /// Clear the watchpoint callback, freeing any owned closure.
    pub fn clear_watchpoint_callback(&mut self) {
        unsafe { ffi::ttd_cursor_set_watchpoint_cb(self.ptr, None, std::ptr::null_mut()) };
        self.wp_slot = None;
    }

    /// Set the replay-progress callback (owned). Called with the cursor's
    /// current position while long replays make progress.
    pub fn set_progress_callback(&mut self, cb: impl FnMut(TtdPosition) + Send + 'static) {
        self.clear_progress_callback();
        let mut slot = Box::new(Box::new(cb) as Box<ProgressFn>);
        unsafe {
            ffi::ttd_cursor_set_progress_cb(
                self.ptr,
                Some(progress_shim),
                &mut *slot as *mut Box<ProgressFn> as *mut c_void,
            );
        }
        self.prog_slot = Some(slot);
    }

    /// Clear the progress callback, freeing any owned closure.
    pub fn clear_progress_callback(&mut self) {
        unsafe { ffi::ttd_cursor_set_progress_cb(self.ptr, None, std::ptr::null_mut()) };
        self.prog_slot = None;
    }

    /// Set event mask.
    pub fn set_event_mask(&self, mask: u32) {
        unsafe { ffi::ttd_cursor_set_event_mask(self.ptr, mask) }
    }

    /// Set exception mask.
    pub fn set_exception_mask(&self, mask: u32) {
        unsafe { ffi::ttd_cursor_set_exception_mask(self.ptr, mask) }
    }

    /// Set the default memory query policy (one of the `TTD_MEM_POLICY_*`
    /// constants). The SDK's `Default` policy may ignore memory written by
    /// other threads, which breaks cross-thread data such as the Go
    /// runtime's `allgs` array; debugger reads should use a global policy.
    pub fn set_memory_policy(&self, policy: u32) {
        unsafe { ffi::ttd_cursor_set_memory_policy(self.ptr, policy) }
    }
}

impl Drop for TtdCursor {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // Clear C++ callbacks to prevent dangling Rust closures before
            // destroying the cursor.
            unsafe {
                ffi::ttd_cursor_set_watchpoint_cb(self.ptr, None, std::ptr::null_mut());
                ffi::ttd_cursor_set_progress_cb(self.ptr, None, std::ptr::null_mut());
                ffi::ttd_cursor_destroy(self.ptr);
            }
        }
        // Free the owned closures only after the C side can no longer reach
        // them (fields are also dropped after Drop::drop by drop glue; this
        // makes the ordering explicit).
        self.wp_slot = None;
        self.prog_slot = None;
    }
}
