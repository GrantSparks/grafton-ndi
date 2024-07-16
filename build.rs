extern crate bindgen;

use std::env;
use std::path::PathBuf;

fn main() {
    // Base path to the NDI SDK
    let ndi_sdk_path = "C:\\Program Files\\NDI\\NDI 6 SDK";

    // Paths to the include and main header file
    let ndi_include_path = format!("{}\\Include", ndi_sdk_path);
    let main_header = format!("{}\\Processing.NDI.Lib.h", ndi_include_path);

    // Determine if we are targeting 64-bit or 32-bit
    let target = env::var("TARGET").expect("TARGET environment variable not set");
    let (lib_subdir, lib_name) = if target.contains("x86_64") {
        ("x64", "Processing.NDI.Lib.x64.lib")
    } else {
        ("x86", "Processing.NDI.Lib.x86.lib")
    };

    // Path to the library directory
    let lib_path = format!("{}\\Lib\\{}", ndi_sdk_path, lib_subdir);

    // Inform cargo about the search path for the linker and the library to link against
    println!("cargo:rustc-link-search=native={}", lib_path);
    println!("cargo:rustc-link-lib=static={}", lib_name);

    // Generate the bindings
    let bindings = bindgen::Builder::default()
        .header(main_header)
        .clang_arg(format!("-I{}", ndi_include_path)) // Include the NDI SDK directory
        .generate()
        .expect("Unable to generate bindings");

    // Write the bindings to the $OUT_DIR/bindings.rs file
    let out_path = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR environment variable not set"));
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}
