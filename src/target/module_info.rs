//! A loaded module (DLL/EXE) in the trace.

/// One module in the trace's load list.
///
/// `base_addr` is the runtime load address, so a client resolves a code
/// address to a module by finding the entry whose `[base_addr, base_addr +
/// size)` range contains it.
#[derive(Clone, Debug)]
pub struct ModuleInfo {
    pub base_addr: u64,
    pub size: u64,
    pub name: String,
}
