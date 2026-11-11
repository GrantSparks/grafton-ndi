use std::{
    env, fs,
    path::{Path, PathBuf},
};
mod build_apple;
mod build_bindings;
mod build_support;

// The same declaration registry drives File's linked/loaded operation tables.
// Ignore signatures here: only symbol presence in the selected archive matters.
macro_rules! file_functions {
    ($( $field:ident => $symbol:ident ($($arg:ident: $ty:ty),*) -> $ret:ty; )*) => {
        const FILE_SYMBOLS: &[&str] = &[$(stringify!($symbol),)*];
    };
}
include!("src/file/functions.rs");

fn default_sdk(
    os: &str,
    arch: &str,
    target: &str,
    options: build_support::SelectionOptions<'_>,
) -> PathBuf {
    let paths: &[&str] = match os {
        "macos" => &[
            "/Library/NDI SDK for macOS",
            "/Library/NDI SDK for Apple",
            "/Library/NDI Advanced SDK for Apple",
            "/Library/NDI 6 SDK",
            "/Library/NDI SDK",
            "/Library/NewTek/NDI SDK",
            "/Library/Application Support/NDI SDK for Apple",
            "/Applications/NDI SDK for Apple",
            "/Applications/NDI 6 SDK",
        ],
        "linux" => &[
            "/usr/share/NDI Advanced SDK for Linux",
            "/usr/share/NDI SDK for Linux",
        ],
        "windows" => &[
            r"C:\Program Files\NDI\NDI 6 SDK",
            r"C:\Program Files\NDI\NDI 6 Advanced SDK",
        ],
        _ => panic!("Set NDI_SDK_DIR to the target SDK installation for {os}"),
    };
    // Edition selection is based on actual artifacts, including when both
    // default installation roots exist. A broken first root cannot mask a
    // complete compatible installation at a later documented location.
    for path in paths {
        if build_support::resolve_installation(Path::new(path), os, arch, target, options).is_ok() {
            return PathBuf::from(path);
        }
    }
    panic!("No compatible SDK found at default locations {paths:?}; set NDI_SDK_DIR to a complete installation matching NDI_SDK_EDITION and TARGET");
}

