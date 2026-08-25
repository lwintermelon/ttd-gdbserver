//! Auto-generated TTD enum bindings from `csrc/ttd_bindings.h` via bindgen.
//!
//! Contains C enum type aliases and constants mirroring TTD SDK's C++ enum class
//! values (EventType, DataAccessMask, EventMask, ExceptionMask).
//!
//! These are verified against the TTD SDK at C++ compile time via `static_assert`
//! in `ttd_wrapper.cpp`.
//!
//! Regenerate by running `cargo build`.
#![allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    clippy::all
)]
include!(concat!(env!("OUT_DIR"), "/ttd_sdk_bindings.rs"));
