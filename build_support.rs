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
        ("linux", "aarch64") => Ok(vec!["aarch64-linux-gnu", "aarch64-rpi4-linux-gnueabihf"]),
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
        "linux" => &[
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
    for subdir in subdirs {
        let directory = sdk.join("lib").join(subdir);
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
            library_subdirs("linux", "aarch64", "aarch64-unknown-linux-gnu").unwrap()[0],
            "aarch64-linux-gnu"
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
}
