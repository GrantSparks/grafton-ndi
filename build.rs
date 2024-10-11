extern crate bindgen;

use std::env;
use std::path::PathBuf;

fn main() {
    // Base path to the NDI SDK from the environment variable or default to /usr/local/'NDI SDK for Linux'
    let ndi_sdk_path = env::var("NDI_SDK_DIR").unwrap_or_else(|_| "/usr/local/'NDI SDK for Linux'".to_string());

    // Paths to the include and main header file
    let ndi_include_path = format!("{}/include", ndi_sdk_path);
    let main_header = format!("{}/Processing.NDI.Lib.h", ndi_include_path);

    // Determine if we are targeting 64-bit or 32-bit
    let target = env::var("TARGET").expect("TARGET environment variable not set");
    let (lib_subdir, lib_name) = if target.contains("x86_64") {
        ("x64", "Processing.NDI.Lib.x64")
    } else {
        ("x86", "Processing.NDI.Lib.x86")
    };

    // Path to the library directory
    let lib_path = format!("{}\\lib\\{}", ndi_sdk_path, lib_subdir);

    // Inform cargo about the search path for the linker and the library to link against
    println!("cargo:rustc-link-search=native={}", lib_path);
    println!("cargo:rustc-link-lib=static={}", lib_name);

    // Generate the bindings
    let bindings = bindgen::Builder::default()
        .header(main_header)
        .clang_arg(format!("-I{}", ndi_include_path))
        //.layout_tests(false)
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
