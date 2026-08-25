//! Packets gdbstub has no native extension trait for, answered through the
//! `UnknownPacket` hook.
//!
//! Everything here is generic reverse-debugging / RSP surface — no packet in
//! this file belongs to one client:
//!
//! - `qRRCmd` — rr's reverse-debugging command vocabulary (`when`,
//!   `checkpoint`, `info checkpoints`, `delete checkpoint`), also spoken by
//!   Delve's gdbserial client; parsed and handled in `commands.rs`.
//! - `qGetTIBAddr` / `qGetTLSAddr` — Windows GDB asks for the TIB (TEB)
//!   first and derives TLS from it; both return the thread's TEB.
//! - `QPassSignals` / `QProgramSignals` — standard RSP; acknowledged and
//!   ignored because replay is read-only (the recorded signal stream is the
//!   only one that matters).
//! - `vCtrlC` — the extended-remote ^C form (LLDB sends it instead of a bare
//!   0x03); aborts the in-flight replay before acking.
//! - `QThreadSuffixSupported` / `QListThreadsInStopReply` — declined with the
//!   protocol's explicit "unsupported" (empty) reply, so the client falls
//!   back to plain `H` thread selection / `qfThreadInfo`.
//!
//! Client-specific *decisions* (which of these the verified clients parse
//! literally, and why) live in `doc/delve-integration.md`, not here — this
//! module is the generic server side of the protocol.

use gdbstub::target::ext::unknown_packet::{ConsoleOutput, UnknownPacket};

use crate::gdb::commands;
use crate::gdb::packet::{encode_hex, parse_tid_from_qpacket};
use crate::target::DebugTarget;

use super::GdbTarget;

impl<T: DebugTarget + Send + 'static> GdbTarget<T> {
    /// Answer `qRRCmd` (`when` / checkpoints) with rr's hex-encoded text
    /// reply.
    fn answer_qrrcmd(&mut self, args: &str, out: &mut ConsoleOutput<'_>) {
        // A qRRCmd may arrive while a replay is running; the empty reply is
        // rr's "unknown command", which the client already handles.
        let Some(backend) = self.runner.try_lock() else {
            return;
        };
        // qRRCmd responses are hex-encoded text (Delve decodes pairs).
        let resp = commands::handle_qrrcmd(&*backend, &mut self.checkpoints, args);
        out.write_raw(encode_hex(resp.as_bytes()).as_bytes());
    }

    /// Answer `qGetTIBAddr` / `qGetTLSAddr` with the thread's TEB address.
    ///
    /// GDB's PE target asks `qGetTIBAddr:<tid>` first and derives the TLS
    /// address from the TIB (TEB) itself. `qGetTLSAddr` is the generic RSP
    /// form; both map to the same TEB on Windows.
    fn answer_tib_addr(&mut self, args: &str, out: &mut ConsoleOutput<'_>) {
        let Some(backend) = self.runner.try_lock() else {
            out.write_raw(b"E01");
            return;
        };
        let os_tid = parse_tid_from_qpacket(args.split(',').next().unwrap_or(""));
        let teb = os_tid
            .and_then(|tid| backend.thread_info(tid))
            .map_or(0, |info| info.teb);
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

        if let Some(args) = s
            .strip_prefix("qGetTIBAddr:")
            .or_else(|| s.strip_prefix("qGetTLSAddr:"))
        {
            self.answer_tib_addr(args, &mut out);
            return Ok(());
        }

        // _M (allocate memory), jGetLoadedDynamicLibrariesInfos,
        // qMemoryRegionInfo, qXfer:siginfo, ... — unsupported.
        Ok(())
    }
}
