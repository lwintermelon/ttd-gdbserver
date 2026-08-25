use std::env;
use std::path::PathBuf;

fn main() {
    let sdk_dir = sdk_dir();
    declare_test_cfgs();
    build_shim(&sdk_dir);
    write_bindings(&sdk_dir);
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
///
/// The variables come from the environment — `mise.toml`'s `[env]` block for
/// this checkout, the caller's shell or CI otherwise. No path is baked in here:
/// where a recording lives is configuration.
fn declare_test_cfgs() {
    println!("cargo:rerun-if-env-changed=TTD_TRACE_PATH");
    println!("cargo:rerun-if-env-changed=TTD_FAULT_TRACE_PATH");
    // Declare the custom cfg names so rustc type-checks every use of them
    // (an unexpected_cfgs warning would fire if they were misspelled).
    println!("cargo::rustc-check-cfg=cfg(has_ttd_trace)");
    println!("cargo::rustc-check-cfg=cfg(has_ttd_fault_trace)");
    // An empty value is treated as unset: otherwise a shell that exports
    // `TTD_TRACE_PATH=` would enable the cfg and make the trace tests fail on
    // an empty path instead of being reported as ignored.
    let present = |var: &str| env::var_os(var).is_some_and(|value| !value.is_empty());
    if present("TTD_TRACE_PATH") {
        println!("cargo:rustc-cfg=has_ttd_trace");
    }
    if present("TTD_FAULT_TRACE_PATH") {
        println!("cargo:rustc-cfg=has_ttd_fault_trace");
    }
}

/// Compile the C++ shim and link the replay DLL.
fn build_shim(sdk_dir: &str) {
    println!("cargo:rerun-if-changed=csrc/ttd_sys.hpp");
    println!("cargo:rerun-if-changed=csrc/ttd_sys.cpp");

    let ttd_include = PathBuf::from(sdk_dir).join("sdk").join("include");
    let ttd_lib = PathBuf::from(sdk_dir).join("sdk").join("lib").join("x64");

    // cc knows MSVC and the Windows SDK include paths automatically.
    cc::Build::new()
        .cpp(true)
        .std("c++20")
        .file("csrc/ttd_sys.cpp")
        .include(&ttd_include)
        .flag("/EHsc")
        .warnings(false)
        .compile("ttd_sys");

    println!("cargo:rustc-link-search=native={}", ttd_lib.display());
    println!("cargo:rustc-link-lib=dylib=TTDReplay");
}

/// Generate the bindings from the shim header, into `OUT_DIR` where
/// `src/ttd_sys/mod.rs` `include!`s them.
fn write_bindings(sdk_dir: &str) {
    let ttd_include = PathBuf::from(sdk_dir).join("sdk").join("include");

    // Parsed in C++ mode because the signatures use the SDK's own types.
    let bindings = bindgen::Builder::default()
        .header("csrc/ttd_sys.hpp")
        .clang_arg(format!("-I{}", ttd_include.display()))
        .clang_arg("-x")
        .clang_arg("c++")
        .clang_arg("-fms-compatibility")
        .clang_arg("-fms-extensions")
        .clang_arg("-std=c++20")
        // The structs cross the FFI by value, so keep the compile-time
        // size/alignment/offset checks.
        .layout_tests(true)
        .allowlist_function("ttd_sys::.*")
        .allowlist_type("ttd_sys::.*")
        .opaque_type("std::.*")
        .opaque_type("TTD::Replay::UniqueReplayEngine")
        .opaque_type("TTD::Replay::UniqueCursor")
        .allowlist_type("TTD::SystemInfo")
        .allowlist_type("TTD::ThreadId")
        .allowlist_type("TTD::Replay::Position")
        .allowlist_type("TTD::Replay::PositionRange")
        .allowlist_type("TTD::Replay::ThreadInfo")
        .allowlist_type("TTD::Replay::Module")
        .allowlist_type("TTD::Replay::ExceptionEvent")
        .allowlist_type("TTD::Replay::MemoryRange")
        .allowlist_type("TTD::Replay::RegisterContext")
        .allowlist_type("TTD::Replay::ExtendedRegisterContext")
        .allowlist_type("TTD::Replay::ActiveThreadInfo")
        .allowlist_type("TTD::Replay::MemoryWatchpointData")
        .allowlist_type("TTD::Replay::ICursorView::ReplayResult")
        .allowlist_type("TTD::Replay::IndexBuildProgressType")
        .allowlist_type("TTD::Replay::IndexBuildProgressCallback")
        .allowlist_type("TTD::Replay::EventType")
        .allowlist_type("TTD::Replay::EventMask")
        .allowlist_type("TTD::Replay::ExceptionMask")
        .allowlist_type("TTD::Replay::DataAccessType")
        .allowlist_type("TTD::Replay::DataAccessMask")
        .allowlist_type("TTD::Replay::QueryMemoryPolicy")
        .allowlist_type("TTD::Replay::IndexStatus")
        .allowlist_type("AMD64_CONTEXT")
        .allowlist_type("AMD64_XMM_SAVE_AREA32")
        .allowlist_type("AVX_EXTENDED_CONTEXT")
        .allowlist_type("X86_NT5_CONTEXT")
        .allowlist_type("ARM64_CONTEXT")
        .allowlist_type("ARM64_NEON128")
        .allowlist_var("TTD::Replay::.*")
        .derive_copy(true)
        .derive_debug(true)
        .derive_default(true)
        .derive_eq(true)
        .derive_hash(true)
        .derive_partialeq(true)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("Unable to generate bindings from csrc/ttd_sys.hpp");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("ttd_sys_bindings.rs");

    // Rewriting an unchanged file would make every build look dirty to rustc
    // and recompile this crate for nothing.
    let contents =
        format!("// Generated by `build.rs` from `csrc/ttd_sys.hpp` — do not edit.\n{bindings}");
    let unchanged = std::fs::read_to_string(&out_path).is_ok_and(|existing| existing == contents);
    if !unchanged {
        std::fs::write(&out_path, contents).expect("Couldn't write ttd_sys bindings");
    }
}
