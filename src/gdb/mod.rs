//! GDB Remote Serial Protocol frontend.
//!
//! Built on the `DebugTarget` trait. GDB RSP is a general mechanism (like
//! rr): GDB, LLDB and Delve all speak it and support reverse debugging, so
//! any of them can attach. The protocol subset is chosen for compatibility
//! with Delve's `pkg/proc/gdbserial` client — the same backend Delve uses
//! for `rr` on Linux — the first verified client (Go debugging works
//! end-to-end). See `doc/delve-integration.md`.
//!
//! The RSP engine is the `gdbstub` crate (a local checkout with a tiny
//! `UnknownPacket` target hook so Delve-specific packets like `qRRCmd` can be
//! handled while gdbstub owns all standard protocol machinery):
//!
//! - `regs.rs` — custom x86-64 `Arch`: 59-register layout (GPRs + x87/SSE +
//!   `gs_base`/`fs_base`), `g` buffer, target description embedded via
//!   `include_str!` (GDB's `gdb/features/i386/` files) and served through
//!   `TargetDescriptionXmlOverride`.
//! - `target.rs` — `GdbTarget<T: DebugTarget>`: gdbstub `Target` impl mapping
//!   registers/memory/threads/breakpoints/forward+reverse resume onto the
//!   backend, plus Delve-specific packets via `UnknownPacket`.
//! - `session.rs` — TCP server + `run_blocking` event loop (replay runs on a
//!   worker thread so ^C can interrupt it).

pub mod regs;
pub mod session;
pub mod target;

pub use session::run_gdb_server;
