//! The replay engine handle: one per loaded trace.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;

use crate::ttd::callback::{call_guarded, callback_panic_guard};
use crate::ttd::cursor::ReplayCursor;
use crate::ttd::error::TtdError;
use crate::ttd::types::{
    ReplayModule, TtdExceptionEvent, TtdIndexStatus, TtdPositionRange, TtdThreadInfo,
};
use crate::ttd_sys as sys;

/// What the trace's process was built for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Architecture {
    X64,
    X86,
    #[default]
    Unknown,
}

/// Owns the SDK engine. Shared with every cursor the engine creates: the SDK
/// requires the engine to outlive its cursors.
pub(crate) struct EngineInner {
    engine: sys::ttd_sys_Engine,
}

impl EngineInner {
    fn new() -> Option<Self> {
        // SAFETY: the SDK constructor initialises the handle on success;
        // `IsValid` reports whether it did.
        let engine = unsafe { sys::ttd_sys_Engine::new() };
        let valid = unsafe { engine.IsValid() };
        valid.then_some(Self { engine })
    }

    pub(crate) fn engine(&self) -> &sys::ttd_sys_Engine {
        &self.engine
    }

    pub(crate) fn engine_mut(&mut self) -> &mut sys::ttd_sys_Engine {
        &mut self.engine
    }

    pub(crate) fn raw_ptr(&self) -> *mut sys::ttd_sys_Engine {
        (&raw const self.engine).cast_mut()
    }

    pub(crate) fn architecture(&self) -> Architecture {
        // SAFETY: the pointer names engine-owned data valid for the call.
        let info = unsafe { self.engine.GetSystemInfo() };
        if info.is_null() {
            return Architecture::Unknown;
        }
        // SAFETY: `GetSystemInfo` returns a pointer to an engine-owned
        // `SystemInfo`; its architecture field holds the Win32
        // `PROCESSOR_ARCHITECTURE_*` constant the recorder saw.
        match unsafe { (*info).System.ProcessorArchitecture } {
            // PROCESSOR_ARCHITECTURE_AMD64
            9 => Architecture::X64,
            // PROCESSOR_ARCHITECTURE_INTEL
            0 => Architecture::X86,
            _ => Architecture::Unknown,
        }
    }
}

impl Drop for EngineInner {
    fn drop(&mut self) {
        // SAFETY: the handle was initialised by `new` and is destroyed once.
        unsafe { self.engine.destruct() };
    }
}

// SAFETY: the SDK engine is not thread-safe, so `EngineInner` is deliberately
// not `Sync`; `Send` only allows moving the whole handle to another thread.
unsafe impl Send for EngineInner {}

/// A loaded trace.
///
/// Deliberately not `Clone`: the SDK engine is single-threaded, so a second
/// handle could be driven from another thread.
pub struct ReplayEngine {
    inner: Arc<EngineInner>,
}

// SAFETY: see `EngineInner`. The engine is moved, never shared.
unsafe impl Send for ReplayEngine {}

