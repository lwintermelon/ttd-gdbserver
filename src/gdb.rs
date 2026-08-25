//! GDB Remote Serial Protocol frontend.
//!
//! Built on the `DebugTarget` trait. GDB RSP is a general mechanism (like
//! rr): GDB, LLDB and Delve all speak it and support reverse debugging, so
//! any of them can attach. The protocol subset is chosen for compatibility
//! with Delve's `pkg/proc/gdbserial` client — the same backend Delve uses
//! for `rr` on Linux — the first verified client (Go debugging works
//! end-to-end). See `doc/delve-integration.md`.
//!
//! The RSP engine is the `gdbstub` crate (git dep, the `ttd-dbg` branch of
//! `github.com/lwintermelon/gdbstub`, carrying a tiny `UnknownPacket` target
//! hook so Delve-specific packets like `qRRCmd` can be
//! handled while gdbstub owns all standard protocol machinery):
//!
//! - `regs.rs` — custom x86-64 `Arch`: 59-register layout (GPRs + x87/SSE +
//!   `gs_base`/`fs_base`), `g` buffer, target description embedded via
//!   `include_str!` (GDB's `gdb/features/i386/` files) and served through
//!   `TargetDescriptionXmlOverride`.
//! - `target.rs` + `target/{base,breakpoints,extended,description,memory_map,
//!   thread_info,monitor_cmd,unknown}.rs` —
//!   `GdbTarget<T: DebugTarget>`: gdbstub `Target` impl mapping
//!   registers/memory/threads/breakpoints/forward+reverse resume onto the
//!   backend, plus the packets gdbstub has no native extension trait for
//!   (`UnknownPacket`: rr-style `qRRCmd`, `qGetTLSAddr`, `vCtrlC`,
//!   handshake declines). The split is by protocol area; the shared
//!   test-only support lives in `target/test_support.rs` and each area's
//!   tests are inline (`#[cfg(test)] mod tests`), per the repo-wide
//!   convention.
//! - `mapping.rs` — pure wire mapping: stop reason → T packet, TTD watch
//!   access bitmask → GDB watch kind, Windows exception code → signal, and
//!   stop-reply thread selection.
//! - `commands.rs` — Delve-specific `qRRCmd`/`vRun` command handling over a
//!   small `CommandTarget` view, unit-tested with a recording stub.
//! - `resume.rs` — `ReplayOp`/`ResumeState`: the pending `vCont` action
//!   state machine (priority, clear/consume, empty-range normalization).
//! - `runner.rs` — `TargetRunner`, the replay-worker / stop-reason /
//!   interrupt concurrency core, unit-tested inline without TCP.
//! - `session.rs` — TCP server + `run_blocking` event loop (replay runs on a
//!   worker thread so ^C can interrupt it).
//!
//! The remaining modules hold the pieces of the protocol that are pure
//! functions or self-contained state, so they are unit-tested directly
//! instead of through a socket: `packet.rs` (qRRCmd/qGetTLSAddr parsing and
//! the RSP hex codec), `xml.rs` (qXfer payloads + chunking),
//! `checkpoint.rs` (rr-style checkpoints) and `monitor.rs`
//! (`monitor ttd …` rendering, driven by the `TraceView` trait).
//!
//! All modules use the modern `foo.rs` + `foo/` layout; there are no
//! `mod.rs` files anywhere in the repository.

pub mod checkpoint;
pub mod commands;
pub mod mapping;
pub mod monitor;
pub mod packet;
pub mod regs;
pub mod resume;
pub mod runner;
pub mod session;
pub mod target;
pub mod xml;

mod pe;

pub use session::run_gdb_server;
