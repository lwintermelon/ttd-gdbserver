// Modern FFI hygiene: every operation inside an `unsafe fn` must carry its
// own explicit `unsafe` block with a safety justification.
#![deny(unsafe_op_in_unsafe_fn)]
// `unsafe` is confined to the FFI layer: everything outside `ttd` must be safe
// code, so a raw pointer can only appear where its safety contract is written
// down. The `ttd` module opts back in below.
#![deny(unsafe_code)]

// The TTD Replay API only exists on Windows; fail fast with a clear message
// instead of a wall of missing-binding errors from bindgen/FFI.
#[cfg(not(windows))]
compile_error!("ttd-gdbserver builds only on Windows: it links the WinDbg TTD Replay DLL");

// The register file, projection structs, and the `u32` -> `usize` FFI
// conversions all assume the 64-bit Windows ABI (x86_64 is the only trace
// architecture TTD records here).
#[cfg(not(target_pointer_width = "64"))]
compile_error!("ttd-gdbserver targets 64-bit Windows only");

pub mod gdb;
pub mod target;
#[allow(unsafe_code)]
pub mod ttd;
#[allow(unsafe_code)]
pub mod ttd_sys;
