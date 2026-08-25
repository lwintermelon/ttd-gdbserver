//! Delve-specific packets handled through gdbstub's `UnknownPacket` hook.
//!
//! `qRRCmd` (when/checkpoints), `qGetTLSAddr`, `QPassSignals` /
//! `QProgramSignals`, `vCtrlC`, and the explicitly unsupported queries that
//! every client probes during handshake.

use gdbstub::target::ext::unknown_packet::{ConsoleOutput, UnknownPacket};

use crate::gdb::commands;
use crate::gdb::packet::{encode_hex, parse_tid_from_qpacket};
use crate::target::DebugTarget;

use super::GdbTarget;

impl<T: DebugTarget + Send + 'static> GdbTarget<T> {
    /// Answer `qRRCmd` (`when` / checkpoints) with rr's hex-encoded text
    /// reply.
    fn answer_qrrcmd(&mut self, args: &str, out: &mut ConsoleOutput<'_>) {
        // A qRRCmd may arrive while a replay is running. Blocking on the
        // backend lock would park the event loop for the whole replay, so
        // answer with rr's "unknown command" reply instead.
        if !self.backend_available() {
            return;
        }
        // qRRCmd responses are hex-encoded text (Delve decodes pairs).
        let backend = self.runner.lock();
        let resp = commands::handle_qrrcmd(&*backend, &mut self.checkpoints, args);
        out.write_raw(encode_hex(resp.as_bytes()).as_bytes());
    }

    /// Answer `qGetTLSAddr` with the thread's TEB address, or `E01`.
    ///
    /// Format: `qGetTLSAddr:p<pid>.<tid>,<offset>,<lm>`. On Windows the TLS
    /// address is the TEB; the offset is a TLS-index used by Linux glibc and
    /// `lm` is the Linux link map, so both are ignored — the TEB base is what
    /// the kernel/runtime would use on Windows. Answering while a replay runs
    /// would block the event loop, so that reports the error the client
    /// already handles (see [`GdbTarget::backend_available`]).
    fn answer_qget_tls_addr(&mut self, args: &str, out: &mut ConsoleOutput<'_>) {
        if !self.backend_available() {
            out.write_raw(b"E01");
            return;
        }
        let os_tid = parse_tid_from_qpacket(args.split(',').next().unwrap_or(""));
        let teb = os_tid
            .and_then(|tid| self.backend().thread_info(tid))
            .map(|info| info.teb)
            .unwrap_or(0);
        if teb == 0 {
            out.write_raw(b"E01");
        } else {
            out.write_raw(format!("{teb:x}").as_bytes());
        }
    }
}

// ─── Unknown packets (Delve-specific) ────────────────────────────

impl<T: DebugTarget + Send + 'static> UnknownPacket for GdbTarget<T> {
    fn handle_unknown_packet(
        &mut self,
        body: &[u8],
        mut out: ConsoleOutput<'_>,
    ) -> Result<(), Self::Error> {
        let s = std::str::from_utf8(body).unwrap_or("");
        log::debug!("gdb custom <- {s}");

        if let Some(args) = s.strip_prefix("qRRCmd") {
            self.answer_qrrcmd(args, &mut out);
            return Ok(());
        }

        // Unsupported -> Delve falls back to plain `H` thread selection /
        // qfThreadInfo. Writing nothing means "unsupported".
        if s == "QThreadSuffixSupported" || s == "QListThreadsInStopReply" {
            return Ok(());
        }

        // `QPassSignals` / `QProgramSignals`: the client enumerates which
        // signals it wants delivered. We acknowledge; replay is read-only and
        // the recorded signal stream is the only one we care about.
        if s.starts_with("QPassSignals") || s.starts_with("QProgramSignals") {
            out.write_raw(b"OK");
            return Ok(());
        }

        // `vCtrlC` is extended-remote's preferred form of ^C (LLDB sends it).
        // gdbstub has no native vCtrlC handling, so it lands here as an
        // unknown packet and the BlockingEventLoop's `on_interrupt` path
        // never runs — abort the replay explicitly before acking.
        if s == "vCtrlC" {
            self.interrupt_replay();
            out.write_raw(b"OK");
            return Ok(());
        }

        if let Some(args) = s.strip_prefix("qGetTLSAddr:") {
            self.answer_qget_tls_addr(args, &mut out);
            return Ok(());
        }

        // _M (allocate memory), jGetLoadedDynamicLibrariesInfos,
        // qMemoryRegionInfo, qXfer:siginfo, ... — unsupported.
        Ok(())
    }
}
