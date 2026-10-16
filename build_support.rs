//! Pure SDK target and artifact selection shared by the build script and its tests.
use std::path::{Path, PathBuf};

pub fn library_subdirs(os: &str, arch: &str, target: &str) -> Result<Vec<&'static str>, String> {
    if target.starts_with("aarch64_be")
        || target.starts_with("armeb")
        || target.starts_with("armv5")
    {
        return Err(format!(
            "No compatible NDI SDK binaries for target {target}"
        ));
    }
    match (os, arch) {
        ("linux", _) if !target.ends_with("-gnu") && !target.ends_with("-gnueabihf") => Err(
            format!("NDI Linux SDK requires a GNU target ABI; unsupported target {target}"),
        ),
        ("linux", "x86_64") => Ok(vec!["x86_64-linux-gnu"]),
        ("linux", "x86") => Ok(vec!["i686-linux-gnu"]),
        ("linux", "aarch64") => Ok(vec!["aarch64-linux-gnu", "aarch64-rpi4-linux-gnueabi"]),
        ("linux", "arm") => Ok(if target.starts_with("armv7") {
            vec![
                "arm-rpi4-linux-gnueabihf",
                "arm-rpi3-linux-gnueabihf",
                "arm-rpi2-linux-gnueabihf",
            ]
        } else {
            // ARMv6 targets cannot use the newer ARMv7 SDK binaries.
            vec!["arm-rpi1-linux-gnueabihf"]
        }),
        ("macos", "x86_64" | "aarch64") => Ok(vec!["macOS", ""]),
        ("windows", "x86_64") if target.ends_with("-msvc") => Ok(vec!["x64"]),
        ("windows", "x86") if target.ends_with("-msvc") => Ok(vec!["x86"]),
        ("android", "aarch64") if target == "aarch64-linux-android" => Ok(vec!["arm64-v8a"]),
        ("android", "arm") if target == "armv7-linux-androideabi" => Ok(vec!["armeabi-v7a"]),
        ("android", "x86") if target == "i686-linux-android" => Ok(vec!["x86"]),
        ("android", "x86_64") if target == "x86_64-linux-android" => Ok(vec!["x86_64"]),
        _ => Err(format!(
            "Unsupported NDI SDK target {target} (OS {os}, architecture {arch})"
        )),
    }
}

