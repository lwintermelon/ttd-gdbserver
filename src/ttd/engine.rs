use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use crate::ttd::error::TtdError;
use crate::ttd::ffi;
use crate::ttd::types::*;

use super::cursor::TtdCursor;

/// Safe wrapper around TTD Replay Engine.
///
/// Owns the C++ IReplayEngine object. Must outlive any TtdCursor created from it.
pub struct TtdEngine {
    ptr: *mut ffi::TtdEngine,
}

unsafe impl Send for TtdEngine {}

impl TtdEngine {
    /// Create a new TTD Replay Engine instance.
    pub fn new() -> Result<Self, TtdError> {
        let ptr = unsafe { ffi::ttd_engine_create() };
        if ptr.is_null() {
            return Err(TtdError::EngineCreationFailed);
        }
        Ok(Self { ptr })
    }

    /// Load a .run trace file.
    pub fn load_trace(&mut self, path: &Path) -> Result<(), TtdError> {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let r = unsafe { ffi::ttd_engine_load(self.ptr, wide.as_ptr()) };
        if r != 0 {
            return Err(TtdError::TraceLoadFailed {
                path: path.display().to_string(),
                code: r,
            });
        }
        Ok(())
    }

    /// Convenience: create engine, load trace, build the memory index.
    ///
    /// The index is required for accurate memory recovery (see the TTD
    /// Concepts doc: the index file is "the only way to recover process
    /// memory with reasonable accuracy"). Without it, old memory writes
    /// (e.g. the Go runtime's `allgs` array) read back as zero.
    pub fn open(path: &Path) -> Result<Self, TtdError> {
        let mut engine = Self::new()?;
        engine.load_trace(path)?;
        engine.build_index()?;
        Ok(engine)
    }

    /// Build the global memory index for the loaded trace.
    ///
    /// Idempotent: the engine skips the build when a valid index already
    /// exists next to the trace file.
    pub fn build_index(&mut self) -> Result<(), TtdError> {
        let r = unsafe { ffi::ttd_engine_build_index(self.ptr) };
        if r != 0 {
            return Err(TtdError::IndexBuildFailed { code: r });
        }
        Ok(())
    }

    /// Get the first (earliest) position in the trace.
    pub fn first_position(&self) -> TtdPosition {
        unsafe { ffi::ttd_engine_first_pos(self.ptr) }
    }

    /// Get the last (latest) position in the trace.
    pub fn last_position(&self) -> TtdPosition {
        unsafe { ffi::ttd_engine_last_pos(self.ptr) }
    }

    /// Get the trace lifetime as a position range.
    pub fn lifetime(&self) -> TtdPositionRange {
        TtdPositionRange {
            min: self.first_position(),
            max: self.last_position(),
        }
    }

    /// Get the number of threads in the trace.
    pub fn thread_count(&self) -> u32 {
        unsafe { ffi::ttd_engine_thread_count(self.ptr) }
    }

    /// Get thread info by index (0..thread_count).
    pub fn thread_info(&self, idx: u32) -> Option<TtdThreadInfo> {
        if idx >= self.thread_count() {
            return None;
        }
        let info = unsafe { ffi::ttd_engine_thread_info(self.ptr, idx) };
        Some(info)
    }

    /// Get all thread infos.
    pub fn thread_list(&self) -> Vec<TtdThreadInfo> {
        let count = self.thread_count();
        (0..count).filter_map(|i| self.thread_info(i)).collect()
    }

    /// Get the number of exception events in the trace.
    pub fn exception_count(&self) -> u32 {
        unsafe { ffi::ttd_engine_exception_count(self.ptr) }
    }

    /// Get the exception event at the given index.
    pub fn exception_event(&self, idx: u32) -> TtdExceptionEvent {
        unsafe { ffi::ttd_engine_exception_event(self.ptr, idx) }
    }

    /// Get all exception events.
    pub fn exception_list(&self) -> Vec<TtdExceptionEvent> {
        let count = self.exception_count();
        (0..count).map(|i| self.exception_event(i)).collect()
    }

    /// Get the number of modules in the trace.
    pub fn module_count(&self) -> u32 {
        unsafe { ffi::ttd_engine_module_count(self.ptr) }
    }

    /// Get module info by index. Returns (base_addr, size, name).
    pub fn module_info(&self, idx: u32) -> Option<(u64, u64, String)> {
        if idx >= self.module_count() {
            return None;
        }
        let mut addr: u64 = 0;
        let mut size: u64 = 0;
        let mut name_buf = [0u16; 512];
        unsafe {
            ffi::ttd_engine_module_info(
                self.ptr,
                idx,
                &mut addr,
                &mut size,
                name_buf.as_mut_ptr(),
                name_buf.len() as u32,
            );
        }
        let name = String::from_utf16_lossy(
            &name_buf[..name_buf
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(name_buf.len())],
        );
        Some((addr, size, name))
    }

    /// Create a new cursor positioned at the start of the trace.
    ///
    /// The returned cursor borrows this engine, so the engine cannot be dropped
    /// while the cursor is alive.
    pub fn create_cursor(&self) -> Result<TtdCursor<'_>, TtdError> {
        let ptr = unsafe { ffi::ttd_cursor_create(self.ptr) };
        if ptr.is_null() {
            return Err(TtdError::CursorCreationFailed);
        }
        Ok(TtdCursor::from_raw(ptr))
    }
}

impl Drop for TtdEngine {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { ffi::ttd_engine_destroy(self.ptr) };
        }
    }
}
