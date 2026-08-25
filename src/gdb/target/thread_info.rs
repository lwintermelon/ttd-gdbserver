//! `qThreadExtraInfo`: the per-thread text shown in `info threads`.
//!
//! The reply is ASCII (gdbstub hex-encodes it on the wire), so a plain
//! byte-copy is enough; the buffer may be shorter than the text and is then
//! truncated, which is what gdb expects from this packet.

use gdbstub::common::Tid;
use gdbstub::target::ext::thread_extra_info::ThreadExtraInfo;

use crate::target::DebugTarget;

use super::GdbTarget;

/// Non-fatal answer for a thread-info query that arrives during a replay.
/// This trait method returns the target's `Error` type directly, and gdbstub
/// treats any such error as fatal, so an empty/error reply is not an option.
const RUNNING_REPLY: &[u8] = b"target is running";

impl<T: DebugTarget + Send + 'static> ThreadExtraInfo for GdbTarget<T> {
    fn thread_extra_info(&self, tid: Tid, buf: &mut [u8]) -> Result<usize, Self::Error> {
        // Format: "UTID <unique> (OS 0x<os>); pos <seq>:<steps>".
        // The OS thread id is implicit (gdb already shows it as `Thread N.M`),
        // but we include it for self-containment. The trace position lets
        // the user locate the thread on the time axis at a glance.
        let Some(backend) = self.runner.try_lock() else {
            let n = RUNNING_REPLY.len().min(buf.len());
            buf[..n].copy_from_slice(&RUNNING_REPLY[..n]);
            return Ok(n);
        };
        let os_tid = u64::try_from(tid.get()).expect("usize is 64-bit on supported targets");
        let s = match backend.thread_info(os_tid) {
            Some(i) => format!(
                "UTID {} (OS 0x{:x}); pos {}",
                i.unique_id,
                tid.get(),
                i.current_position
            ),
            None => format!("UTID ? (OS 0x{:x})", tid.get()),
        };
        let n = s.len().min(buf.len());
        buf[..n].copy_from_slice(&s.as_bytes()[..n]);
        Ok(n)
    }
}
