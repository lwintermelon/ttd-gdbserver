//! Raw bindings to the TTD Replay API.
//!
//! The generated half is `csrc/ttd_sys.hpp` read by bindgen: the SDK's own
//! structs and enums, plus the declarations of a C++ surface that forwards to
//! it. Nothing here interprets the values it returns — that is the wrapper's
//! job (`src/ttd/`).
//!
//! The output lands in `OUT_DIR`; `cargo build` prints the exact path as
//! `target/<profile>/build/ttd-gdbserver-<hash>/out/ttd_sys_bindings.rs`, which
//! is where to look when a generated name or layout needs checking.

#![allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    clippy::all,
    clippy::pedantic
)]
// bindgen's method wrappers forward to `unsafe fn`s; the outer `unsafe fn`
// already carries the contract.
#![allow(unsafe_op_in_unsafe_fn)]

include!(concat!(env!("OUT_DIR"), "/ttd_sys_bindings.rs"));
