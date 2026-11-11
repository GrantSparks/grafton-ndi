//! Build configuration for grafton-ndi.
//!
//! Dynamically loaded targets build without an NDI SDK: the vendored bindings in
//! `src/sys` describe the ABI and the runtime library is located when the
//! program runs. This script only
//!
//! * records the default runtime edition (`NDI_SDK_EDITION`, else the edition of
//!   an installed SDK, else Standard);
//! * with the `advanced` feature, generates declarations for the Advanced-only
//!   headers of an installed Advanced SDK, which may not be redistributed;
//! * for statically linked Apple targets, selects, validates and links the
//!   vendor archive.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

mod build_apple;
mod build_support;

use build_support::{Edition, Sdk};

fn main() {
    for path in [
        "build.rs",
        "build_support.rs",
        "build_apple.rs",
        "src/sys/declarations.rs",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    for name in [
        "NDI_SDK_DIR",
        "NDI_SDK_EDITION",
        "IPHONEOS_DEPLOYMENT_TARGET",
        "TVOS_DEPLOYMENT_TARGET",
        "XROS_DEPLOYMENT_TARGET",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    for cfg in ["ndi_link_static", "ndi_has_advanced_header"] {
        println!("cargo:rustc-check-cfg=cfg({cfg})");
    }

    let target = env::var("TARGET").expect("Cargo TARGET missing");
    let os = env::var("CARGO_CFG_TARGET_OS").expect("Cargo target OS missing");
    let arch = env::var("CARGO_CFG_TARGET_ARCH").expect("Cargo target architecture missing");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR missing"));
    println!("cargo:rustc-env=GRAFTON_NDI_TARGET={target}");

    let requested = env::var("NDI_SDK_EDITION")
        .ok()
        .map(|value| Edition::parse(&value).unwrap_or_else(|error| panic!("{error}")));
    let sdk = Sdk::locate(env::var_os("NDI_SDK_DIR").as_deref().map(Path::new), &os)
        .unwrap_or_else(|error| panic!("{error}"));
    if let Some(sdk) = &sdk {
        println!("cargo:rerun-if-changed={}", sdk.include_dir.display());
    }

    #[cfg(feature = "advanced")]
    let advanced_declarations = sdk
        .as_ref()
        .filter(|sdk| sdk.is_advanced())
        .map(|sdk| generate_advanced_bindings(sdk, &target, &out));
    #[cfg(not(feature = "advanced"))]
    let advanced_declarations: Option<String> = None;
    if advanced_declarations.is_some() {
        println!("cargo:rustc-cfg=ndi_has_advanced_header");
    }

    let edition = if build_support::links_statically(&os) {
        let sdk = sdk.as_ref().unwrap_or_else(|| {
            panic!("{target} links the NDI SDK statically; set NDI_SDK_DIR to the Apple SDK")
        });
        link_static(
            sdk,
            &os,
            &arch,
            requested,
            advanced_declarations.as_deref(),
            &out,
        )
    } else {
        build_support::default_edition(requested, sdk.as_ref())
    };
    println!("cargo:rustc-env=GRAFTON_NDI_EDITION={}", edition.as_str());
}

/// Select, validate and link the vendor archive, and write the static
/// provider's symbol map. Returns the archive's edition.
fn link_static(
    sdk: &Sdk,
    os: &str,
    arch: &str,
    requested: Option<Edition>,
    advanced_declarations: Option<&str>,
    out: &Path,
) -> Edition {
    let library = build_support::static_library(
        sdk,
        os,
        requested,
        env::var_os("CARGO_FEATURE_FILE_IO").is_some(),
    )
    .unwrap_or_else(|error| panic!("{error}"));
    println!("cargo:rerun-if-changed={}", library.archive.display());
    let archive = build_apple::inspect(&library.archive, arch, os)
        .unwrap_or_else(|error| panic!("Invalid Apple static SDK artifact: {error}"));

    let (deployment_variable, floor) = (
        match os {
            "ios" => "IPHONEOS_DEPLOYMENT_TARGET",
            "tvos" => "TVOS_DEPLOYMENT_TARGET",
            _ => "XROS_DEPLOYMENT_TARGET",
        },
        archive.deployment_floor,
    );
    // An unset deployment target is left to the application's linker, which
    // rejects objects built for a newer OS than it targets.
    if let Ok(value) = env::var(deployment_variable) {
        if !deployment_target_at_least(&value, floor) {
            panic!(
                "{deployment_variable}={value} is invalid or below the selected SDK minimum {}.{}.{}",
                floor >> 16,
                (floor >> 8) & 255,
                floor & 255
            );
        }
    }

    println!("cargo:rustc-cfg=ndi_link_static");

    // Rust's static bundling accepts an ordinary archive, not a universal one.
    let thin_directory = out.join("ndi-static");
    fs::create_dir_all(&thin_directory).expect("Create selected Apple archive directory");
    let thin_archive = thin_directory.join(
        library
            .archive
            .file_name()
            .expect("Selected Apple archive filename"),
    );
    archive
        .write_thin(&thin_archive)
        .unwrap_or_else(|error| panic!("Materialize selected Apple static archive: {error}"));
    println!(
        "cargo:rustc-link-search=native={}",
        thin_directory.display()
    );
    println!("cargo:rustc-link-lib=static={}", library.link_name);
    for framework in [
        "AudioToolbox",
        "CoreFoundation",
        "CoreMedia",
        "CoreVideo",
        "Foundation",
        "VideoToolbox",
        "Accelerate",
    ] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
    // Apple exposes DNS-SD through libSystem; no separate dns_sd library exists.
    for system_library in ["c++", "objc", "z"] {
        println!("cargo:rustc-link-lib=dylib={system_library}");
    }

    let vendored =
        fs::read_to_string("src/sys/declarations.rs").expect("Read vendored declarations");
    let mut declarations = vec![("crate::sys::declarations", vendored.as_str())];
    if let Some(advanced) = advanced_declarations {
        declarations.push(("crate::sys::advanced_declarations", advanced));
    }
    let symbols = build_support::static_symbol_map(&declarations, &archive.symbols)
        .unwrap_or_else(|error| panic!("{error}"));
    fs::write(out.join("ndi_static_symbols.rs"), symbols).expect("Write static symbol map");
    library.edition
}

/// Whether `value` (`major[.minor[.patch]]`) is a valid version at or above
/// `floor`, packed as Mach-O encodes it.
fn deployment_target_at_least(value: &str, floor: u32) -> bool {
    let parts: Vec<Option<u32>> = value
        .split('.')
        .map(|part| part.parse().ok().filter(|&number: &u32| number <= 255))
        .collect();
    let [major, rest @ ..] = parts.as_slice() else {
        return false;
    };
    if rest.len() > 2 || parts.iter().any(Option::is_none) {
        return false;
    }
    let component = |index: usize| rest.get(index).copied().flatten().unwrap_or(0);
    ((major.unwrap_or(0) << 16) | (component(0) << 8) | component(1)) >= floor
}

/// Generate bindings for the headers of `sdk` that may not be redistributed:
/// types into `OUT_DIR/ndi_advanced.rs` and foreign functions into
/// `OUT_DIR/ndi_advanced_declarations.rs`. Returns the function declarations.
///
/// Common SDK types are not re-emitted: the generated code names them and the
/// including modules import the vendored bindings.
#[cfg(feature = "advanced")]
fn generate_advanced_bindings(sdk: &Sdk, target: &str, out: &Path) -> String {
    // visionOS is called xros by Apple's Clang; Rust's target name differs.
    let clang_target = if target == "aarch64-apple-visionos" {
        "aarch64-apple-xros"
    } else {
        target
    };
    // Only the compiler's own freestanding headers are visible, so no target
    // sysroot is needed. The SDK includes <string.h> without using it.
    let freestanding = out.join("ndi-freestanding");
    fs::create_dir_all(&freestanding)
        .and_then(|()| fs::write(freestanding.join("string.h"), ""))
        .expect("Create freestanding header directory");
    let mut builder = bindgen::Builder::default()
        .header_contents("wrapper.h", "#include <Processing.NDI.Advanced.h>\n")
        .clang_arg(format!("--target={clang_target}"))
        .clang_args(["-ffreestanding", "-nostdlibinc"])
        .clang_arg(format!("-I{}", freestanding.display()))
        .clang_arg(format!("-I{}", sdk.include_dir.display()))
        .derive_default(true)
        .allowlist_recursively(false)
        .formatter(bindgen::Formatter::Prettyplease);
    let restricted = sdk
        .restricted_headers()
        .unwrap_or_else(|error| panic!("{error}"));
    for header in &restricted {
        let escaped: String = header
            .chars()
            .flat_map(|character| {
                let literal = character.is_ascii_alphanumeric() || character == '_';
                (!literal).then_some('\\').into_iter().chain([character])
            })
            .collect();
        builder = builder.allowlist_file(format!(r".*[/\\]{escaped}"));
    }
    let generate = |codegen: bindgen::CodegenConfig, file: &str| {
        let bindings = builder
            .clone()
            .with_codegen_config(codegen)
            .generate()
            .unwrap_or_else(|error| {
                panic!("Unable to generate Advanced SDK bindings for {target}: {error}")
            })
            .to_string();
        fs::write(out.join(file), &bindings).expect("Write Advanced SDK bindings");
        bindings
    };
    generate(
        bindgen::CodegenConfig::all() - bindgen::CodegenConfig::FUNCTIONS,
        "ndi_advanced.rs",
    );
    generate(
        bindgen::CodegenConfig::FUNCTIONS,
        "ndi_advanced_declarations.rs",
    )
}
