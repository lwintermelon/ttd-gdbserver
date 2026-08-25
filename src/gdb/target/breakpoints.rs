//! Breakpoint and watchpoint protocol extensions (`Z`/`z` packets).
//!
//! TTD has no native execution breakpoints: they are emulated as TTD memory
//! watchpoints by the backend, and this layer only maps the GDB wire kinds
//! onto that API.

use gdbstub::target::TargetResult;
use gdbstub::target::ext::breakpoints::{Breakpoints, HwWatchpoint, SwBreakpoint, WatchKind};

use crate::target::DebugTarget;
use crate::ttd::types::TTD_Replay_DataAccessMask_Execute;

use super::GdbTarget;
use crate::gdb::mapping::{access_mask_from_watch_kind, breakpoint_kind_from_watch_kind};

// ─── Breakpoints (Z0 sw + Z1 hw + Z2/Z3/Z4 watchpoints / z* remove) ─

impl<T: DebugTarget + Send + 'static> Breakpoints for GdbTarget<T> {
    fn support_sw_breakpoint(
        &mut self,
    ) -> Option<gdbstub::target::ext::breakpoints::SwBreakpointOps<'_, Self>> {
        Some(self)
    }

    fn support_hw_breakpoint(
        &mut self,
    ) -> Option<gdbstub::target::ext::breakpoints::HwBreakpointOps<'_, Self>> {
        Some(self)
    }

    fn support_hw_watchpoint(
        &mut self,
    ) -> Option<gdbstub::target::ext::breakpoints::HwWatchpointOps<'_, Self>> {
        Some(self)
    }
}

// Removal is ADDRESS-keyed end to end: RSP `z` packets identify breakpoints
// by `(addr, kind)` only, clients legitimately accumulate several backend IDs
// for one address (restarts re-insert breakpoints the stub still holds), and
// a new session has no memory of earlier session's IDs. The backend's
// `remove_breakpoint_exact` therefore clears every ID at the triple — no
// per-session ID bookkeeping is needed (or possible).

impl<T: DebugTarget + Send + 'static> SwBreakpoint for GdbTarget<T> {
    fn add_sw_breakpoint(&mut self, addr: u64, _kind: usize) -> TargetResult<bool, Self> {
        match self.backend().set_breakpoint(addr, None) {
            Ok(id) => {
                log::debug!("Z0 sw bp add addr={:#x} id={}", addr, id);
                Ok(true)
            }
            // Ok(false) is the RSP "cannot insert breakpoint" answer; the
            // client stops instead of continuing into an unwatched address.
            Err(e) => {
                log::warn!("Z0 sw bp add failed addr={:#x}: {}", addr, e);
                Ok(false)
            }
        }
    }

    fn remove_sw_breakpoint(&mut self, addr: u64, _kind: usize) -> TargetResult<bool, Self> {
        log::debug!("z0 sw bp remove addr={:#x}", addr);
        let removed =
            self.backend()
                .remove_breakpoint_exact(addr, 1, TTD_Replay_DataAccessMask_Execute);
        // Answer OK unconditionally: clients remove stale breakpoints on every
        // reconnect, and an error there breaks the handshake. Log the miss so
        // "deleted but still breaking" stays diagnosable.
        if !removed {
            log::warn!("z0 sw bp remove addr={:#x}: nothing registered", addr);
        }
        Ok(true)
    }
}

impl<T: DebugTarget + Send + 'static> gdbstub::target::ext::breakpoints::HwBreakpoint
    for GdbTarget<T>
{
    fn add_hw_breakpoint(&mut self, addr: u64, _kind: usize) -> TargetResult<bool, Self> {
        match self.backend().set_breakpoint(addr, None) {
            Ok(id) => {
                log::debug!("Z1 hw bp add addr={:#x} id={}", addr, id);
                Ok(true)
            }
            Err(e) => {
                log::warn!("Z1 hw bp add failed addr={:#x}: {}", addr, e);
                Ok(false)
            }
        }
    }

    fn remove_hw_breakpoint(&mut self, addr: u64, _kind: usize) -> TargetResult<bool, Self> {
        log::debug!("z1 hw bp remove addr={:#x}", addr);
        let removed =
            self.backend()
                .remove_breakpoint_exact(addr, 1, TTD_Replay_DataAccessMask_Execute);
        if !removed {
            log::warn!("z1 hw bp remove addr={:#x}: nothing registered", addr);
        }
        Ok(true)
    }
}

impl<T: DebugTarget + Send + 'static> HwWatchpoint for GdbTarget<T> {
    fn add_hw_watchpoint(
        &mut self,
        addr: u64,
        len: u64,
        kind: WatchKind,
    ) -> TargetResult<bool, Self> {
        match self
            .backend()
            .set_data_breakpoint(addr, len, breakpoint_kind_from_watch_kind(kind))
        {
            Ok(id) => {
                log::debug!(
                    "Z2/3/4 watch add addr={:#x} len={} kind={:?} id={}",
                    addr,
                    len,
                    kind,
                    id
                );
                Ok(true)
            }
            Err(e) => {
                log::warn!(
                    "Z2/3/4 watch add failed addr={:#x} len={}: {}",
                    addr,
                    len,
                    e
                );
                Ok(false)
            }
        }
    }

    fn remove_hw_watchpoint(
        &mut self,
        addr: u64,
        len: u64,
        kind: WatchKind,
    ) -> TargetResult<bool, Self> {
        let access = access_mask_from_watch_kind(kind);
        let removed = self.backend().remove_breakpoint_exact(addr, len, access);
        if !removed {
            log::warn!(
                "z2/3/4 watch remove addr={:#x} len={}: nothing registered",
                addr,
                len
            );
        }
        Ok(true)
    }
}
