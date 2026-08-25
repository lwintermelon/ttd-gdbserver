use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;

use crate::ttd::error::TtdError;
use crate::ttd::ffi;
use crate::ttd::types::*;

use super::cursor::TtdCursor;

/// The heap-allocated half of [`TtdEngine`]: owns the C++ `IReplayEngine`.
///
/// Cursors hold a strong reference to this, which is what makes them safe
/// without a lifetime parameter: the SDK states that after the engine is
/// destroyed, the only valid operation on a cursor is `Destroy()`. An
/// `Arc<EngineInner>` inside the cursor *guarantees* the engine outlives it,
/// instead of relying on the caller to respect a drop order.
pub(crate) struct EngineInner {
    ptr: *mut ffi::TtdEngine,
}

// Safety: the C++ engine may be *moved* between threads, but it is not
// thread-safe for concurrent use — hence `Send` without `Sync`. Not being
// `Sync` also keeps `Arc<EngineInner>` from becoming `Send`, so a cursor can
// only be handed to another thread through the explicit `unsafe impl Send`
// on `TtdCursor` (the same guarantee the previous design asserted by hand).
unsafe impl Send for EngineInner {}

impl EngineInner {
    pub(crate) fn as_raw(&self) -> *mut ffi::TtdEngine {
        self.ptr
    }
}

impl Drop for EngineInner {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { ffi::ttd_engine_destroy(self.ptr) };
        }
    }
}

/// Safe wrapper around TTD Replay Engine.
///
/// A cheap, clone-free handle to the shared `EngineInner`. Cursors created
/// from it keep the engine alive on their own.
pub struct TtdEngine {
    inner: Arc<EngineInner>,
}

// Safety: same reasoning as `EngineInner`; the handle is never shared.
unsafe impl Send for TtdEngine {}

impl TtdEngine {
    /// Create a new TTD Replay Engine instance.
    pub fn new() -> Result<Self, TtdError> {
        let ptr = unsafe { ffi::ttd_engine_create() };
        if ptr.is_null() {
            return Err(TtdError::EngineCreationFailed);
        }
        Ok(Self {
            // `EngineInner` is deliberately `Send` but not `Sync` (the C++
            // engine is not thread-safe), so this `Arc` is used purely as a
            // lifetime device — cursors keep the engine alive by holding a
            // clone — never to share it across threads.
            #[allow(clippy::arc_with_non_send_sync)]
            inner: Arc::new(EngineInner { ptr }),
        })
    }

    /// Load a .run trace file.
    pub fn load_trace(&self, path: &Path) -> Result<(), TtdError> {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let r = unsafe { ffi::ttd_engine_load(self.inner.as_raw(), wide.as_ptr()) };
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
        let engine = Self::new()?;
        engine.load_trace(path)?;
        engine.build_index()?;
        Ok(engine)
    }

    /// Build the global memory index for the loaded trace.
    ///
    /// Idempotent: the engine skips the build when a valid index already
    /// exists next to the trace file.
    pub fn build_index(&self) -> Result<(), TtdError> {
        let r = unsafe { ffi::ttd_engine_build_index(self.inner.as_raw()) };
        if r != 0 {
            return Err(TtdError::IndexBuildFailed { code: r });
        }
        Ok(())
    }

    /// Like [`Self::build_index`], but forwards keyframe progress to
    /// `callback(processed, total)`. When a valid index file already exists
    /// the build is skipped and no callbacks fire.
    ///
    /// Synchronous: `callback` is only invoked while this call is on the
    /// stack, so it may borrow local state.
    pub fn build_index_with_progress<F>(&self, mut callback: F) -> Result<(), TtdError>
    where
        F: FnMut(u32, u32),
    {
        // Safety: `callback` lives on this call's stack and outlives every C
        // invocation; BuildIndex is synchronous.
        unsafe extern "C" fn trampoline<F: FnMut(u32, u32)>(
            ctx: *mut c_void,
            processed: u32,
            total: u32,
        ) {
            // Safety: ctx points at this call's stack-local `callback`, which
            // outlives the synchronous BuildIndex call.
            let f = unsafe { &mut *(ctx as *mut F) };
            f(processed, total);
        }
        let r = unsafe {
            ffi::ttd_engine_build_index_cb(
                self.inner.as_raw(),
                Some(trampoline::<F>),
                &mut callback as *mut F as *mut c_void,
            )
        };
        if r != 0 {
            return Err(TtdError::IndexBuildFailed { code: r });
        }
        Ok(())
    }

    /// Get the first (earliest) position in the trace.
    pub fn first_position(&self) -> TtdPosition {
        unsafe { ffi::ttd_engine_first_pos(self.inner.as_raw()) }
    }

    /// Get the last (latest) position in the trace.
    pub fn last_position(&self) -> TtdPosition {
        unsafe { ffi::ttd_engine_last_pos(self.inner.as_raw()) }
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
        unsafe { ffi::ttd_engine_thread_count(self.inner.as_raw()) }
    }

    /// Get thread info by index (0..thread_count).
    pub fn thread_info(&self, idx: u32) -> Option<TtdThreadInfo> {
        if idx >= self.thread_count() {
            return None;
        }
        let info = unsafe { ffi::ttd_engine_thread_info(self.inner.as_raw(), idx) };
        Some(info)
    }

    /// Get all thread infos.
    pub fn thread_list(&self) -> Vec<TtdThreadInfo> {
        let count = self.thread_count();
        (0..count).filter_map(|i| self.thread_info(i)).collect()
    }

    /// Get the number of exception events in the trace.
    pub fn exception_count(&self) -> u32 {
        unsafe { ffi::ttd_engine_exception_count(self.inner.as_raw()) }
    }

    /// Get the exception event at the given index.
    pub fn exception_event(&self, idx: u32) -> TtdExceptionEvent {
        unsafe { ffi::ttd_engine_exception_event(self.inner.as_raw(), idx) }
    }

    /// Get all exception events.
    pub fn exception_list(&self) -> Vec<TtdExceptionEvent> {
        let count = self.exception_count();
        (0..count).map(|i| self.exception_event(i)).collect()
    }

    /// Get the number of modules in the trace.
    pub fn module_count(&self) -> u32 {
        unsafe { ffi::ttd_engine_module_count(self.inner.as_raw()) }
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
                self.inner.as_raw(),
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
    /// The returned cursor keeps the engine alive (it holds a strong
    /// reference to it), so it has no lifetime parameter and may outlive this
    /// handle.
    pub fn create_cursor(&self) -> Result<TtdCursor, TtdError> {
        TtdCursor::new(self.inner.clone())
    }
}
