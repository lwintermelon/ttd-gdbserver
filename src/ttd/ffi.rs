//! Auto-generated FFI declarations from `csrc/ttd_wrapper.h` via bindgen.
//!
//! Our C wrapper API: projection types, opaque handles, callback typedefs,
//! and all `extern "C"` function declarations.
//!
//! Enum types and constants are in `super::bindings` (generated from `ttd_bindings.h`).
//!
//! Regenerate by running `cargo build`.
#![allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    clippy::all
)]
include!(concat!(env!("OUT_DIR"), "/ttd_wrapper_bindings.rs"));
