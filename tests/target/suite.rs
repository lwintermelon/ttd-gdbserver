//! Integration tests for the target layer: `TtdProcess` via the
//! [`DebugTarget`] trait.
//!
//! These test the target layer directly (no protocol frontend), complementing
//! the full-chain GDB RSP tests in `tests/gdb_ttd.rs`. Shared helpers live in
//! [`support`]; the test bodies are grouped by concern.
//!
//! Requires `TTD_TRACE_PATH`: without it build.rs does not emit the
//! `has_ttd_trace` cfg and every test here is tagged `#[ignore]`, so libtest
//! reports them as skipped (not passing).
//!
//! The target is declared explicitly in `Cargo.toml`
//! (`[[test]] path = "tests/target/suite.rs"`), so this root lives beside its
//! modules in the modern `foo.rs` + `foo/` layout.

mod support;

mod breakpoints;
mod execution;
mod position;
mod range;
mod threads;
