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
//! - `target.rs` + `target/{base,breakpoints,inspect,unknown}.rs` —
//!   `GdbTarget<T: DebugTarget>`: gdbstub `Target` impl mapping
//!   registers/memory/threads/breakpoints/forward+reverse resume onto the
//!   backend, plus Delve-specific packets via `UnknownPacket`. The split is
//!   by protocol area; stub-backed tests live in `target/tests.rs`.
//! - `mapping.rs` — pure wire mapping: stop reason → T packet, TTD watch
//!   access bitmask → GDB watch kind, Windows exception code → signal, and
//!   stop-reply thread selection.
//! - `commands.rs` — Delve-specific `qRRCmd`/`vRun` command handling over a
//!   small `CommandTarget` view, unit-tested with a recording stub.
//! - `resume.rs` — `ReplayOp`/`ResumeState`: the pending `vCont` action
//!   state machine (priority, clear/consume, empty-range normalization).
//! - `runner.rs` + `runner/tests.rs` — `TargetRunner`, the replay-worker /
//!   stop-reason / interrupt concurrency core.
//! - `session.rs` — TCP server + `run_blocking` event loop (replay runs on a
//!   worker thread so ^C can interrupt it).
//!
//! The remaining modules hold the pieces of the protocol that are pure
//! functions or self-contained state, so they are unit-tested directly
//! instead of through a socket: `packet.rs` (qRRCmd/qGetTLSAddr parsing and
//! the RSP hex codec), `xml.rs` (qXfer payloads + chunking),
//! `checkpoint.rs` (rr-style checkpoints) and `monitor.rs`
//! (`monitor ttd …` rendering, unit tests in `monitor/tests.rs`).
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

pub use session::run_gdb_server;
