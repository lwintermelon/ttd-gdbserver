//! Panic escalation across the callback FFI boundary.
//!
//! The C shims in `cursor.rs`/`engine.rs` invoke user-supplied Rust closures.
//! Letting a panic unwind out of an `extern "C"` frame is undefined
//! behaviour, so every shim wraps the call in `catch_unwind` and stashes the
//! payload in a thread-local slot instead. The public wrapper that made the
//! FFI call holds a [`CallbackPanicGuard`]; when the call returns, the guard
//! re-throws the payload with `resume_unwind` on the original thread.
//!
//! A panicked watchpoint callback makes its shim return `true` (stop the
//! replay) so a broken callback can never keep filtering hits.

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

thread_local! {
    static STASHED: RefCell<Option<Box<dyn std::any::Any + Send + 'static>>> =
        const { RefCell::new(None) };
}

/// Run `f`, converting a panic into a stashed payload + `on_panic`.
///
/// This is the single funnel every callback shim must use: it keeps
/// `extern "C"` frames unwind-free while preserving the panic for the
/// guarded FFI wrapper to re-throw.
pub(crate) fn call_guarded<R>(f: impl FnOnce() -> R, on_panic: impl FnOnce() -> R) -> R {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(payload) => {
            STASHED.with(|s| {
                let mut slot = s.borrow_mut();
                if slot.is_some() {
                    // A payload is already pending (a second callback fired
                    // after the first one panicked). Keep the first: it is
                    // the root failure, and its guard is closest to the
                    // original FFI call.
                    log::error!(
                        "callback panicked twice across one FFI call; dropping the second payload"
                    );
                } else {
                    *slot = Some(payload);
                }
            });
            on_panic()
        }
    }
}

/// Held by every public FFI entry point that can synchronously invoke a
/// user-installed callback. On drop — after the FFI call has returned and the
/// C stack is gone — a stashed callback panic is re-thrown with
/// `resume_unwind`, so the panicking closure still unwinds the Rust caller
/// even though the `extern "C"` frames it crossed are gone.
pub(crate) struct CallbackPanicGuard {
    // No public constructor outside this module: taking the guard also
    // clears any stale payload, which must not happen mid-call.
    _private: (),
}

/// Arm panic escalation for one FFI call. Any callback panic stashed while
/// the guard is alive is re-thrown when the guard drops.
pub(crate) fn callback_panic_guard() -> CallbackPanicGuard {
    // Defensive: a payload stashed without a live guard (impossible through
    // the public API, but cheap to handle) would otherwise re-throw from an
    // unrelated later call.
    STASHED.with(|s| {
        let mut slot = s.borrow_mut();
        if slot.is_some() {
            log::error!("stale callback panic found at FFI entry; dropping it");
            *slot = None;
        }
    });
    CallbackPanicGuard { _private: () }
}

impl Drop for CallbackPanicGuard {
    fn drop(&mut self) {
        if let Some(payload) = STASHED.with(|s| s.borrow_mut().take()) {
            resume_unwind(payload);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::panic_any;

    #[test]
    fn guarded_call_passes_through_ok() {
        let r = call_guarded(|| 42, || 0);
        assert_eq!(r, 42);
    }

    #[test]
    fn panicking_callback_is_stashed_and_rerun_by_the_guard() {
        let result = catch_unwind(AssertUnwindSafe(|| {
            let guard = callback_panic_guard();
            let r = call_guarded(|| panic_any("boom"), || "fallback");
            assert_eq!(r, "fallback");
            drop(guard);
        }));
        let payload = result.expect_err("guard must re-throw the stashed panic");
        let msg = payload
            .downcast_ref::<&str>()
            .expect("payload is the original panic value");
        assert_eq!(*msg, "boom");
    }

    #[test]
    fn second_panic_keeps_the_first_payload() {
        let result = catch_unwind(AssertUnwindSafe(|| {
            let guard = callback_panic_guard();
            call_guarded(|| panic_any("first"), || ());
            call_guarded(|| panic_any("second"), || ());
            drop(guard);
        }));
        let payload = result.expect_err("guard must re-throw");
        let msg = payload
            .downcast_ref::<&str>()
            .expect("payload is a panic value");
        assert_eq!(*msg, "first");
    }

    #[test]
    fn stale_payload_is_cleared_at_the_next_entry() {
        // Simulate a stash without a guard (only possible from within this
        // module) and check the next guard drops it instead of re-throwing.
        call_guarded(|| panic_any("orphan"), || ());
        let result = catch_unwind(AssertUnwindSafe(|| {
            let guard = callback_panic_guard();
            drop(guard);
        }));
        assert!(
            result.is_ok(),
            "stale payload must not re-throw from an unrelated call"
        );
    }
}
