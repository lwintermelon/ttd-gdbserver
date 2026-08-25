use std::env;
use std::path::PathBuf;

fn main() {
    let sdk_dir = sdk_dir();
    declare_test_cfgs();
    build_cxx_bridge(&sdk_dir);
}

/// The SDK comes from the `Microsoft.TimeTravelDebugging.Apis` `NuGet` package
/// (headers + `TTDReplay.lib`). `TTD_SDK_DIR` points at the package directory;
/// the include/lib paths are derived from it.
fn sdk_dir() -> String {
    let sdk_dir = env::var("TTD_SDK_DIR")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            panic!(
                "TTD_SDK_DIR is not set: point it at the Microsoft.TimeTravelDebugging.Apis \
                 NuGet package directory"
            )
        });
    println!("cargo:rerun-if-env-changed=TTD_SDK_DIR");
    sdk_dir
}

/// `has_ttd_trace` gates the trace-dependent integration tests: with no
/// `TTD_TRACE_PATH` (and no `TTD_FAULT_TRACE_PATH`), the suites'
/// `#[cfg_attr(not(has_ttd_trace), ignore)]` tags expand to `#[ignore]`, so
/// libtest reports them as skipped.
fn declare_test_cfgs() {
    println!("cargo:rerun-if-env-changed=TTD_TRACE_PATH");
    println!("cargo:rerun-if-env-changed=TTD_FAULT_TRACE_PATH");
    println!("cargo::rustc-check-cfg=cfg(has_ttd_trace)");
    println!("cargo::rustc-check-cfg=cfg(has_ttd_fault_trace)");
    let present = |var: &str| env::var_os(var).is_some_and(|value| !value.is_empty());
    if present("TTD_TRACE_PATH") {
        println!("cargo:rustc-cfg=has_ttd_trace");
    }
    if present("TTD_FAULT_TRACE_PATH") {
        println!("cargo:rustc-cfg=has_ttd_fault_trace");
    }
}

/// Compile the C++ wrapper and the cxx bridge, then link the replay DLL.
fn build_cxx_bridge(sdk_dir: &str) {
    println!("cargo:rerun-if-changed=src/ttd/cpp/ttd_cxx.hpp");
    println!("cargo:rerun-if-changed=src/ttd/cpp/ttd_cxx.cpp");
    println!("cargo:rerun-if-changed=src/ttd/ffi.rs");

    let ttd_include = PathBuf::from(sdk_dir).join("sdk").join("include");
    let ttd_lib = PathBuf::from(sdk_dir).join("sdk").join("lib").join("x64");

    cxx_build::bridge("src/ttd/ffi.rs")
        .include("src/ttd/cpp")
        .include(&ttd_include)
        .std("c++20")
        .file("src/ttd/cpp/ttd_cxx.cpp")
        .compile("ttd_cxx");

    println!("cargo:rustc-link-search=native={}", ttd_lib.display());
    println!("cargo:rustc-link-lib=dylib=TTDReplay");
}