fn main() {
    for path in [
        "build.rs",
        "build_support.rs",
        "build_apple.rs",
        "build_bindings.rs",
        "build_profiles.tsv",
        "src/file/functions.rs",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    for name in [
        "NDI_SDK_DIR",
        "NDI_SDK_PROFILE",
        "NDI_SDK_EDITION",
        "LIBCLANG_PATH",
        "CLANG_PATH",
        "BINDGEN_EXTRA_CLANG_ARGS",
        "SDKROOT",
        "IPHONEOS_DEPLOYMENT_TARGET",
        "TVOS_DEPLOYMENT_TARGET",
        "XROS_DEPLOYMENT_TARGET",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    for cfg in [
        "ndi_public_docs",
        "ndi_link_static",
        "ndi_file_static",
        "ndi_has_advanced_header",
        "ndi_has_discovery_header",
    ] {
        println!("cargo:rustc-check-cfg=cfg({cfg})");
    }
    let target = env::var("TARGET").expect("Cargo TARGET missing");
    let host = env::var("HOST").expect("Cargo HOST missing");
    let os = env::var("CARGO_CFG_TARGET_OS").expect("Cargo target OS missing");
    let arch = env::var("CARGO_CFG_TARGET_ARCH").expect("Cargo target architecture missing");
    if target != host && env::var_os("NDI_SDK_DIR").is_none() {
        panic!("Cross-compiling {host} to {target} requires NDI_SDK_DIR and target headers/sysroot configured via BINDGEN_EXTRA_CLANG_ARGS or the target toolchain");
    }
    let profile = env::var("NDI_SDK_PROFILE").ok();
    let edition = env::var("NDI_SDK_EDITION").ok();
    let options = build_support::SelectionOptions {
        profile: profile.as_deref(),
        edition: edition.as_deref(),
        file_io: env::var_os("CARGO_FEATURE_FILE_IO").is_some(),
    };
    let sdk = env::var_os("NDI_SDK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_sdk(&os, &arch, &target, options));
    let selected = build_support::resolve_installation(&sdk, &os, &arch, &target, options)
        .unwrap_or_else(|e| panic!("{e}"));
    build_support::validate_elf(&selected.core_artifact, &os, &arch, &target)
        .unwrap_or_else(|e| panic!("{e}"));
    let is_static = selected.linkage == "static";
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR missing"));
    let static_archive = if is_static {
        Some(
            build_apple::inspect(&selected.link_artifact, &arch, &os)
                .unwrap_or_else(|e| panic!("Invalid Apple static SDK artifact: {e}")),
        )
    } else {
        None
    };
    println!("cargo:rerun-if-changed={}", selected.library_dir.display());
    println!(
        "cargo:rerun-if-changed={}",
        selected.core_artifact.display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        selected.link_artifact.display()
    );
    let version_file = sdk.join("Version.txt");
    if version_file.is_file() {
        println!("cargo:rerun-if-changed={}", version_file.display());
    } else {
        println!("cargo:rerun-if-changed={}", sdk.display());
    }
    let version = fs::read_to_string(&version_file)
        .unwrap_or_default()
        .trim()
        .replace(['\r', '\n'], " ");
    // Only portable names are embedded; the developer's private SDK path must
    // never become an application's runtime search path.
    for (name, value) in [
        (
            "GRAFTON_NDI_CORE_LIBRARY_NAME",
            selected.library_name.as_str(),
        ),
        ("GRAFTON_NDI_LIBRARY_NAME", selected.library_name.as_str()),
        (
            "GRAFTON_NDI_FILE_LIBRARY_NAME",
            selected.file_library_name.as_str(),
        ),
        ("GRAFTON_NDI_EDITION", selected.edition),
        ("GRAFTON_NDI_LINKAGE", selected.linkage),
        ("GRAFTON_NDI_PROFILE", selected.profile.as_str()),
        ("GRAFTON_NDI_STATIC_PROVIDER", selected.static_provider),
        ("GRAFTON_NDI_SDK_VERSION", version.as_str()),
    ] {
        println!("cargo:rustc-env={name}={value}");
    }
    if is_static {
        let floor = static_archive.as_ref().unwrap().deployment_floor;
        let deployment = match os.as_str() {
            "ios" => "IPHONEOS_DEPLOYMENT_TARGET",
            "tvos" => "TVOS_DEPLOYMENT_TARGET",
            _ => "XROS_DEPLOYMENT_TARGET",
        };
        let minimum = format!("{}.{}.{}", floor >> 16, (floor >> 8) & 255, floor & 255);
        if let Ok(value) = env::var(deployment) {
            let parts: Vec<u32> = value
                .split('.')
                .map(|v| v.parse::<u32>().unwrap_or(u32::MAX))
                .collect();
            let invalid = parts.is_empty() || parts.len() > 3 || parts.iter().any(|&v| v > 255);
            let requested = if invalid {
                0
            } else {
                (parts[0] << 16)
                    | (parts.get(1).copied().unwrap_or(0) << 8)
                    | parts.get(2).copied().unwrap_or(0)
            };
            if invalid || requested < floor {
                panic!(
                    "{deployment}={value} is below or invalid for selected SDK minimum {minimum}"
                );
            }
        } else {
            println!("cargo:warning=Selected SDK requires {deployment}>={minimum}; set the deployment target in the application build environment");
        }
        println!("cargo:rustc-env=GRAFTON_NDI_MIN_DEPLOYMENT_TARGET={minimum}");
        println!("cargo:rustc-cfg=ndi_link_static");
        if selected.static_provider == "file" {
            let archive = static_archive.as_ref().unwrap();
            let missing: Vec<_> = FILE_SYMBOLS
                .iter()
                .filter(|symbol| !archive.symbols.contains(**symbol))
                .collect();
            if missing.is_empty() {
                println!("cargo:rustc-cfg=ndi_file_static");
            } else {
                println!("cargo:warning=Selected File archive lacks required File operations {missing:?}; its core remains usable but the File API is unavailable");
            }
        }
        let thin_directory = out.join("ndi-static");
        fs::create_dir_all(&thin_directory).expect("Create selected Apple archive directory");
        let thin_archive = thin_directory.join(
            selected
                .link_artifact
                .file_name()
                .expect("Selected Apple archive filename"),
        );
        static_archive
            .as_ref()
            .unwrap()
            .write_thin(&thin_archive)
            .unwrap_or_else(|e| panic!("Materialize selected Apple static archive: {e}"));
        println!(
            "cargo:rustc-link-search=native={}",
            thin_directory.display()
        );
        println!("cargo:rustc-link-lib=static={}", selected.link_name);
        // Mobile File includes its own core. Exactly one vendor archive is
        // selected above; no whole-archive or second Standard/Advanced provider.
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
        // Apple exposes DNS-SD through libSystem's system_dnssd component;
        // there is no separate dns_sd library required by these targets.
        for library in ["c++", "objc", "z"] {
            println!("cargo:rustc-link-lib=dylib={library}");
        }
    }
    // Shared providers deliberately emit NO mandatory NDI linker directives.
    // Runtime dispatch resolves the complete baseline from one selected module.
    let wrapper = out.join("wrapper.h");
    let mut source = format!(
        "#include <{}>\n",
        selected.header.file_name().unwrap().to_string_lossy()
    );
    let advanced_header = selected.include_dir.join("Processing.NDI.Advanced.h");
    let mut header_dirs = vec![selected.include_dir.clone()];
    while let Some(dir) = header_dirs.pop() {
        for entry in fs::read_dir(dir).expect("Read SDK include directory") {
            let path = entry.expect("Read SDK header entry").path();
            if path.is_dir() {
                header_dirs.push(path);
            } else if path.extension().is_some_and(|ext| ext == "h") {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
    if selected
        .include_dir
        .join("Processing.NDI.SendListener.h")
        .is_file()
        && selected
            .include_dir
            .join("Processing.NDI.RecvListener.h")
            .is_file()
    {
        println!("cargo:rustc-cfg=ndi_has_discovery_header");
    }
    println!("cargo:rerun-if-changed={}", selected.include_dir.display());
    let advanced = env::var_os("CARGO_FEATURE_ADVANCED").is_some() && advanced_header.is_file();
    if advanced {
        source.push_str("#include <Processing.NDI.Advanced.h>\n");
        println!("cargo:rustc-cfg=ndi_has_advanced_header");
    }
    fs::write(&wrapper, source).expect("Write bindgen wrapper");
    // visionOS is called xros by Apple's Clang; Rust's target name differs.
    let clang_target = if target == "aarch64-apple-visionos" {
        "aarch64-apple-xros"
    } else {
        &target
    };
    let mut builder = bindgen::Builder::default()
        .header(wrapper.to_string_lossy())
        .clang_arg(format!("-I{}", selected.include_dir.display()))
        .clang_arg(format!("--target={clang_target}"))
        .parse_callbacks(Box::new(
            bindgen::CargoCallbacks::new().rerun_on_header_files(false),
        ))
        .derive_default(true);
    if is_static {
        builder = builder.clang_arg("-DPROCESSINGNDILIB_STATIC");
    }
    let advanced_types = if advanced {
        builder
            .clone()
            .ignore_functions()
            .allowlist_type("NDIlib_.*")
            .allowlist_var("NDIlib_.*")
            .generate()
            .unwrap_or_else(|e| panic!("Unable to generate local Advanced ABI types: {e}"))
            .to_string()
    } else {
        String::new()
    };
    let generated = builder.generate().unwrap_or_else(|e| panic!("Unable to generate NDI bindings for {target}: {e}. Configure the target sysroot through BINDGEN_EXTRA_CLANG_ARGS.")).to_string();
    fs::write(out.join("ndi_lib.rs"), &generated).expect("Write private NDI bindings");
    // Advanced output is generated only from the user's installed headers and
    // lives exclusively in OUT_DIR. Standard/old-header all-features builds work.
    fs::write(out.join("ndi_advanced.rs"), advanced_types)
        .expect("Write local Advanced binding probe");
    let mut symbols = String::from("#[allow(deprecated)]\n{ match name {\n");
    let static_functions = static_archive
        .as_ref()
        .map(|archive| build_bindings::static_functions(&generated, &archive.symbols))
        .transpose()
        .expect("Generate static NDI dispatch addresses")
        .unwrap_or_default();
    for name in static_functions {
        symbols.push_str(&format!("b\"{name}\\0\" => Some(crate::ndi_lib::raw::{name} as *const () as *const std::ffi::c_void),\n"));
    }
    symbols.push_str("_ => None,\n} }\n");
    fs::write(out.join("ndi_static_symbols.rs"), symbols).expect("Write static dispatch addresses");
}
