//! Extended mode: `vRun` restart and the process lifecycle packets.
//!
//! `vRun` is Delve's restart form (`vRun;;<hex position|checkpoint>`); the
//! command parsing itself lives in [`crate::gdb::commands`]. `R` restart is
//! the older GDB form and only ever jumps to the trace start.

use gdbstub::common::Pid;
use gdbstub::target::ext::extended_mode::{AttachKind, ExtendedMode, ShouldTerminate};
use gdbstub::target::{TargetError, TargetResult};

use crate::gdb::commands;
use crate::target::DebugTarget;

use super::GdbTarget;

impl<T: DebugTarget + Send + 'static> ExtendedMode for GdbTarget<T> {
    fn run(
        &mut self,
        _filename: Option<&[u8]>,
        args: gdbstub::target::ext::extended_mode::Args<'_, '_>,
    ) -> TargetResult<Pid, Self> {
        // Restarting while a replay runs would block on the backend lock and
        // stall the event loop, so vRun refuses with a NonFatal error.
        let Some(mut backend) = self.runner.try_lock() else {
            return Err(TargetError::NonFatal);
        };
        commands::handle_vrun(&mut *backend, &self.checkpoints, &mut args.into_iter()).map_err(
            |e| {
                log::warn!("vRun restart rejected: {e}");
                // The client asked for a restart it cannot have; answer with
                // an error packet instead of a successful pid.
                TargetError::NonFatal
            },
        )?;
        drop(backend);
        // The goto moved the backend away from a process-exit stop, so a
        // forward resume must replay again rather than report `W00`, and the
        // resume Delve sends right after the restart stops at this position.
        self.clear_end_state();
        self.note_restart();
        Ok(super::server_pid())
    }

    fn attach(&mut self, _pid: Pid) -> TargetResult<(), Self> {
        Err(TargetError::NonFatal)
    }

    fn query_if_attached(&mut self, _pid: Pid) -> TargetResult<AttachKind, Self> {
        Ok(AttachKind::Attach)
    }

    fn kill(&mut self, _pid: Option<Pid>) -> TargetResult<ShouldTerminate, Self> {
        Ok(ShouldTerminate::Yes)
    }

    fn restart(&mut self) -> Result<(), Self::Error> {
        // `R` can arrive while a replay is in flight on a confused client;
        // blocking on the backend would park the event loop (and ^C with it),
        // and this trait method has no non-fatal error path. Ignoring the
        // restart is the least destructive answer; the client's stop reply
        // arrives from the worker as usual.
        let Some(mut backend) = self.runner.try_lock() else {
            log::warn!("R restart ignored while a replay is in flight");
            return Ok(());
        };
        let (first, _) = backend.lifetime();
        // A failed goto leaves the backend where it was, which is a valid
        // stop for the client; `R` has no error reply, so tearing the session
        // down (gdbstub's only other answer) would be strictly worse.
        if let Err(e) = backend.goto(first) {
            log::warn!("R restart: goto to trace start failed: {e}");
            return Ok(());
        }
        drop(backend);
        self.clear_end_state();
        self.note_restart();
        Ok(())
    }
}