impl ReplayEngine {
    /// Open a trace file.
    ///
    /// # Errors
    /// [`TtdError::Ffi`] when the engine cannot be created,
    /// [`TtdError::TraceLoadFailed`] when the trace cannot be loaded.
    pub fn open(path: &Path) -> Result<Self, TtdError> {
        let mut inner = EngineInner::new().ok_or(TtdError::Ffi("create replay engine"))?;

        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `wide` is NUL-terminated and outlives the call.
        if unsafe { inner.engine_mut().Load(wide.as_ptr()) } != 0 {
            return Err(TtdError::TraceLoadFailed {
                path: path.display().to_string(),
            });
        }
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    fn raw(&self) -> &sys::ttd_sys_Engine {
        &self.inner.engine
    }

    /// The architecture the trace's process was built for.
    #[must_use]
    pub fn architecture(&self) -> Architecture {
        self.inner.architecture()
    }

    /// A new cursor positioned at the start of the trace.
    ///
    /// # Errors
    /// [`TtdError::CursorCreationFailed`] when the SDK refuses to create one.
    pub fn cursor(&self) -> Result<ReplayCursor, TtdError> {
        ReplayCursor::new(self.engine_inner())
    }

    pub(crate) fn engine_inner(&self) -> Arc<EngineInner> {
        Arc::clone(&self.inner)
    }

    /// The trace's `(first, last)` positions.
    #[must_use]
    pub fn lifetime(&self) -> TtdPositionRange {
        // SAFETY: the pointer names engine-owned data valid for the call.
        unsafe { *self.raw().GetLifetime() }
    }

    /// The trace index's state.
    #[must_use]
    pub fn index_state(&self) -> TtdIndexStatus {
        // SAFETY: the handle is valid for the engine's lifetime.
        unsafe { self.raw().GetIndexStatus() }.into()
    }

    /// Build the trace index, reporting keyframe progress.
    ///
    /// # Errors
    /// [`TtdError::IndexBuildFailed`] when the SDK does not end up with a
    /// loaded index.
    ///
    /// # Panics
    /// When the engine has already been shared with a cursor: the index must
    /// be built before the trace is used, because an incomplete index makes
    /// every memory read inaccurate.
    pub fn build_index_with_progress<F>(
        &mut self,
        mut progress: F,
    ) -> Result<TtdIndexStatus, TtdError>
    where
        F: FnMut(usize, usize),
    {
        unsafe extern "C" fn report(
            context: *const std::ffi::c_void,
            progress: *const sys::TTD_Replay_IndexBuildProgressType,
        ) {
            // SAFETY: both pointers are the SDK's arguments and stay valid for
            // the duration of the call.
            let (callback, progress) = unsafe {
                (
                    &mut *(context as *mut &mut dyn FnMut(usize, usize)),
                    &*progress,
                )
            };
            let (processed, total) = (
                progress.KeyframesProcessed as usize,
                progress.KeyframeCount as usize,
            );
            call_guarded(|| callback(processed, total), || ());
        }

        let _guard = callback_panic_guard();
        let mut erased: &mut dyn FnMut(usize, usize) = &mut progress;
        let context = (&raw mut erased).cast::<std::ffi::c_void>().cast_const();
        // SAFETY: the callback and its context outlive the call.
        let engine = Arc::get_mut(&mut self.inner)
            .expect("the index must be built before the engine is shared")
            .engine_mut();
        let raw = unsafe { engine.BuildIndex(Some(report), context) };
        let status = TtdIndexStatus::from(raw);
        if status == TtdIndexStatus::Loaded {
            Ok(status)
        } else {
            Err(TtdError::IndexBuildFailed)
        }
    }

    /// Every thread that ever existed in the trace, in TTD's order.
    #[must_use]
    pub fn threads(&self) -> Vec<TtdThreadInfo> {
        // SAFETY: the SDK owns the list for the engine's lifetime; it is
        // copied out here.
        unsafe {
            let count = self.raw().GetThreadCount();
            let list = self.raw().GetThreadList();
            if list.is_null() {
                return Vec::new();
            }
            std::slice::from_raw_parts(list, count).to_vec()
        }
    }

    /// Every module the trace recorded.
    #[must_use]
    pub fn modules(&self) -> Vec<ReplayModule> {
        // SAFETY: the SDK owns the list for the engine's lifetime; names are
        // copied out below.
        let modules = unsafe {
            let count = self.raw().GetModuleCount();
            let list = self.raw().GetModuleList();
            if list.is_null() {
                return Vec::new();
            }
            std::slice::from_raw_parts(list, count)
        };

        modules
            .iter()
            .map(|module| ReplayModule {
                name: if module.pName.is_null() {
                    String::new()
                } else {
                    // SAFETY: the SDK owns a UTF-16 name of `NameLength` units
                    // for as long as the engine lives.
                    String::from_utf16_lossy(unsafe {
                        std::slice::from_raw_parts(module.pName, module.NameLength)
                    })
                },
                address: module.Address,
                size: module.Size,
            })
            .collect()
    }

    /// Every exception event the trace recorded.
    #[must_use]
    pub fn exception_events(&self) -> Vec<TtdExceptionEvent> {
        // SAFETY: the SDK owns the list for the engine's lifetime; it is
        // copied out here.
        unsafe {
            let count = self.raw().GetExceptionEventCount();
            let list = self.raw().GetExceptionEventList();
            if list.is_null() {
                return Vec::new();
            }
            std::slice::from_raw_parts(list, count)
                .iter()
                .map(|event| TtdExceptionEvent {
                    position: event.Position,
                    code: event.Code,
                    record_address: event.RecordAddress,
                    program_counter: event.ProgramCounter,
                })
                .collect()
        }
    }
}
