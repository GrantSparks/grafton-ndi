//! Pure SDK target/profile selection, shared by the build script and its tests.
use std::path::{Path, PathBuf};

/// A vendor artifact profile. CPU/libc values are inspected artifact requirements,
/// not a claim of physical-device validation or the complete dependency closure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    pub id: &'static str,
    pub target: &'static str,
    pub os: &'static str,
    pub arch: &'static str,
    pub subdir: &'static str,
    pub editions: &'static str,
    pub libc: &'static str,
    pub float_abi: &'static str,
    pub cpu: &'static str,
    pub min_glibc: &'static str,
    pub linkage: &'static str,
}

pub fn profiles() -> impl Iterator<Item = Profile> {
    include_str!("build_profiles.tsv")
        .lines()
        .skip(1)
        .map(|line| {
            let f: Vec<_> = line.split('\t').collect();
            assert_eq!(f.len(), 11, "invalid build_profiles.tsv row");
            Profile {
                id: f[0],
                target: f[1],
                os: f[2],
                arch: f[3],
                subdir: f[4],
                editions: f[5],
                libc: f[6],
                float_abi: f[7],
                cpu: f[8],
                min_glibc: f[9],
                linkage: f[10],
            }
        })
}

pub fn library_subdirs(os: &str, arch: &str, target: &str) -> Result<Vec<&'static str>, String> {
    // Preserve historical generic inputs, never infer a specialized SoC from its CPU.
    match (os, arch, target) {
        ("linux", "x86_64", "x86_64-unknown-linux-gnu") => Ok(vec!["x86_64-linux-gnu"]),
        ("linux", "x86", "i686-unknown-linux-gnu") => Ok(vec!["i686-linux-gnu"]),
        ("linux", "aarch64", "aarch64-unknown-linux-gnu") => Ok(vec!["aarch64-linux-gnu", "aarch64-rpi4-linux-gnueabi"]),
        ("linux", "arm", "armv7-unknown-linux-gnueabihf") => Ok(vec!["arm-rpi4-linux-gnueabihf", "arm-rpi3-linux-gnueabihf", "arm-rpi2-linux-gnueabihf"]),
        // This preserves the prior selection, not a new ARMv6 runtime guarantee:
        // even the historical 6.3.2 rpi1 artifact declares ARMv7 attributes.
        ("linux", "arm", "arm-unknown-linux-gnueabihf") => Ok(vec!["arm-rpi1-linux-gnueabihf"]),
        ("macos", "x86_64", "x86_64-apple-darwin") | ("macos", "aarch64", "aarch64-apple-darwin") => Ok(vec!["macOS", ""]),
        _ => profiles().find(|p| p.os == os && p.arch == arch && p.target == target && os != "linux")
            .map(|p| vec![p.subdir]).ok_or_else(|| format!(
                "Unsupported or ambiguous NDI SDK target {target} (OS {os}, architecture {arch}). Specialized Linux targets require NDI_SDK_PROFILE; Apple arm64 simulator slices are not supplied by this SDK.")),
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SelectionOptions<'a> {
    pub profile: Option<&'a str>,
    pub edition: Option<&'a str>,
    pub file_io: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installation {
    pub include_dir: PathBuf,
    pub header: PathBuf,
    pub library_dir: PathBuf,
    pub core_artifact: PathBuf,
    pub link_artifact: PathBuf,
    pub link_name: String,
    pub library_name: String,
    pub file_library_name: String,
    pub edition: &'static str,
    pub linkage: &'static str,
    pub profile: String,
    pub static_provider: &'static str,
}

/// Casing belongs to the installation layout, not to the target operating system.
pub fn directory(root: &Path, lowercase: &str, uppercase: &str) -> Result<PathBuf, String> {
    // Probing both spellings with is_dir() finds the same entry twice on a
    // case-insensitive filesystem. Enumerate names so the returned path uses
    // the installation's spelling on every host, including APFS and Windows.
    let read_error = |error| format!("Cannot read SDK directory {}: {error}", root.display());
    let mut directories = Vec::new();
    for entry in std::fs::read_dir(root).map_err(read_error)? {
        let entry = entry.map_err(read_error)?;
        if entry.file_name().to_str().is_some_and(|name| {
            name.eq_ignore_ascii_case(lowercase) || name.eq_ignore_ascii_case(uppercase)
        }) && entry.path().is_dir()
        {
            directories.push(entry.path());
        }
    }
    // Preserve the prior preference when actual symlink aliases coexist.
    let preference = |path: &PathBuf| match path.file_name().and_then(|name| name.to_str()) {
        Some(name) if name == lowercase => 0,
        Some(name) if name == uppercase => 1,
        _ => 2,
    };
    directories.sort_by(|a, b| preference(a).cmp(&preference(b)).then_with(|| a.cmp(b)));
    let Some(selected) = directories.first() else {
        return Err(format!(
            "SDK directory missing: {} or {}",
            root.join(lowercase).display(),
            root.join(uppercase).display()
        ));
    };
    for other in directories.iter().skip(1) {
        if !matches!((selected.canonicalize(), other.canonicalize()), (Ok(a), Ok(b)) if a == b) {
            return Err(format!(
                "Ambiguous SDK layout: both {} and {} exist",
                selected.display(),
                other.display()
            ));
        }
    }
    Ok(selected.clone())
}

#[derive(Clone, Copy)]
struct Candidate {
    artifact: &'static str,
    link: &'static str,
    runtime: &'static str,
    edition: &'static str,
}

fn candidates(os: &str, arch: &str) -> &'static [Candidate] {
    use Candidate as C;
    match (os, arch) {
        ("linux", _) => &[
            C {
                artifact: "libndi_advanced.so",
                link: "ndi_advanced",
                runtime: "libndi_advanced.so.6",
                edition: "advanced",
            },
            C {
                artifact: "libndi.so",
                link: "ndi",
                runtime: "libndi.so.6",
                edition: "standard",
            },
        ],
        ("android", _) => &[
            C {
                artifact: "libndi_advanced.so",
                link: "ndi_advanced",
                runtime: "libndi_advanced.so",
                edition: "advanced",
            },
            C {
                artifact: "libndi.so",
                link: "ndi",
                runtime: "libndi.so",
                edition: "standard",
            },
        ],
        ("macos", _) => &[
            C {
                artifact: "libndi_advanced.dylib",
                link: "ndi_advanced",
                runtime: "libndi_advanced.dylib",
                edition: "advanced",
            },
            C {
                artifact: "libndi.dylib",
                link: "ndi",
                runtime: "libndi.dylib",
                edition: "standard",
            },
        ],
        ("windows", "x86_64") => &[
            C {
                artifact: "Processing.NDI.Lib.Advanced.x64.lib",
                link: "Processing.NDI.Lib.Advanced.x64",
                runtime: "Processing.NDI.Lib.Advanced.x64.dll",
                edition: "advanced",
            },
            C {
                artifact: "Processing.NDI.Lib.x64.lib",
                link: "Processing.NDI.Lib.x64",
                runtime: "Processing.NDI.Lib.x64.dll",
                edition: "standard",
            },
        ],
        ("windows", _) => &[
            C {
                artifact: "Processing.NDI.Lib.Advanced.x86.lib",
                link: "Processing.NDI.Lib.Advanced.x86",
                runtime: "Processing.NDI.Lib.Advanced.x86.dll",
                edition: "advanced",
            },
            C {
                artifact: "Processing.NDI.Lib.x86.lib",
                link: "Processing.NDI.Lib.x86",
                runtime: "Processing.NDI.Lib.x86.dll",
                edition: "standard",
            },
        ],
        ("ios", _) => &[
            C {
                artifact: "libndi_advanced_ios.a",
                link: "ndi_advanced_ios",
                runtime: "",
                edition: "advanced",
            },
            C {
                artifact: "libndi_ios.a",
                link: "ndi_ios",
                runtime: "",
                edition: "standard",
            },
        ],
        ("tvos", _) => &[
            C {
                artifact: "libndi_advanced_tvos.a",
                link: "ndi_advanced_tvos",
                runtime: "",
                edition: "advanced",
            },
            C {
                artifact: "libndi_tvos.a",
                link: "ndi_tvos",
                runtime: "",
                edition: "standard",
            },
        ],
        ("visionos", _) => &[
            C {
                artifact: "libndi_advanced_visionos.a",
                link: "ndi_advanced_visionos",
                runtime: "",
                edition: "advanced",
            },
            C {
                artifact: "libndi_visionos.a",
                link: "ndi_visionos",
                runtime: "",
                edition: "standard",
            },
        ],
        _ => &[],
    }
}

