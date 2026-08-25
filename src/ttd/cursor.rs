use std::ffi::c_void;
use std::marker::PhantomData;

use crate::ttd::error::TtdError;
use crate::ttd::ffi;
use crate::ttd::types::*;

/// Safe wrapper around TTD Cursor.
///
/// The lifetime parameter `'engine` ensures this cursor cannot outlive the
/// engine that created it. The TTD C++ API states: "after destroying the engine
/// that backs up the cursor, the only operation that is guaranteed to be valid
/// on the cursor is Destroy()."
pub struct TtdCursor<'engine> {
    ptr: *mut ffi::TtdCursor,
    // PhantomData<&'engine TtdEngine> tells the drop checker that this type
    // may access a TtdEngine with lifetime 'engine, preventing the engine
    // from being dropped while this cursor exists.
    _engine: PhantomData<&'engine super::engine::TtdEngine>,
}

// Safety: TtdCursor accesses C++ state that is not thread-safe.
// Marking Send to allow moving between threads, but not Sync.
unsafe impl<'engine> Send for TtdCursor<'engine> {}

impl<'engine> TtdCursor<'engine> {
    /// Create from raw pointer. Called by TtdEngine::create_cursor().
    pub(crate) fn from_raw(ptr: *mut ffi::TtdCursor) -> Self {
        Self {
            ptr,
            _engine: PhantomData,
        }
    }

    /// Get current position.
    pub fn position(&self) -> TtdPosition {
        unsafe { ffi::ttd_cursor_get_pos(self.ptr) }
    }

    /// Set cursor position (jumps to closest valid position).
    pub fn set_position(&mut self, pos: TtdPosition) {
        unsafe { ffi::ttd_cursor_set_pos(self.ptr, pos) }
    }

    /// Step forward N instructions. Returns the replay result.
    pub fn step_forward(&mut self, steps: u64) -> Result<TtdReplayResult, TtdError> {
        let mut result = TtdReplayResult::default();
        let r = unsafe { ffi::ttd_cursor_step_forward(self.ptr, steps, &mut result) };
        if r != 0 {
            return Err(TtdError::Ffi("step_forward failed"));
        }
        Ok(result)
    }

    /// Step backward N instructions.
    pub fn step_backward(&mut self, steps: u64) -> Result<TtdReplayResult, TtdError> {
        let mut result = TtdReplayResult::default();
        let r = unsafe { ffi::ttd_cursor_step_backward(self.ptr, steps, &mut result) };
        if r != 0 {
            return Err(TtdError::Ffi("step_backward failed"));
        }
        Ok(result)
    }

    /// Replay forward to a position limit (stops on breakpoints/watchpoints).
    pub fn replay_forward_to(&mut self, limit: TtdPosition) -> Result<TtdReplayResult, TtdError> {
        let mut result = TtdReplayResult::default();
        let r = unsafe { ffi::ttd_cursor_replay_forward_to(self.ptr, limit, &mut result) };
        if r != 0 {
            return Err(TtdError::Ffi("replay_forward_to failed"));
        }
        Ok(result)
    }

