extern crate bindgen;

use std::{
    env,
    path::{Path, PathBuf},
};

fn main() {
    // Register custom cfg for conditional compilation based on SDK capabilities
    println!("cargo:rustc-check-cfg=cfg(has_async_completion_callback)");

    // Determine the base NDI SDK directory.
    let ndi_sdk_path = env::var("NDI_SDK_DIR").unwrap_or_else(|_| {
        if cfg!(target_os = "macos") {
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
        } else if cfg!(target_os = "linux") {
            // For Linux, try the Advanced SDK directory first.
            let advanced = "/usr/share/NDI Advanced SDK for Linux";
            let standard = "/usr/share/NDI SDK for Linux";
            if Path::new(advanced).exists() {
                advanced.to_string()
            } else {
                standard.to_string()
            }
        } else if cfg!(target_os = "windows") {
            // NDI 6 SDK default installation path
            "C:\\Program Files\\NDI\\NDI 6 SDK".to_string()
        } else {
            panic!("Unsupported platform, please set NDI_SDK_DIR manually.");
        }
    });

    // Determine if we're using the Advanced SDK (only relevant on Linux).
    let is_advanced = if cfg!(target_os = "linux") {
        ndi_sdk_path.to_lowercase().contains("advanced")
    } else {
        false
    };

    // Construct the include path and header file location.
    let ndi_include_path = format!("{ndi_sdk_path}/include");

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
// We need full prototypes so bindgen can generate bindings for all functions,
// including NDIlib_send_set_video_async_completion (available in NDI Advanced SDK)

// Include the main NDI header
#include <{header_file}>
"#
    );
    std::fs::write(&wrapper_path, wrapper_content).expect("Failed to create wrapper.h");

    let main_header = wrapper_path.to_str().unwrap().to_string();

    // Determine the library name and linking type based on the platform.
    let (lib_name, link_type) = if cfg!(target_os = "macos") {
        ("ndi", "dylib")
    } else if cfg!(target_os = "linux") {
        if is_advanced {
            ("ndi_advanced", "dylib")
        } else {
            ("ndi", "dylib")
        }
    } else if cfg!(target_os = "windows") {
        let target = env::var("TARGET").expect("TARGET environment variable not set");
        if target.contains("x86_64") {
            ("Processing.NDI.Lib.x64", "static")
        } else {
            ("Processing.NDI.Lib.x86", "static")
        }
    } else {
        panic!("Unsupported platform");
    };

    // Add library directory path for all platforms.
    if cfg!(target_os = "windows") {
        let target = env::var("TARGET").expect("TARGET environment variable not set");
        let lib_subdir = if target.contains("x86_64") {
            "x64"
        } else {
            "x86"
        };
        let lib_path = format!("{ndi_sdk_path}\\lib\\{lib_subdir}");
        println!("cargo:rustc-link-search=native={lib_path}");
    } else if cfg!(target_os = "linux") {
        // For Linux, detect the architecture and find the appropriate library directory
        let target = env::var("TARGET").expect("TARGET environment variable not set");

        // Determine possible architecture-specific library directories
        let arch_dirs: Vec<String> = if target.contains("x86_64") {
            vec!["x86_64-linux-gnu".to_string()]
        } else if target.contains("i686") || target.contains("i586") {
            vec!["i686-linux-gnu".to_string()]
        } else if target.contains("aarch64") {
            // For ARM64, try generic first, then Raspberry Pi specific
            vec![
                "aarch64-linux-gnu".to_string(),
                "aarch64-rpi4-linux-gnueabihf".to_string(),
            ]
        } else if target.contains("armv7") {
            // For ARMv7, try Raspberry Pi 4, 3, then 2
            vec![
                "arm-rpi4-linux-gnueabihf".to_string(),
                "arm-rpi3-linux-gnueabihf".to_string(),
                "arm-rpi2-linux-gnueabihf".to_string(),
            ]
        } else if target.contains("arm") {
            // For other ARM variants, try all Raspberry Pi variants
            vec![
                "arm-rpi4-linux-gnueabihf".to_string(),
                "arm-rpi3-linux-gnueabihf".to_string(),
                "arm-rpi2-linux-gnueabihf".to_string(),
                "arm-rpi1-linux-gnueabihf".to_string(),
            ]
        } else {
            panic!(
                "Unsupported Linux architecture: {target}. Please set NDI_SDK_DIR to point to your NDI SDK installation and ensure the architecture-specific library directory exists."
            );
        };

        // Find the first architecture directory that exists
        let lib_base = format!("{ndi_sdk_path}/lib");
        let lib_path = arch_dirs
            .iter()
            .map(|arch| format!("{lib_base}/{arch}"))
            .find(|path| Path::new(path).exists())
            .unwrap_or_else(|| {
                let searched = arch_dirs.iter().map(|arch| format!("{lib_base}/{arch}")).collect::<Vec<_>>();
                panic!(
                    "NDI SDK library directory not found for architecture: {target}. Searched in: {searched:?}. \
                    Please ensure the NDI SDK is installed correctly or set NDI_SDK_DIR to the correct location."
                )
            });

        println!("cargo:rustc-link-search=native={lib_path}");
    } else if cfg!(target_os = "macos") {
        // For macOS, add the library search path
        // NDI SDK on macOS often has libraries in lib/macOS subdirectory
        let lib_macos = format!("{ndi_sdk_path}/lib/macOS");
        let lib_base = format!("{ndi_sdk_path}/lib");

        if Path::new(&lib_macos).exists() {
            println!("cargo:rustc-link-search=native={lib_macos}");
        } else {
            println!("cargo:rustc-link-search=native={lib_base}");
        }
    }

    // Inform Cargo about the library to link against.
    println!("cargo:rustc-link-lib={link_type}={lib_name}");

    // Generate the bindings using bindgen.
    let bindings = bindgen::Builder::default()
        .header(main_header)
        .clang_arg(format!("-I{ndi_include_path}"))
        .derive_default(true)
        .generate()
        .expect("Unable to generate bindings");

    // Write the bindings to the $OUT_DIR/ndi_lib.rs file.
    let out_path =
        PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR environment variable not set"));
    let bindings_path = out_path.join("ndi_lib.rs");
    bindings
        .write_to_file(&bindings_path)
        .expect("Couldn't write bindings!");

    // Check if NDIlib_send_set_video_async_completion is available in the bindings
    // This function is only available in NDI Advanced SDK 6.1.1+
    let bindings_content =
        std::fs::read_to_string(&bindings_path).expect("Failed to read generated bindings");

    if bindings_content.contains("NDIlib_send_set_video_async_completion") {
        println!("cargo:rustc-cfg=has_async_completion_callback");
    }
}