fn selected_artifact(
    sdk: &Path,
    os: &str,
    arch: &str,
    target: &str,
    options: SelectionOptions<'_>,
) -> Result<(PathBuf, Candidate, String), String> {
    if let Some(edition) = options.edition {
        if !matches!(edition, "standard" | "advanced" | "auto") {
            return Err(format!(
                "Invalid NDI_SDK_EDITION {edition:?}; expected standard, advanced or auto"
            ));
        }
    }
    let subdirs = if let Some(id) = options.profile {
        let profile = profiles()
            .find(|p| p.id == id)
            .ok_or_else(|| format!("Unknown NDI_SDK_PROFILE {id:?}; see build_profiles.tsv"))?;
        if profile.target != target || profile.os != os || profile.arch != arch {
            return Err(format!("NDI_SDK_PROFILE {id} requires {}, not {target}; libc, float ABI and device/simulator must match", profile.target));
        }
        if options
            .edition
            .is_some_and(|e| e != "auto" && !profile.editions.split(',').any(|v| v == e))
        {
            return Err(format!(
                "NDI_SDK_PROFILE {id} does not supply the requested edition"
            ));
        }
        vec![profile.subdir]
    } else {
        library_subdirs(os, arch, target)?
    };
    let lib_root = directory(sdk, "lib", "Lib")?;
    let mut searched = Vec::new();
    for subdir in subdirs {
        let dir = lib_root.join(subdir);
        for &c in candidates(os, arch) {
            if options
                .edition
                .is_some_and(|e| e != "auto" && e != c.edition)
            {
                continue;
            }
            let artifact = dir.join(c.artifact);
            if artifact.is_file() {
                let profile = options.profile.map(str::to_owned).unwrap_or_else(|| {
                    profiles()
                        .find(|p| p.subdir == subdir && p.target == target)
                        .map_or(subdir, |p| p.id)
                        .to_owned()
                });
                return Ok((dir, c, profile));
            }
            searched.push(artifact.display().to_string());
        }
    }
    Err(format!("NDI SDK library for {target} not found. Searched: {}. Set NDI_SDK_DIR to a complete target SDK and NDI_SDK_PROFILE for specialized artifacts.", searched.join(", ")))
}

