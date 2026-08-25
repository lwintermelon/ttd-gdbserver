// Modern FFI hygiene: every operation inside an `unsafe fn` must carry its
// own explicit `unsafe` block with a safety justification.
#![deny(unsafe_op_in_unsafe_fn)]

// The TTD Replay API only exists on Windows; fail fast with a clear message
// instead of a wall of missing-binding errors from bindgen/FFI.
#[cfg(not(windows))]
compile_error!("ttd-gdbserver builds only on Windows: it links the WinDbg TTD Replay DLL");

pub mod gdb;
pub mod target;
pub mod ttd;