pub fn select_library(
    sdk: &Path,
    os: &str,
    arch: &str,
    target: &str,
) -> Result<(PathBuf, &'static str, &'static str), String> {
    let subdirs = library_subdirs(os, arch, target)?;
    let candidates: &[(&str, &str, &str)] = match os {
        "linux" | "android" => &[
            ("libndi_advanced.so", "ndi_advanced", "dylib"),
            ("libndi.so", "ndi", "dylib"),
        ],
        "macos" => &[("libndi.dylib", "ndi", "dylib")],
        "windows" if arch == "x86_64" => &[(
            "Processing.NDI.Lib.x64.lib",
            "Processing.NDI.Lib.x64",
            "static",
        )],
        "windows" => &[(
            "Processing.NDI.Lib.x86.lib",
            "Processing.NDI.Lib.x86",
            "static",
        )],
        _ => unreachable!("validated target"),
    };
    let mut searched = Vec::new();
    // Windows installers use Lib/Include. Keep their casing usable when
    // cross-compiling on a case-sensitive host, without modifying the SDK.
    let lib_root = if os == "windows" && sdk.join("Lib").is_dir() {
        sdk.join("Lib")
    } else {
        sdk.join("lib")
    };
    for subdir in subdirs {
        let directory = lib_root.join(subdir);
        for &(artifact, name, kind) in candidates {
            let path = directory.join(artifact);
            if path.is_file() {
                return Ok((directory, name, kind));
            }
            searched.push(path.display().to_string());
        }
    }
    Err(format!("NDI SDK library for {target} not found. Searched: {}. Set NDI_SDK_DIR to a complete SDK installation for the target.", searched.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uses_target_architecture_and_rejects_unsupported_abis() {
        assert_eq!(
            library_subdirs("windows", "x86_64", "x86_64-pc-windows-msvc").unwrap(),
            ["x64"]
        );
        assert_eq!(
            library_subdirs("linux", "aarch64", "aarch64-unknown-linux-gnu").unwrap(),
            ["aarch64-linux-gnu", "aarch64-rpi4-linux-gnueabi"]
        );
        assert!(library_subdirs("linux", "x86_64", "x86_64-unknown-linux-musl").is_err());
        assert!(library_subdirs("windows", "aarch64", "aarch64-pc-windows-msvc").is_err());
        assert!(library_subdirs("linux", "aarch64", "aarch64_be-unknown-linux-gnu").is_err());
        assert!(library_subdirs("windows", "x86_64", "x86_64-pc-windows-gnu").is_err());
    }
    #[test]
    fn selects_windows_and_macos_artifacts_from_target_layout() {
        let root = std::env::temp_dir().join(format!(
            "grafton-build-platform-test-{}",
            std::process::id()
        ));
        for (os, arch, target, subdir, file, expected) in [
            (
                "windows",
                "x86_64",
                "x86_64-pc-windows-msvc",
                "x64",
                "Processing.NDI.Lib.x64.lib",
                "Processing.NDI.Lib.x64",
            ),
            (
                "macos",
                "aarch64",
                "aarch64-apple-darwin",
                "macOS",
                "libndi.dylib",
                "ndi",
            ),
        ] {
            let sdk = root.join(os);
            let lib = sdk.join("lib").join(subdir);
            std::fs::create_dir_all(&lib).unwrap();
            std::fs::write(lib.join(file), []).unwrap();
            assert_eq!(select_library(&sdk, os, arch, target).unwrap().1, expected);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn library_selection_uses_artifacts_not_sdk_directory_name() {
        let root = std::env::temp_dir().join(format!("grafton-build-test-{}", std::process::id()));
        let sdk = root.join("arbitrary-sdk-name");
        let lib = sdk.join("lib/x86_64-linux-gnu");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("libndi_advanced.so"), []).unwrap();
        assert_eq!(
            select_library(&sdk, "linux", "x86_64", "x86_64-unknown-linux-gnu")
                .unwrap()
                .1,
            "ndi_advanced"
        );
        std::fs::remove_file(lib.join("libndi_advanced.so")).unwrap();
        std::fs::write(lib.join("libndi.so"), []).unwrap();
        assert_eq!(
            select_library(&sdk, "linux", "x86_64", "x86_64-unknown-linux-gnu")
                .unwrap()
                .1,
            "ndi"
        );
        std::fs::remove_file(lib.join("libndi.so")).unwrap();
        assert!(
            select_library(&sdk, "linux", "x86_64", "x86_64-unknown-linux-gnu")
                .unwrap_err()
                .contains("libndi.so")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selects_aarch64_artifacts_only_from_matching_sdk_directories() {
        let root =
            std::env::temp_dir().join(format!("grafton-aarch64-build-test-{}", std::process::id()));
        let sdk = root.join("sdk");
        let x86 = sdk.join("lib/x86_64-linux-gnu");
        let fallback = sdk.join("lib/aarch64-rpi4-linux-gnueabi");
        let generic = sdk.join("lib/aarch64-linux-gnu");
        std::fs::create_dir_all(&x86).unwrap();
        std::fs::create_dir_all(&fallback).unwrap();
        std::fs::write(x86.join("libndi.so"), []).unwrap();
        let target = "aarch64-unknown-linux-gnu";
        assert!(select_library(&sdk, "linux", "aarch64", target).is_err());

        std::fs::write(fallback.join("libndi.so"), []).unwrap();
        assert_eq!(
            select_library(&sdk, "linux", "aarch64", target).unwrap(),
            (fallback.clone(), "ndi", "dylib")
        );

        std::fs::write(fallback.join("libndi_advanced.so"), []).unwrap();
        assert_eq!(
            select_library(&sdk, "linux", "aarch64", target).unwrap(),
            (fallback, "ndi_advanced", "dylib")
        );

        std::fs::create_dir_all(&generic).unwrap();
        std::fs::write(generic.join("libndi.so"), []).unwrap();
        assert_eq!(
            select_library(&sdk, "linux", "aarch64", target).unwrap(),
            (generic, "ndi", "dylib")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selects_android_libraries_for_each_abi_without_linux_fallback() {
        let root =
            std::env::temp_dir().join(format!("grafton-android-build-test-{}", std::process::id()));
        let linux = root.join("lib/x86_64-linux-gnu");
        std::fs::create_dir_all(&linux).unwrap();
        std::fs::write(linux.join("libndi.so"), []).unwrap();
        for (arch, target, abi) in [
            ("aarch64", "aarch64-linux-android", "arm64-v8a"),
            ("arm", "armv7-linux-androideabi", "armeabi-v7a"),
            ("x86", "i686-linux-android", "x86"),
            ("x86_64", "x86_64-linux-android", "x86_64"),
        ] {
            assert!(select_library(&root, "android", arch, target).is_err());
            let lib = root.join("lib").join(abi);
            std::fs::create_dir_all(&lib).unwrap();
            std::fs::write(lib.join("libndi.so"), []).unwrap();
            assert_eq!(
                select_library(&root, "android", arch, target).unwrap(),
                (lib, "ndi", "dylib")
            );
        }
        assert!(library_subdirs("android", "arm", "arm-linux-androideabi").is_err());
        assert!(library_subdirs("android", "riscv64", "riscv64-linux-android").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selects_windows_installer_casing_on_case_sensitive_hosts() {
        let root = std::env::temp_dir().join(format!(
            "grafton-windows-casing-test-{}",
            std::process::id()
        ));
        let lib = root.join("Lib/x64");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("Processing.NDI.Lib.x64.lib"), []).unwrap();
        assert_eq!(
            select_library(&root, "windows", "x86_64", "x86_64-pc-windows-msvc").unwrap(),
            (lib, "Processing.NDI.Lib.x64", "static")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