pub fn resolve_installation(
    sdk: &Path,
    os: &str,
    arch: &str,
    target: &str,
    options: SelectionOptions<'_>,
) -> Result<Installation, String> {
    let include_dir = directory(sdk, "include", "Include")?;
    let header = std::fs::read_dir(&include_dir)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            p.file_name().is_some_and(|n| {
                n.to_string_lossy()
                    .eq_ignore_ascii_case("Processing.NDI.Lib.h")
            })
        })
        .ok_or_else(|| format!("NDI umbrella header not found in {}", include_dir.display()))?;
    let (library_dir, core, profile) = selected_artifact(sdk, os, arch, target, options)?;
    let static_link = matches!(os, "ios" | "tvos" | "visionos");
    let file_library_name = match (os, arch) {
        ("linux", _) => "libndifile.so.6",
        ("android", _) => "libndifile.so",
        ("macos", _) => "libndifile.dylib",
        ("windows", "x86_64") => "Processing.NDI.Lib.File.x64.dll",
        ("ios", _) => "libndifile_ios.a",
        ("tvos", _) => "libndifile_tvos.a",
        ("visionos", _) => "libndifile_visionos.a",
        _ => "",
    }
    .to_owned();
    let core_artifact = library_dir.join(core.artifact);
    let file = library_dir.join(&file_library_name);
    // Mobile File archives already contain the core implementation. Never force
    // both providers into one process. Old static SDKs without File still build.
    let use_file = static_link && options.file_io && file.is_file();
    let link_name = if use_file {
        file_library_name
            .trim_start_matches("lib")
            .trim_end_matches(".a")
            .to_owned()
    } else {
        core.link.to_owned()
    };
    Ok(Installation {
        include_dir,
        header,
        library_dir,
        core_artifact: core_artifact.clone(),
        link_artifact: if use_file { file } else { core_artifact },
        link_name,
        library_name: core.runtime.to_owned(),
        file_library_name,
        edition: core.edition,
        linkage: if static_link { "static" } else { "dynamic" },
        profile,
        static_provider: if use_file { "file" } else { "core" },
    })
}

