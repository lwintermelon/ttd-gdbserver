//! Process-description packets: exec file, auxv, process info, section
//! offsets, and the target-description XML annexes.
//!
//! None of these control execution; all read-only ones go through
//! [`super::GdbTarget::try_backend`] so a packet that arrives during a replay
//! answers `E` instead of parking the event loop.

use gdbstub::common::Pid;
use gdbstub::target::ext::auxv::Auxv;
use gdbstub::target::ext::exec_file::ExecFile;
use gdbstub::target::ext::section_offsets::{Offsets, SectionOffsets};
use gdbstub::target::ext::target_description_xml_override::TargetDescriptionXmlOverride;
use gdbstub::target::{TargetError, TargetResult};

use crate::gdb::regs::target_xml;
use crate::gdb::xml::write_xml_chunk;
use crate::target::DebugTarget;

use super::GdbTarget;

impl<T: DebugTarget + Send + 'static> ExecFile for GdbTarget<T> {
    fn get_exec_file(
        &self,
        _pid: Option<Pid>,
        offset: u64,
        length: usize,
        buf: &mut [u8],
    ) -> TargetResult<usize, Self> {
        // The trace's main executable comes from the backend's own notion of
        // "the program", mirroring how `dlv replay` learns the executable for
        // an rr trace.
        let backend = self.try_backend()?;
        let path = match backend.main_module() {
            Some(m) => m.name,
            None => return Err(TargetError::NonFatal),
        };
        drop(backend);
        write_xml_chunk(path.as_bytes(), offset, length, buf)
    }
}

impl<T: DebugTarget + Send + 'static> Auxv for GdbTarget<T> {
    fn get_auxv(&self, offset: u64, length: usize, buf: &mut [u8]) -> TargetResult<usize, Self> {
        // Synthetic ELF auxv: AT_ENTRY = runtime base of the main module.
        // Delve needs the executable's runtime base to relocate DWARF; this
        // flows through its existing readAuxv -> EntryPointFromAuxv path.
        const AT_NULL: u64 = 0;
        const AT_ENTRY: u64 = 9;
        let Some(base) = self
            .runner
            .try_lock()
            .and_then(|backend| backend.main_module().map(|m| m.base_addr))
        else {
            return Err(TargetError::NonFatal);
        };
        let mut auxv = Vec::with_capacity(32);
        auxv.extend_from_slice(&AT_ENTRY.to_le_bytes());
        auxv.extend_from_slice(&base.to_le_bytes());
        auxv.extend_from_slice(&AT_NULL.to_le_bytes());
        auxv.extend_from_slice(&0u64.to_le_bytes());

        write_xml_chunk(&auxv, offset, length, buf)
    }
}

impl<T: DebugTarget + Send + 'static> SectionOffsets for GdbTarget<T> {
    fn get_section_offsets(
        &mut self,
    ) -> Result<Offsets<<Self::Arch as gdbstub::arch::Arch>::Usize>, Self::Error> {
        // GDB's `Segments` form wants the absolute address of the text
        // segment, not the image base: it relocates that segment and keeps
        // the rest of the image at fixed offsets. Reporting the image base
        // puts every symbol one section alignment (0x1000) low.
        //
        // `data_seg` is omitted on purpose: the remaining PE sections move by
        // the same delta as `.text`, and GDB then keeps them at fixed offsets
        // relative to the last relocated segment.
        let text_seg = self.text_seg.unwrap_or(0);
        Ok(Offsets::Segments {
            text_seg,
            data_seg: None,
        })
    }
}

impl<T: DebugTarget + Send + 'static> TargetDescriptionXmlOverride for GdbTarget<T> {
    fn target_description_xml(
        &self,
        annex: &[u8],
        offset: u64,
        length: usize,
        buf: &mut [u8],
    ) -> TargetResult<usize, Self> {
        let annex = std::str::from_utf8(annex).map_err(|_| TargetError::NonFatal)?;
        let xml = target_xml(annex).map_err(|_| TargetError::NonFatal)?;
        write_xml_chunk(xml, offset, length, buf)
    }
}
