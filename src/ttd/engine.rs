//! The replay engine handle: one per loaded trace.

use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use cxx::UniquePtr;

use crate::ttd::callback::{call_guarded, callback_panic_guard};
use crate::ttd::cursor::ReplayCursor;
use crate::ttd::error::TtdError;
use crate::ttd::ffi;
use crate::ttd::types::{
    ReplayModule, TtdExceptionEvent, TtdIndexStatus, TtdPositionRange, TtdThreadInfo,
};

/// What the trace's process was built for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Architecture {
    X64,
    X86,
    #[default]
    Unknown,
}

/// Owns the cxx engine handle. Shared with every cursor the engine creates: the
/// SDK requires the engine to outlive its cursors.
pub(crate) struct EngineInner {
    engine: UniquePtr<ffi::Engine>,
}

impl EngineInner {
    fn new() -> Option<Self> {
        let engine = ffi::NewEngine();
        if engine.is_null() || !engine.as_ref().unwrap().IsValid() {
            return None;
        }
        Some(Self { engine })
    }

    pub(crate) fn engine(&self) -> &ffi::Engine {
        self.engine.as_ref().unwrap()
    }

    pub(crate) fn engine_mut(&mut self) -> Pin<&mut ffi::Engine> {
        self.engine.as_mut().unwrap()
    }

    pub(crate) fn architecture(&self) -> Architecture {
        match self.engine().Architecture() {
            // PROCESSOR_ARCHITECTURE_AMD64
            9 => Architecture::X64,
            // PROCESSOR_ARCHITECTURE_INTEL
            0 => Architecture::X86,
            _ => Architecture::Unknown,
        }
    }
}

// SAFETY: the SDK engine is not thread-safe, so `EngineInner` is deliberately
// not `Sync`; `Send` only allows moving the whole handle to another thread.
unsafe impl Send for EngineInner {}

// SAFETY: the shared `EngineInner` is only used for immutable queries
// (`architecture`/`lifetime`) and cursor creation, both serialized by the
// target's mutex; the SDK engine itself is never driven concurrently.
unsafe impl Sync for EngineInner {}

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
        let path_str = path
            .to_str()
            .ok_or(TtdError::Ffi("trace path is not valid UTF-8"))?;
        cxx::let_cxx_string!(cpath = path_str);
        if !inner.engine_mut().Load(&cpath) {
            return Err(TtdError::TraceLoadFailed {
                path: path.display().to_string(),
            });
        }
        Ok(Self {
            inner: Arc::new(inner),
        })
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
        self.inner.engine().Lifetime()
    }

    /// The trace index's state.
    #[must_use]
    pub fn index_state(&self) -> TtdIndexStatus {
        self.inner.engine().IndexStatus().into()
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
        fn report(context: usize, processed: usize, total: usize) {
            // SAFETY: `context` points at the `&mut dyn FnMut` created below,
            // which outlives the synchronous BuildIndex call.
            let callback = unsafe { &mut *(context as *mut &mut dyn FnMut(usize, usize)) };
            call_guarded(|| callback(processed, total), || ());
        }

        let _guard = callback_panic_guard();
        let mut erased: &mut dyn FnMut(usize, usize) = &mut progress;
        let context = std::ptr::from_mut(&mut erased) as usize;
        let engine = Arc::get_mut(&mut self.inner)
            .expect("the index must be built before the engine is shared")
            .engine_mut();
        let raw = engine.BuildIndex(report, context);
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
        let engine = self.inner.engine();
        (0..engine.ThreadCount())
            .map(|index| engine.ThreadAt(index))
            .collect()
    }

    /// Every module the trace recorded.
    #[must_use]
    pub fn modules(&self) -> Vec<ReplayModule> {
        let engine = self.inner.engine();
        (0..engine.ModuleCount())
            .map(|index| {
                let module = engine.ModuleAt(index);
                ReplayModule {
                    name: engine.ModuleName(index).clone(),
                    address: module.address,
                    size: module.size,
                }
            })
            .collect()
    }

    /// Every exception event the trace recorded.
    #[must_use]
    pub fn exception_events(&self) -> Vec<TtdExceptionEvent> {
        let engine = self.inner.engine();
        (0..engine.ExceptionCount())
            .map(|index| engine.ExceptionAt(index))
            .collect()
    }
}
