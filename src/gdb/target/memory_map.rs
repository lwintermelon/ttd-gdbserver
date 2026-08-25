//! qXfer shared-library list.
//!
//! The main executable is **not** part of the list: GDB already knows it from
//! `qXfer:exec-file` / the client's `file` command, and a shared-library entry
//! pointing at the trace's recorded path would make GDB re-open that path
//! (typically stale on the debugging machine) and resolve symbols against the
//! wrong module. Only genuinely shared modules are reported.
//!
//! The module address map (`qXfer:memory-map`) is deliberately **not**
//! advertised: GDB treats it as the complete set of readable regions, so a
//! map containing only module ranges makes GDB refuse to ask for stack/TEB
//! memory (it reports "Cannot access memory" locally without sending `m`,
//! because `mem inaccessible-by-default` is on). Falling back to remote `m`
//! reads is what lets GDB reach those pages through the positioned cursor.
//!
//! The XML itself is built once, in `GdbTarget::new`, and only chunked here:
//! see `xml` for the construction.

use gdbstub::target::TargetResult;
use gdbstub::target::ext::libraries::Libraries;

use crate::gdb::xml::write_xml_chunk;
use crate::target::DebugTarget;

use super::GdbTarget;

impl<T: DebugTarget + Send + 'static> Libraries for GdbTarget<T> {
    fn get_libraries(
        &self,
        offset: u64,
        length: usize,
        buf: &mut [u8],
    ) -> TargetResult<usize, Self> {
        // The payload is immutable (see `libraries_xml`), so answering a chunk
        // needs no backend access at all — it works even while a replay holds
        // the lock.
        write_xml_chunk(self.libraries_xml.as_bytes(), offset, length, buf)
    }
}
