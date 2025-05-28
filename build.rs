extern crate bindgen;

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    // Determine the base NDI SDK directory.
    let ndi_sdk_path = env::var("NDI_SDK_DIR").unwrap_or_else(|_| {
        if cfg!(unix) {
            // For Unix, try the Advanced SDK directory first.
            let advanced = "/usr/share/NDI Advanced SDK for Linux";
            let standard = "/usr/share/NDI SDK for Linux";
            if Path::new(advanced).exists() {
                advanced.to_string()
            } else {
                standard.to_string()
            }
        } else if cfg!(windows) {
            // NDI 6 SDK default installation path
            "C:\\Program Files\\NDI\\NDI 6 SDK".to_string()
        } else {
            panic!("Unsupported platform, please set NDI_SDK_DIR manually.");
        }
    });

    // Determine if we're using the Advanced SDK (only relevant on Unix).
    let is_advanced = if cfg!(unix) {
        ndi_sdk_path.to_lowercase().contains("advanced")
    } else {
        false
    };

    // Construct the include path and header file location.
    let ndi_include_path = format!("{}/include", ndi_sdk_path);

    // Create wrapper.h in OUT_DIR for bindgen
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR environment variable not set");
    let wrapper_path = Path::new(&out_dir).join("wrapper.h");

    // Write wrapper.h content
    let wrapper_content = r#"// wrapper.h - Include file for bindgen to properly generate NDI bindings

// DO NOT define NDI_NO_PROTOTYPES or NDILIB_NO_PROTOTYPES here!
// We need full prototypes so bindgen can generate bindings for all functions,
// including NDIlib_send_set_video_async_completion (available in NDI Advanced SDK)

// Include the main NDI header
#include <Processing.NDI.Lib.h>
"#;
    std::fs::write(&wrapper_path, wrapper_content).expect("Failed to create wrapper.h");

    let main_header = wrapper_path.to_str().unwrap().to_string();

    // Determine the library name and linking type based on the platform.
    let (lib_name, link_type) = if cfg!(unix) {
        if is_advanced {
            ("ndi_advanced", "dylib")
        } else {
            ("ndi", "dylib")
        }
    } else if cfg!(windows) {
        let target = env::var("TARGET").expect("TARGET environment variable not set");
        if target.contains("x86_64") {
            ("Processing.NDI.Lib.x64", "static")
        } else {
            ("Processing.NDI.Lib.x86", "static")
        }
    } else {
        panic!("Unsupported platform");
    };

    // Add library directory path for both platforms.
    if cfg!(windows) {
        let target = env::var("TARGET").expect("TARGET environment variable not set");
        let lib_subdir = if target.contains("x86_64") {
            "x64"
        } else {
            "x86"
        };
        let lib_path = format!("{}\\lib\\{}", ndi_sdk_path, lib_subdir);
        println!("cargo:rustc-link-search=native={}", lib_path);
    } else if cfg!(unix) {
        // For Linux, add the library search path
        let lib_path = format!("{}/lib/x86_64-linux-gnu", ndi_sdk_path);
        println!("cargo:rustc-link-search=native={}", lib_path);
    }

    // Inform Cargo about the library to link against.
    println!("cargo:rustc-link-lib={}={}", link_type, lib_name);

    // Generate the bindings using bindgen.
    let bindings = bindgen::Builder::default()
        .header(main_header)
        .clang_arg(format!("-I{}", ndi_include_path))
        .derive_default(true)
        .generate()
        .expect("Unable to generate bindings");

    // Write the bindings to the $OUT_DIR/ndi_lib.rs file.
    let out_path =
        PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR environment variable not set"));
    bindings
        .write_to_file(out_path.join("ndi_lib.rs"))
        .expect("Couldn't write bindings!");
}