    /// Replay backward to a position limit.
    pub fn replay_backward_to(&mut self, limit: TtdPosition) -> Result<TtdReplayResult, TtdError> {
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

    /// Read memory into a new Vec.
    pub fn read_memory_vec(&self, addr: u64, len: usize) -> Vec<u8> {
        let mut buf = vec![0u8; len];
        let n = self.read_memory(addr, &mut buf);
        buf.truncate(n);
        buf
    }

    /// Read all general-purpose registers.
    pub fn read_registers(&self) -> TtdX64Regs {
        unsafe { ffi::ttd_cursor_read_regs(self.ptr) }
    }

    /// Get current program counter (RIP).
    pub fn pc(&self) -> u64 {
        unsafe { ffi::ttd_cursor_get_pc(self.ptr) }
    }

    /// Get current stack pointer (RSP).
    pub fn sp(&self) -> u64 {
        unsafe { ffi::ttd_cursor_get_sp(self.ptr) }
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

    /// Get SP of a specific thread (thread_id=0 for current thread).
    pub fn sp_thread(&self, thread_id: u32) -> u64 {
        unsafe { ffi::ttd_cursor_get_sp_thread(self.ptr, thread_id) }
    }

    /// Get position of a specific thread (thread_id=0 for current thread).
    pub fn pos_thread(&self, thread_id: u32) -> TtdPosition {
        unsafe { ffi::ttd_cursor_get_pos_thread(self.ptr, thread_id) }
    }

    /// Get the TEB address of a specific thread (thread_id=0 for current thread).
    /// On Windows x64 this is the GS base — Delve uses it to locate the
    /// goroutine `g` without executing code in the tracee.
    pub fn teb_thread(&self, thread_id: u32) -> u64 {
        unsafe { ffi::ttd_cursor_get_teb_thread(self.ptr, thread_id) }
    }

    /// Add a memory watchpoint.
    pub fn add_watchpoint(
        &mut self,
        addr: u64,
        size: u64,
        access_mask: u8,
        thread_id: u32,
    ) -> Result<(), TtdError> {
        let r =
            unsafe { ffi::ttd_cursor_add_watchpoint(self.ptr, addr, size, access_mask, thread_id) };
        if r != 0 {
            return Err(TtdError::Ffi("add_watchpoint failed"));
        }
        Ok(())
    }

    /// Remove a memory watchpoint.
    pub fn remove_watchpoint(
        &mut self,
        addr: u64,
        size: u64,
        access_mask: u8,
        thread_id: u32,
    ) -> Result<(), TtdError> {
        let r = unsafe {
            ffi::ttd_cursor_remove_watchpoint(self.ptr, addr, size, access_mask, thread_id)
        };
        if r != 0 {
            return Err(TtdError::Ffi("remove_watchpoint failed"));
        }
        Ok(())
    }

    /// Interrupt ongoing replay (callable from any thread).
    pub fn interrupt(&self) {
        unsafe { ffi::ttd_cursor_interrupt(self.ptr) }
    }

    /// Set raw watchpoint callback (function pointer + void* context).
    ///
    /// # Safety
    /// Caller must ensure `ctx` remains valid until the callback is cleared
    /// or the cursor is dropped.
    pub unsafe fn set_watchpoint_callback_raw(
        &mut self,
        cb: unsafe extern "C" fn(*mut c_void, u64, u64, u8) -> bool,
        ctx: *mut c_void,
    ) {
        ffi::ttd_cursor_set_watchpoint_cb(self.ptr, Some(cb), ctx);
    }

    /// Clear watchpoint callback.
    pub fn clear_watchpoint_callback(&mut self) {
        unsafe { ffi::ttd_cursor_set_watchpoint_cb(self.ptr, None, std::ptr::null_mut()) };
    }

    /// Set event mask.
    pub fn set_event_mask(&mut self, mask: u32) {
        unsafe { ffi::ttd_cursor_set_event_mask(self.ptr, mask) }
    }

    /// Set exception mask.
    pub fn set_exception_mask(&mut self, mask: u32) {
        unsafe { ffi::ttd_cursor_set_exception_mask(self.ptr, mask) }
    }

    /// Set the default memory query policy (one of the `TTD_MEM_POLICY_*`
    /// constants). The SDK's `Default` policy may ignore memory written by
    /// other threads, which breaks cross-thread data such as the Go
    /// runtime's `allgs` array; debugger reads should use a global policy.
    pub fn set_memory_policy(&mut self, policy: u32) {
        unsafe { ffi::ttd_cursor_set_memory_policy(self.ptr, policy) }
    }
}

impl<'engine> Drop for TtdCursor<'engine> {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // Clear C++ callbacks to prevent dangling Rust function pointers.
            // The TTD API states that after engine destruction, the only valid
            // operation on a cursor is Destroy(). But while the engine is alive,
            // we must clear callbacks before destroying the cursor.
            unsafe {
                ffi::ttd_cursor_set_watchpoint_cb(self.ptr, None, std::ptr::null_mut());
                ffi::ttd_cursor_set_progress_cb(self.ptr, None, std::ptr::null_mut());
                ffi::ttd_cursor_destroy(self.ptr);
            }
        }
    }
}
