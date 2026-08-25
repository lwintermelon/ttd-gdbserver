//! Introspection and process-description extensions.
//!
//! Extended mode (`vRun` restart), exec-file, auxv, process info, library /
//! memory-map XML, section offsets, thread extra info, `monitor ttd ...` and
//! the target description XML. None of these perform execution control.

use gdbstub::common::{Endianness, Pid, Tid};
use gdbstub::target::ext::auxv::Auxv;
use gdbstub::target::ext::exec_file::ExecFile;
use gdbstub::target::ext::extended_mode::{AttachKind, ExtendedMode, ShouldTerminate};
use gdbstub::target::ext::libraries::Libraries;
use gdbstub::target::ext::memory_map::MemoryMap;
use gdbstub::target::ext::monitor_cmd::MonitorCmd;
use gdbstub::target::ext::process_info::{ProcessInfo, ProcessInfoResponse};
use gdbstub::target::ext::section_offsets::{Offsets, SectionOffsets};
use gdbstub::target::ext::target_description_xml_override::TargetDescriptionXmlOverride;
use gdbstub::target::ext::thread_extra_info::ThreadExtraInfo;
use gdbstub::target::{TargetError, TargetResult};

use crate::target::DebugTarget;

use super::GdbTarget;
use crate::gdb::commands;
use crate::gdb::monitor;
use crate::gdb::regs::target_xml;
use crate::gdb::xml::{build_libraries_xml, build_memory_map_xml, write_xml_chunk};

// ─── Extended mode (vRun restart) ────────────────────────────────

impl<T: DebugTarget + Send + 'static> ExtendedMode for GdbTarget<T> {
    fn run(
        &mut self,
        _filename: Option<&[u8]>,
        args: gdbstub::target::ext::extended_mode::Args<'_, '_>,
    ) -> TargetResult<Pid, Self> {
        // See [`GdbTarget::backend_available`]: restarting while a replay
        // runs would block on the backend lock and stall the event loop.
        if !self.backend_available() {
            return Err(TargetError::NonFatal);
        }
        let mut backend = self.runner.lock();
        commands::handle_vrun(&mut *backend, &self.checkpoints, &mut args.into_iter());
        Ok(Pid::new(1).unwrap())
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
        let (first, _) = self.backend().lifetime();
        let _ = self.backend().goto(first);
        Ok(())
    }
}

// ─── Exec-file / auxv / process-info / libraries / memory-map ─────

impl<T: DebugTarget + Send + 'static> ExecFile for GdbTarget<T> {
    fn get_exec_file(
        &self,
        _pid: Option<Pid>,
        offset: u64,
        length: usize,
        buf: &mut [u8],
    ) -> TargetResult<usize, Self> {
        // The trace's first module is the main executable — the stub is
        // self-describing, mirroring how `dlv replay` learns the executable
        // for an rr trace.
        let path = match self.backend().modules().first() {
            Some(m) => m.name.clone(),
            None => return Err(TargetError::NonFatal),
        };
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
        let Some(base) = self.exe_module_base() else {
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

impl<T: DebugTarget + Send + 'static> ProcessInfo for GdbTarget<T> {
    fn process_info(
        &self,
        write_item: &mut dyn FnMut(&ProcessInfoResponse<'_>),
    ) -> Result<(), Self::Error> {
        write_item(&ProcessInfoResponse::Pid(Pid::new(1).unwrap()));
        write_item(&ProcessInfoResponse::Triple("x86_64-pc-windows-msvc"));
        write_item(&ProcessInfoResponse::Endianness(Endianness::Little));
        write_item(&ProcessInfoResponse::PointerSize(8));
        Ok(())
    }
}

// ─── Library list (qXfer:libraries:read) + Memory map (qXfer:memory-map:read) ─

impl<T: DebugTarget + Send + 'static> Libraries for GdbTarget<T> {
    fn get_libraries(
        &self,
        offset: u64,
        length: usize,
        buf: &mut [u8],
    ) -> TargetResult<usize, Self> {
        let xml = build_libraries_xml(&self.backend().modules());
        write_xml_chunk(xml.as_bytes(), offset, length, buf)
    }
}

impl<T: DebugTarget + Send + 'static> MemoryMap for GdbTarget<T> {
    fn memory_map_xml(
        &self,
        offset: u64,
        length: usize,
        buf: &mut [u8],
    ) -> TargetResult<usize, Self> {
        let xml = build_memory_map_xml(&self.backend().modules());
        write_xml_chunk(xml.as_bytes(), offset, length, buf)
    }
}

// ─── Section offsets (qOffsets) ────────────────────────────────────

impl<T: DebugTarget + Send + 'static> SectionOffsets for GdbTarget<T> {
    fn get_section_offsets(
        &mut self,
    ) -> Result<Offsets<<Self::Arch as gdbstub::arch::Arch>::Usize>, Self::Error> {
        // For PE executables the convention is to report the image base as
        // the text segment, with the data segment at the same address (or
        // omitted). gdb uses this to relocate symbols; combined with the
        // correct AT_ENTRY from qXfer:auxv:read (see `Auxv::get_auxv`), the
        // symbol load is exact.
        let modules = self.backend().modules();
        let main = modules.first().map(|m| m.base_addr).unwrap_or(0);
        Ok(Offsets::Segments {
            text_seg: main,
            data_seg: Some(main),
        })
    }
}

impl<T: DebugTarget + Send + 'static> ThreadExtraInfo for GdbTarget<T> {
    fn thread_extra_info(&self, tid: Tid, buf: &mut [u8]) -> Result<usize, Self::Error> {
        // Format: "UTID <unique> (OS 0x<os>); pos <seq>:<steps>".
        // The OS thread id is implicit (gdb already shows it as `Thread N.M`),
        // but we include it for self-containment. The trace position lets
        // the user locate the thread on the time axis at a glance.
        let info = self.backend().thread_info(tid.get() as u64);
        let s = match info {
            Some(i) => format!(
                "UTID {} (OS 0x{:x}); pos {:x}:{:x}",
                i.unique_id,
                tid.get(),
                i.current_position.sequence,
                i.current_position.steps
            ),
            None => format!("UTID ? (OS 0x{:x})", tid.get()),
        };
        let n = s.len().min(buf.len());
        buf[..n].copy_from_slice(&s.as_bytes()[..n]);
        Ok(n)
    }
}

// ─── Monitor (RR-style custom commands: monitor ttd …) ──────────
//
// gdb sends `monitor ttd <subcmd> [<args>]` for the user-facing
// introspection commands (`info`, `threads`, `module <addr>`,
// `events [n]`). Rendering lives in `super::monitor`; here we only hand it
// the backend and push the text back to the gdb console.

impl<T: DebugTarget + Send + 'static> MonitorCmd for GdbTarget<T> {
    fn handle_monitor_cmd(
        &mut self,
        cmd: &[u8],
        mut out: gdbstub::target::ext::monitor_cmd::ConsoleOutput<'_>,
    ) -> Result<(), Self::Error> {
        let s = std::str::from_utf8(cmd).unwrap_or("");
        // A monitor command can arrive while a replay is running. Taking the
        // backend lock here would park the event loop for the whole replay
        // and ^C could no longer be delivered — answer without the backend.
        // See [`GdbTarget::backend_available`].
        if !self.backend_available() {
            out.write_raw(b"target is running; retry when stopped\n");
            return Ok(());
        }
        let mut text = String::new();
        {
            let backend = self.backend();
            monitor::render(s, &*backend, &mut text);
        }
        out.write_raw(text.as_bytes());
        Ok(())
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
