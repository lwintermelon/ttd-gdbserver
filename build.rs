use std::env;
use std::path::PathBuf;

fn main() {
    // The TTD SDK comes from the `Microsoft.TimeTravelDebugging.Apis` NuGet
    // package (headers + TTDReplay.lib). Point `TTD_SDK_DIR` at the package
    // directory; the include/lib paths are derived from it. See README.md.
    let sdk_dir = env::var("TTD_SDK_DIR").unwrap_or_else(|_| {
        panic!(
            "TTD_SDK_DIR is not set: point it at the Microsoft.TimeTravelDebugging.Apis \
             NuGet package directory (see README.md)"
        )
    });
    println!("cargo:rerun-if-env-changed=TTD_SDK_DIR");

    let ttd_include = PathBuf::from(&sdk_dir).join("sdk").join("include");
    let ttd_lib = PathBuf::from(&sdk_dir).join("sdk").join("lib").join("x64");

    // Compile C++ shim — cc knows MSVC/SDK include paths automatically
    cc::Build::new()
        .cpp(true)
        .std("c++20")
        .file("csrc/ttd_wrapper.cpp")
        .include(&ttd_include)
        .flag("/EHsc")
        .warnings(false)
        .compile("ttd_wrapper");

    println!("cargo:rustc-link-search=native={}", ttd_lib.display());
    println!("cargo:rustc-link-lib=dylib=TTDReplay");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());

    // ── Pass 1: TTD SDK enum bindings (C++ mode, from ttd_bindings.h) ──
    // ttd_bindings.h includes <TTD/IReplayEngine.h>.
    // bindgen parses it in C++ mode and extracts enum class values.
    // NOTE: In C++ mode, allowlist patterns match the C++ qualified name
    // ("TTD::Replay::EventType"), NOT the flattened Rust name
    // ("TTD_Replay_EventType").
    let sdk_bindings = bindgen::Builder::default()
        .header("csrc/ttd_bindings.h")
        .clang_arg(format!("-I{}", ttd_include.display()))
        .clang_arg("-x")
        .clang_arg("c++")
        .clang_arg("-fms-compatibility")
        .clang_arg("-fms-extensions")
        .clang_arg("-std=c++20")
        .allowlist_type("TTD::Replay::EventType")
        .allowlist_type("TTD::Replay::EventMask")
        .allowlist_type("TTD::Replay::DataAccessMask")
        .allowlist_type("TTD::Replay::ExceptionMask")
        .allowlist_var("TTD::Replay::EventType::.*")
        .allowlist_var("TTD::Replay::EventMask::.*")
        .allowlist_var("TTD::Replay::DataAccessMask::.*")
        .allowlist_var("TTD::Replay::ExceptionMask::.*")
        .derive_copy(true)
        .derive_debug(true)
        .derive_default(true)
        .derive_eq(true)
        .derive_partialeq(true)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("Unable to generate SDK bindings from ttd_bindings.h");

    sdk_bindings
        .write_to_file(out_path.join("ttd_sdk_bindings.rs"))
        .expect("Couldn't write SDK bindings!");

    // ── Pass 2: Wrapper API (C mode, from ttd_wrapper.h) ──
    // ttd_wrapper.h defines our C ABI: projection structs, opaque handles,
    // callbacks, and function declarations.
    let wrapper_bindings = bindgen::Builder::default()
        .header("csrc/ttd_wrapper.h")
        .allowlist_type("TtdPosition")
        .allowlist_type("TtdPositionRange")
        .allowlist_type("TtdThreadInfo")
        .allowlist_type("TtdActiveThreadInfo")
        .allowlist_type("TtdX64Regs")
        .allowlist_type("TtdReplayResult")
        .allowlist_type("TtdExceptionEvent")
        .allowlist_type("TtdQueryMemoryPolicy")
        .allowlist_type("TtdWatchpointCb")
        .allowlist_type("TtdProgressCb")
        .allowlist_type("TtdEngine")
        .allowlist_type("TtdCursor")
        .allowlist_function("ttd_.*")
        .derive_copy(true)
        .derive_debug(true)
        .derive_default(true)
        .derive_eq(true)
        .derive_partialeq(true)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("Unable to generate wrapper bindings from ttd_wrapper.h");

    wrapper_bindings
        .write_to_file(out_path.join("ttd_wrapper_bindings.rs"))
        .expect("Couldn't write wrapper bindings!");
}
