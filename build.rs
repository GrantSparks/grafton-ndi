extern crate bindgen;

use std::env;
use std::path::PathBuf;

fn main() {
    // Base path to the NDI SDK from the environment variable or default based on the platform
    let ndi_sdk_path = env::var("NDI_SDK_DIR").unwrap_or_else(|_| {
        if cfg!(target_os = "macos") {
            "/Library/NDI SDK for Apple".to_string()
        } else if cfg!(target_os = "linux") {
            "/usr/share/NDI SDK for Linux".to_string()
        } else if cfg!(target_os = "windows") {
            "C:\\Program Files\\NDI SDK for Windows".to_string()
        } else {
            panic!("Unsupported platform, please set NDI_SDK_DIR manually.");
        }
    });

    // Paths to the include and main header file
    let ndi_include_path = format!("{}/include", ndi_sdk_path);
    let main_header = format!("{}/Processing.NDI.Lib.h", ndi_include_path);

    // Determine the library name and linking type based on the platform
    let (lib_name, link_type) = if cfg!(target_os = "macos") {
        // For Unix-like systems, use the shared library `libndi.so`
        ("ndi", "dylib") // Use "dylib" for dynamic linking
    } else if cfg!(target_os = "linux") {
        // For Unix-like systems, use the shared library `libndi.so`
        ("ndi", "dylib") // Use "dylib" for dynamic linking
    } else if cfg!(target_os = "windows") {
        // For Windows systems, use the specific x86/x64 libraries with static linking
        let target = env::var("TARGET").expect("TARGET environment variable not set");
        if target.contains("x86_64") {
            ("Processing.NDI.Lib.x64", "static")
        } else {
            ("Processing.NDI.Lib.x86", "static")
        }
    } else {
        panic!("Unsupported platform");
    };

    // On Windows, add the specific library directory path
    if cfg!(windows) {
        let target = env::var("TARGET").expect("TARGET environment variable not set");
        let lib_subdir = if target.contains("x86_64") {
            "x64"
        } else {
            "x86"
        };
        let lib_path = format!("{}\\lib\\{}", ndi_sdk_path, lib_subdir);

        // Inform cargo about the search path for the linker and the library to link against
        println!("cargo:rustc-link-search=native={}", lib_path);
    }

    // Inform cargo about the library to link against
    println!("cargo:rustc-link-lib={}={}", link_type, lib_name);

    // Generate the bindings
    let bindings = bindgen::Builder::default()
        .header(main_header)
        .clang_arg(format!("-I{}", ndi_include_path))
        .derive_default(true)
        .generate()
        .expect("Unable to generate bindings");

    // Write the bindings to the $OUT_DIR/ndi_lib.rs file
    let out_path =
        PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR environment variable not set"));
    bindings
        .write_to_file(out_path.join("ndi_lib.rs"))
        .expect("Couldn't write bindings!");
}
