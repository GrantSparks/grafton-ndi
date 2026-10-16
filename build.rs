use std::{
    env, fs,
    path::{Path, PathBuf},
};

mod build_support;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=build_support.rs");
    for name in [
        "NDI_SDK_DIR",
        "LIBCLANG_PATH",
        "CLANG_PATH",
        "BINDGEN_EXTRA_CLANG_ARGS",
        "SDKROOT",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    let target = env::var("TARGET").expect("Cargo TARGET missing");
    let host = env::var("HOST").expect("Cargo HOST missing");
    let target_os = env::var("CARGO_CFG_TARGET_OS").expect("Cargo target OS missing");
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").expect("Cargo target architecture missing");
    build_support::library_subdirs(&target_os, &target_arch, &target)
        .unwrap_or_else(|error| panic!("{error}"));
    if target != host && env::var_os("NDI_SDK_DIR").is_none() {
        panic!("Cross-compiling {host} to {target} requires NDI_SDK_DIR pointing to the target SDK, plus target headers/sysroot configured through BINDGEN_EXTRA_CLANG_ARGS or the target C toolchain");
    }

    // Determine the base NDI SDK directory.
    let ndi_sdk_path = env::var("NDI_SDK_DIR").unwrap_or_else(|_| {
        if target_os == "macos" {
            // Try multiple possible NDI SDK locations on macOS
            let possible_paths = vec![
                "/Library/NDI SDK for macOS",
                "/Library/NDI SDK for Apple",
                "/Library/NDI 6 SDK",
                "/Library/NDI SDK",
                "/Library/NewTek/NDI SDK",
                "/Library/Application Support/NDI SDK for Apple",
                "/Applications/NDI SDK for Apple",
                "/Applications/NDI 6 SDK",
            ];

            for path in &possible_paths {
                if Path::new(path).exists() {
                    return path.to_string();
                }
            }

            // Default to the most common location
            "/Library/NDI SDK for macOS".to_string()
        } else if target_os == "linux" {
            // For Linux, try the Advanced SDK directory first.
            let advanced = "/usr/share/NDI Advanced SDK for Linux";
            let standard = "/usr/share/NDI SDK for Linux";
            if Path::new(advanced).exists() {
                advanced.to_string()
            } else {
                standard.to_string()
            }
        } else if target_os == "windows" {
            // NDI 6 SDK default installation path
            "C:\\Program Files\\NDI\\NDI 6 SDK".to_string()
        } else {
            panic!("Unsupported platform, please set NDI_SDK_DIR manually.");
        }
    });

    // Construct the include path and header file location.
    let ndi_include_path =
        if target_os == "windows" && Path::new(&ndi_sdk_path).join("Include").is_dir() {
            format!("{ndi_sdk_path}/Include")
        } else {
            format!("{ndi_sdk_path}/include")
        };

    // Create wrapper.h in OUT_DIR for bindgen
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR environment variable not set");
    let wrapper_path = Path::new(&out_dir).join("wrapper.h");

    // Check for the actual header file name (case variations)
    let header_file = {
        let possible_headers = vec![
            "Processing.NDI.Lib.h",
            "Processing.NDI.lib.h",
            "Processing.ndi.lib.h",
            "processing.ndi.lib.h",
        ];

        let mut found_header = None;
        for header in &possible_headers {
            let header_path = format!("{ndi_include_path}/{header}");
            if Path::new(&header_path).exists() {
                found_header = Some(header.to_string());
                break;
            }
        }

        found_header.unwrap_or_else(|| {
            panic!(
                "NDI header file not found in {ndi_include_path}. Searched for: {possible_headers:?}"
            )
        })
    };

    // Write wrapper.h content
    let wrapper_content = format!(
        r#"// wrapper.h - Include file for bindgen to properly generate NDI bindings

// DO NOT define NDI_NO_PROTOTYPES or NDILIB_NO_PROTOTYPES here!
// We need full prototypes so bindgen can generate bindings for all functions.

// Include the main NDI header
#include <{header_file}>
"#
    );
    fs::write(&wrapper_path, wrapper_content).expect("Failed to create wrapper.h");

    let main_header = wrapper_path.to_str().unwrap().to_string();

    let (library_dir, library_name, link_kind) =
        build_support::select_library(Path::new(&ndi_sdk_path), &target_os, &target_arch, &target)
            .unwrap_or_else(|error| panic!("{error}"));
    println!("cargo:rustc-link-search=native={}", library_dir.display());
    println!("cargo:rustc-link-lib={link_kind}={library_name}");
    println!("cargo:rerun-if-changed={}", library_dir.display());

    // Generate the bindings using bindgen.
    let bindings = bindgen::Builder::default()
        .header(main_header)
        .clang_arg(format!("-I{ndi_include_path}"))
        .clang_arg(format!("--target={target}"))
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .derive_default(true)
        .generate()
        .unwrap_or_else(|error| panic!("Unable to generate NDI bindings for {target}: {error}. For cross builds, configure target C headers and sysroot via BINDGEN_EXTRA_CLANG_ARGS."));

    // Write the bindings to the $OUT_DIR/ndi_lib.rs file.
    let out_path =
        PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR environment variable not set"));
    let bindings_path = out_path.join("ndi_lib.rs");
    bindings
        .write_to_file(&bindings_path)
        .expect("Couldn't write bindings!");
}