// Kept as a small pure selector for existing build-selection callers/tests.
#[allow(dead_code)]
pub fn select_library(
    sdk: &Path,
    os: &str,
    arch: &str,
    target: &str,
) -> Result<(PathBuf, &'static str, &'static str), String> {
    let (dir, candidate, _) =
        selected_artifact(sdk, os, arch, target, SelectionOptions::default())?;
    Ok((
        dir,
        candidate.link,
        if matches!(os, "windows" | "ios" | "tvos" | "visionos") {
            "static"
        } else {
            "dylib"
        },
    ))
}

/// Check the actual ELF ABI before giving bindgen/linker a selected shared SDK.
/// Symbol/dependency/CPU-feature acceptance remains a separate profile CI gate.
pub fn validate_elf(path: &Path, os: &str, arch: &str, target: &str) -> Result<(), String> {
    use std::io::Read;
    if !matches!(os, "linux" | "android") {
        return Ok(());
    }
    let mut bytes = [0u8; 52];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| format!("Cannot inspect {}: {e}", path.display()))?;
    let machine = u16::from_le_bytes([bytes[18], bytes[19]]);
    let expected = match arch {
        "x86" => (1, 3),
        "x86_64" => (2, 62),
        "arm" => (1, 40),
        "aarch64" => (2, 183),
        _ => return Err(format!("Unsupported ELF architecture {arch}")),
    };
    if &bytes[..4] != b"\x7fELF" || bytes[5] != 1 || bytes[4] != expected.0 || machine != expected.1
    {
        return Err(format!(
            "SDK artifact {} does not match little-endian {target} ELF ABI",
            path.display()
        ));
    }
    if arch == "arm" {
        let flags = u32::from_le_bytes(bytes[36..40].try_into().unwrap());
        let hard = flags & 0x400 != 0;
        let soft = flags & 0x200 != 0;
        let expects_hard = target.ends_with("eabihf");
        if (expects_hard && (!hard || soft)) || (!expects_hard && hard) {
            return Err(format!(
                "SDK artifact {} has incompatible ARM float calling convention for {target}",
                path.display()
            ));
        }
    }
    Ok(())
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

    #[test]
    fn directory_preserves_actual_entry_spelling_on_every_host() {
        let root = std::env::temp_dir().join(format!(
            "grafton-directory-spelling-test-{}",
            std::process::id()
        ));
        for (index, name) in ["Include", "iNcLuDe", "include"].iter().enumerate() {
            let sdk = root.join(index.to_string());
            let actual = sdk.join(name);
            std::fs::create_dir_all(&actual).unwrap();
            assert_eq!(directory(&sdk, "include", "Include").unwrap(), actual);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn directory_rejects_distinct_matching_aliases() {
        let root = std::env::temp_dir().join(format!(
            "grafton-directory-ambiguity-test-{}",
            std::process::id()
        ));
        // Different alias names let every filesystem represent two distinct
        // candidates, including hosts that cannot create both lib and Lib.
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::create_dir_all(root.join("Libraries")).unwrap();
        assert!(directory(&root, "lib", "Libraries")
            .unwrap_err()
            .contains("Ambiguous SDK layout"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn directory_accepts_same_target_aliases_with_declared_preference() {
        let root = std::env::temp_dir().join(format!(
            "grafton-directory-alias-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("Libraries")).unwrap();
        std::os::unix::fs::symlink("Libraries", root.join("lib")).unwrap();
        assert_eq!(
            directory(&root, "lib", "Libraries").unwrap(),
            root.join("lib")
        );
        assert_eq!(
            directory(&root, "Libraries", "lib").unwrap(),
            root.join("Libraries")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
