//! `monitor` command dispatch (`qRcmd`).
//!
//! Rendering lives in [`crate::gdb::monitor`]; this module only bridges the
//! protocol `ConsoleOutput` to it and keeps the backend lock out of the wire
//! write.

use gdbstub::target::ext::monitor_cmd::MonitorCmd;

use crate::gdb::monitor;
use crate::target::DebugTarget;

use super::GdbTarget;

impl<T: DebugTarget + Send + 'static> MonitorCmd for GdbTarget<T> {
    fn handle_monitor_cmd(
        &mut self,
        cmd: &[u8],
        mut out: gdbstub::target::ext::monitor_cmd::ConsoleOutput<'_>,
    ) -> Result<(), Self::Error> {
        let s = std::str::from_utf8(cmd).unwrap_or("");
        // A monitor command can arrive while a replay is running; answer
        // without the backend in that case (see `TargetRunner::try_lock`).
        let Some(backend) = self.runner.try_lock() else {
            out.write_raw(b"target is running; retry when stopped\n");
            return Ok(());
        };
        // Render with the lock held, then release it before touching the
        // wire: the connection write can block, and the worker may be
        // waiting on this lock to publish its stop reason.
        let mut text = String::new();
        monitor::render(s, &*backend, &mut text);
        drop(backend);
        out.write_raw(text.as_bytes());
        Ok(())
    }
}
