//! The cross-thread interrupt channel for a replay in flight.
//!
//! A replay runs on one thread while ^C arrives on another. The cursor is a
//! C++ object that only the replaying thread may drive, so the handoff is a
//! single integer slot: the owner publishes the cursor's address for the
//! duration of the call and clears it afterwards. Keeping that discipline in
//! one type means the rest of the crate never touches a raw pointer.
//!
//! [`InterruptSlot::publish`] returns a guard that clears the slot on drop,
//! including on the panic path.

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::ttd::ReplayCursor;

/// The interrupt slot owned by a replaying target and read by ^C handlers.
#[derive(Debug, Default)]
pub(crate) struct InterruptSlot {
    /// Cursor address, or 0 when no replay is in flight.
    handle: AtomicUsize,
}

impl InterruptSlot {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Publish `cursor` for interrupters until the returned guard drops.
    ///
    /// Only the address is read here; the caller keeps the cursor alive (and
    /// immovable) for the guard's lifetime, which a replay call does by
    /// holding its `&mut ReplayCursor` across the whole operation.
    pub(crate) fn publish<'a>(&'a self, cursor: &ReplayCursor) -> Published<'a> {
        self.handle
            .store(std::ptr::from_ref(cursor) as usize, Ordering::Release);
        Published { slot: self }
    }

    /// Drop any published handle.
    ///
    /// Only needed on paths that do not hold a [`Published`] guard (teardown);
    /// normal replays clear through the guard, including when they panic.
    pub(crate) fn clear(&self) {
        self.handle.store(0, Ordering::Release);
    }

    /// Ask the replay in flight, if any, to stop.
    ///
    /// Safe: a non-zero value is only visible while a [`Published`] guard is
    /// alive, and that guard borrows the cursor it published, so the pointer
    /// cannot dangle. `ReplayCursor::interrupt_replay` is documented as
    /// callable from any thread, and an interrupt that arrives after the
    /// replay finished is a no-op on a still-valid cursor.
    pub(crate) fn interrupt(&self) {
        let handle = self.handle.load(Ordering::Acquire);
        if handle != 0 {
            // SAFETY: see the method note — the handle names a live cursor.
            unsafe { interrupt_raw(handle) };
        }
    }
}

/// Cast a published handle back to a cursor and interrupt it.
///
/// # Safety
/// `handle` must come from [`InterruptSlot::publish`] on a cursor kept alive
/// by the publish/clear pair around the replay call.
unsafe fn interrupt_raw(handle: usize) {
    // SAFETY: the caller guarantees the handle names a live cursor.
    let cursor = unsafe { &*(handle as *const ReplayCursor) };
    cursor.interrupt_replay();
}

/// Proof that a cursor is published; clears the slot on drop.
#[derive(Debug)]
pub(crate) struct Published<'a> {
    slot: &'a InterruptSlot,
}

impl Drop for Published<'_> {
    fn drop(&mut self) {
        self.slot.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Interrupting an idle slot is the path ^C takes when nothing is
    /// replaying: it must be a no-op, not a dereference of the empty handle.
    #[test]
    fn interrupt_on_an_idle_slot_is_a_no_op() {
        let slot = InterruptSlot::new();
        slot.interrupt();
        assert_eq!(slot.handle.load(Ordering::Acquire), 0);
        slot.interrupt();
    }

    /// Teardown clears the slot explicitly; doing so twice (or with nothing
    /// published) stays empty.
    #[test]
    fn clear_is_idempotent() {
        let slot = InterruptSlot::new();
        slot.clear();
        slot.clear();
        assert_eq!(slot.handle.load(Ordering::Acquire), 0);
    }
}
