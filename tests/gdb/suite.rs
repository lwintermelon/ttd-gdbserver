//! GDB RSP frontend integration tests over TCP with a configurable mock.
//!
//! These tests replay the *exact* packet sequence Delve's gdbserial client
//! sends (see delve/pkg/proc/gdbserial/gdbserver_conn.go handshake()), then
//! exercise the reversible-debugging packets: vCont / bc / bs / vRun /
//! qRRCmd checkpoints.
//!
//! No TTD required - the backend is an in-memory [`support::MockTarget`].
//! The split modules group tests by protocol area; shared TCP, handshake and
//! mock-backend code lives in the `ttd-gdbserver-test-support` dev-dependency
//! crate.
//!
//! This file is the crate root of the integration-test target `gdb`
//! (`cargo test --test gdb`). The target is declared explicitly in
//! `Cargo.toml` (`[[test]] path = "tests/gdb/suite.rs"`), so this descriptive
//! root file lives beside its modules in the modern `foo.rs` + `foo/` layout.

mod support;

mod breakpoints;
mod delve;
mod handshake;
mod introspection;
mod monitor;
mod resume;
mod reverse;
mod sessions;
