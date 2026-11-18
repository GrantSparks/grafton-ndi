// Exercise SDK discovery and static-archive contracts without an installed SDK
// or native runtime.
#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(fixture);

#[path = "../build_advanced.rs"]
mod build_advanced;
#[path = "../build_apple.rs"]
mod build_apple;
#[path = "../build_support.rs"]
mod build_support;
use build_support::{directory, Edition};
use std::{
    collections::{BTreeSet, HashSet},
    fs,
    path::{Path, PathBuf},
};

#[cfg(feature = "advanced")]
#[test]
fn advanced_availability_discards_missing_functions_types_and_constants_before_resolution() {
    let selection = build_advanced::selection(
        "pub const Present: u32 = 7; pub type Handle = *mut u8;",
        "unsafe extern \"C\" {
            pub fn NDIlib_recv_set_bandwidth();
            pub fn NDIlib_recv_set_video_allocator();
        }",
    );
    assert_eq!(
        selection.cfgs,
        ["ndi_has_advanced_receiver_bandwidth_control"]
    );
    assert!(selection
        .cfgs
        .iter()
        .all(|selected| build_advanced::cfgs().any(|known| known == *selected)));
    assert!(build_advanced::selection("", "").cfgs.is_empty());
    // Exercise the actual macro expansion, including discarded tokens which
    // deliberately refer to nonexistent types. A table family needs all of
    // its functions, while individual constants can be checked independently.
    let source = format!(
        r#"{}
        declared! {{ AbsentType {{ type Broken = NotAType; }} else {{}} }}
        fn main() {{
            assert_eq!(declared! {{ Present {{ 7 }} else {{ 0 }} }}, 7);
            assert!(declared! {{ ReceiverBandwidthControl {{ true }} else {{ false }} }});
            assert!(!declared! {{ ReceiveAllocators {{ NotAType::missing() }} else {{ false }} }});
            assert!(!declared! {{ ScatterSend {{ NotAType::missing() }} else {{ false }} }});
            assert!(!declared! {{ Genlock {{ NotAType::missing() }} else {{ false }} }});
        }}"#,
        selection.source
    );
    let fixture = Sdk::new("advanced-availability");
    let path = fixture.root().join("selector.rs");
    fs::write(&path, source).unwrap();
    let output = std::process::Command::new("rustc")
        .args(["--edition=2021", "--deny=warnings"])
        .arg(&path)
        .arg("-o")
        .arg(fixture.root().join("selector"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(std::process::Command::new(fixture.root().join("selector"))
        .status()
        .unwrap()
        .success());
}

#[test]
fn unavailable_advanced_selection_discards_every_declaration() {
    let fixture = Sdk::new("unavailable-advanced-selection");
    let path = fixture.root().join("selector.rs");
    fs::write(&path, format!(r#"{}
        fn main() {{
            assert!(!declared! {{ ReceiveAllocators {{ NotAType::missing() }} else {{ false }} }});
            assert!(!declared! {{ NDIlib_recv_create_v4 {{ NotAType::missing() }} else {{ false }} }});
        }}"#, build_advanced::unavailable_selection())).unwrap();
    assert_eq!(build_advanced::cfgs().count(), 23);
    let executable = fixture.root().join("selector");
    let output = std::process::Command::new("rustc")
        .args(["--edition=2021", "--deny=warnings"])
        .arg(&path)
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(std::process::Command::new(executable)
        .status()
        .unwrap()
        .success());
}

/// A synthetic SDK installation in a private temporary directory.
struct Sdk(common::FixtureDir);
impl Sdk {
    fn new(name: &str) -> Self {
        let root = common::FixtureDir::create(name);
        fs::create_dir_all(root.join("Include")).unwrap();
        fs::write(root.join("Include/Processing.NDI.Lib.h"), "").unwrap();
        Self(root)
    }
    fn artifact(&self, relative: &str) -> PathBuf {
        let p = self.0.join(relative);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, []).unwrap();
        p
    }
    fn root(&self) -> &Path {
        self.0.path()
    }
    fn located(&self) -> build_support::Sdk {
        build_support::Sdk::open(self.root()).unwrap()
    }
}

#[test]
fn an_sdk_is_optional_but_an_explicit_root_must_be_one() {
    // No default installation exists for this operating system.
    assert_eq!(build_support::Sdk::locate(None, "freebsd"), Ok(None));
    assert!(build_support::default_sdk_roots("freebsd").is_empty());
    for os in ["linux", "macos", "windows"] {
        assert!(!build_support::default_sdk_roots(os).is_empty(), "{os}");
    }

    let sdk = Sdk::new("locate");
    let located = build_support::Sdk::locate(Some(sdk.root()), "linux")
        .unwrap()
        .unwrap();
    assert_eq!(located.root, sdk.root());
    assert_eq!(located.include_dir, sdk.root().join("Include"));

    let not_an_sdk = sdk.root().join("Include");
    let error = build_support::Sdk::locate(Some(&not_an_sdk), "linux").unwrap_err();
    assert!(error.contains("NDI_SDK_DIR"), "{error}");
    fs::remove_file(sdk.root().join("Include/Processing.NDI.Lib.h")).unwrap();
    assert!(build_support::Sdk::locate(Some(sdk.root()), "linux").is_err());
}

#[test]
fn the_selected_static_archives_generate_typed_runtime_editions() {
    let sdk = Sdk::new("generated-edition");
    let declarations = [("crate::sys", "pub fn NDIlib_initialize() -> bool;")];
    let exports = HashSet::from(["NDIlib_initialize".to_owned()]);
    for (archive, variant) in [
        ("lib/iOS/libndi_ios.a", "Standard"),
        ("lib/iOS/libndi_advanced_ios.a", "Advanced"),
    ] {
        sdk.artifact(archive);
        let selected =
            build_support::static_library(&sdk.located(), apple_target("ios", "aarch64"), false)
                .unwrap();
        let source =
            build_support::static_symbol_map(&declarations, &exports, selected.edition).unwrap();
        assert!(source.contains(&format!(
            "const LINKED_EDITION: SdkEdition = SdkEdition::{variant};"
        )));
        syn::parse_file(&source).unwrap();
    }
}

#[test]
fn an_installation_is_advanced_when_it_has_the_advanced_header() {
    let sdk = Sdk::new("edition");
    assert!(!sdk.located().is_advanced());
    sdk.artifact("Include/Processing.NDI.Advanced.h");
    assert!(sdk.located().is_advanced());
}

#[test]
fn only_headers_without_the_mit_grant_are_generated_locally() {
    let sdk = Sdk::new("restricted");
    fs::write(
        sdk.root().join("Include/Processing.NDI.Lib.h"),
        format!("// {}, to any person\n", build_support::MIT_GRANT),
    )
    .unwrap();
    fs::write(
        sdk.root().join("Include/Processing.NDI.Advanced.h"),
        "// This file is part of the NDI Advanced SDK and may not be distributed.\n",
    )
    .unwrap();
    fs::write(
        sdk.root().join("Include/Processing.NDI.Send.h"),
        format!("// {}, to any person\n", build_support::MIT_GRANT),
    )
    .unwrap();
    sdk.artifact("Include/Processing.NDI.Private.h");
    sdk.artifact("Include/notes.txt");
    let headers = build_support::classify_headers(&sdk.located().include_dir).unwrap();
    assert_eq!(
        headers.licensed,
        ["Processing.NDI.Lib.h", "Processing.NDI.Send.h"]
    );
    assert_eq!(
        headers.restricted,
        ["Processing.NDI.Advanced.h", "Processing.NDI.Private.h"]
    );
}

#[test]
fn header_patterns_escape_every_separator() {
    assert_eq!(
        build_support::header_pattern("Processing.NDI.Lib.h"),
        r".*[/\\]Processing\.NDI\.Lib\.h"
    );
}

#[test]
fn only_apple_mobile_targets_link_statically() {
    assert_eq!(
        build_support::clang_target("aarch64-apple-visionos"),
        "aarch64-apple-xros"
    );
    for os in ["ios", "tvos", "visionos"] {
        assert!(build_support::links_statically(os), "{os}");
    }
    for os in ["linux", "android", "macos", "windows"] {
        assert!(!build_support::links_statically(os), "{os}");
    }
}

fn apple_target(os: &str, arch: &str) -> &'static build_support::StaticTarget {
    build_support::STATIC_TARGETS
        .iter()
        .find(|target| (target.os, target.arch) == (os, arch))
        .unwrap()
}

fn inspect_apple(path: &Path, arch: &str, os: &str) -> Result<build_apple::StaticArchive, String> {
    let target = apple_target(os, arch);
    build_apple::inspect(path, target.arch, target.platform)
}

#[test]
fn static_targets_keep_each_supported_slice_and_deployment_variable() {
    let supported = [
        (
            "aarch64-apple-ios",
            "ios",
            "aarch64",
            "",
            "iOS",
            2,
            "IPHONEOS_DEPLOYMENT_TARGET",
        ),
        (
            "x86_64-apple-ios",
            "ios",
            "x86_64",
            "sim",
            "iOS",
            7,
            "IPHONEOS_DEPLOYMENT_TARGET",
        ),
        (
            "aarch64-apple-tvos",
            "tvos",
            "aarch64",
            "",
            "tvOS",
            3,
            "TVOS_DEPLOYMENT_TARGET",
        ),
        (
            "x86_64-apple-tvos",
            "tvos",
            "x86_64",
            "sim",
            "tvOS",
            8,
            "TVOS_DEPLOYMENT_TARGET",
        ),
        (
            "aarch64-apple-visionos",
            "visionos",
            "aarch64",
            "",
            "visionOS",
            11,
            "XROS_DEPLOYMENT_TARGET",
        ),
    ];
    assert_eq!(build_support::STATIC_TARGETS.len(), supported.len());
    for (triple, os, arch, abi, subdirectory, platform, variable) in supported {
        let target = build_support::static_target(triple, os, arch, abi).unwrap();
        assert_eq!(target.triple, triple);
        assert_eq!(
            (
                target.sdk_subdirectory,
                target.platform,
                target.deployment_variable
            ),
            (subdirectory, platform, variable),
            "{triple}"
        );
        // Pin the ABI rustc actually gives Cargo, including Intel simulators.
        let cfg = std::process::Command::new("rustc")
            .args(["--print", "cfg", "--target", triple])
            .output()
            .unwrap();
        assert!(
            cfg.status.success(),
            "{}",
            String::from_utf8_lossy(&cfg.stderr)
        );
        assert!(
            String::from_utf8_lossy(&cfg.stdout)
                .lines()
                .any(|line| line == format!("target_abi=\"{abi}\"")),
            "{triple}"
        );
    }
    assert_eq!(
        build_support::static_deployment_variables().collect::<Vec<_>>(),
        [
            "IPHONEOS_DEPLOYMENT_TARGET",
            "TVOS_DEPLOYMENT_TARGET",
            "XROS_DEPLOYMENT_TARGET"
        ]
    );
}

#[test]
fn static_target_cfg_must_agree_with_its_exact_triple() {
    for (os, arch, abi) in [
        ("tvos", "aarch64", ""),
        ("ios", "x86_64", ""),
        ("ios", "aarch64", "sim"),
    ] {
        let error = build_support::static_target("aarch64-apple-ios", os, arch, abi).unwrap_err();
        assert!(error.contains("aarch64-apple-ios has cfg"), "{error}");
        assert!(
            error.contains("table row requires (ios, aarch64"),
            "{error}"
        );
    }
}

#[test]
fn unsupported_apple_abis_fail_before_the_build_script_opens_an_sdk() {
    let fixture = Sdk::new("unsupported-apple-build");
    let executable = fixture
        .root()
        .join(format!("build-script{}", std::env::consts::EXE_SUFFIX));
    let compiled = std::process::Command::new("rustc")
        .args(["--edition=2021", "--deny=warnings", "build.rs", "-o"])
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    for (triple, os, arch, abi) in [
        ("arm64e-apple-ios", "ios", "aarch64", ""),
        ("arm64e-apple-tvos", "tvos", "aarch64", ""),
        ("aarch64-apple-ios-sim", "ios", "aarch64", "sim"),
        ("aarch64-apple-tvos-sim", "tvos", "aarch64", "sim"),
        ("aarch64-apple-visionos-sim", "visionos", "aarch64", "sim"),
        ("aarch64-apple-ios-macabi", "ios", "aarch64", "macabi"),
        ("x86_64-apple-ios-macabi", "ios", "x86_64", "macabi"),
    ] {
        let cfg = std::process::Command::new("rustc")
            .args(["--print", "cfg", "--target", triple])
            .output()
            .unwrap();
        assert!(
            cfg.status.success(),
            "{}",
            String::from_utf8_lossy(&cfg.stderr)
        );
        assert!(
            String::from_utf8_lossy(&cfg.stdout)
                .lines()
                .any(|line| line == format!("target_abi=\"{abi}\"")),
            "{triple}"
        );
        let output = std::process::Command::new(&executable)
            .env("TARGET", triple)
            .env("CARGO_CFG_TARGET_OS", os)
            .env("CARGO_CFG_TARGET_ARCH", arch)
            .env("CARGO_CFG_TARGET_ABI", abi)
            .env("OUT_DIR", fixture.root())
            // This deliberately invalid path must never be consulted.
            .env("NDI_SDK_DIR", fixture.root().join("missing-sdk"))
            .output()
            .unwrap();
        assert!(!output.status.success(), "{triple}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains(&format!("Unsupported Apple static target {triple};")),
            "{error}"
        );
        assert!(
            error.contains("docs/setup.md#apple-mobile-targets"),
            "{error}"
        );
        assert!(!error.contains("NDI_SDK_DIR is not an NDI SDK"), "{error}");
    }
    // A supported target reaches SDK discovery after emitting all deployment
    // inputs from the same table; it must not take the unsupported-ABI path.
    let output = std::process::Command::new(&executable)
        .env("TARGET", "aarch64-apple-ios")
        .env("CARGO_CFG_TARGET_OS", "ios")
        .env("CARGO_CFG_TARGET_ARCH", "aarch64")
        .env("CARGO_CFG_TARGET_ABI", "")
        .env("OUT_DIR", fixture.root())
        .env("NDI_SDK_DIR", fixture.root().join("missing-sdk"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("NDI_SDK_DIR is not an NDI SDK"), "{error}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for target in build_support::STATIC_TARGETS {
        assert!(
            stdout
                .lines()
                .any(|line| line
                    == format!("cargo:rerun-if-env-changed={}", target.deployment_variable)),
            "{stdout}"
        );
    }
}

#[test]
fn mobile_file_is_the_sole_archive_provider_and_old_sdk_remains_usable() {
    let sdk = Sdk::new("mobile-file");
    let core = sdk.artifact("lib/iOS/libndi_ios.a");
    let selected =
        build_support::static_library(&sdk.located(), apple_target("ios", "aarch64"), true)
            .unwrap();
    assert_eq!(selected.archive, core);
    assert_eq!(selected.link_name, "ndi_ios");
    assert_eq!(selected.edition, Edition::Standard);

    let file = sdk.artifact("lib/iOS/libndifile_ios.a");
    let selected =
        build_support::static_library(&sdk.located(), apple_target("ios", "aarch64"), true)
            .unwrap();
    assert_eq!(selected.archive, file);
    assert_eq!(selected.link_name, "ndifile_ios");
    // Without the feature the File archive is never linked.
    let selected =
        build_support::static_library(&sdk.located(), apple_target("ios", "aarch64"), false)
            .unwrap();
    assert_eq!(selected.archive, core);

    assert!(
        build_support::static_library(&sdk.located(), apple_target("tvos", "aarch64"), true)
            .is_err()
    );
    assert!(
        build_support::static_target("x86_64-unknown-linux-gnu", "linux", "x86_64", "").is_err()
    );
}

#[test]
fn the_static_archive_of_the_sdk_decides_the_edition() {
    let sdk = Sdk::new("mobile-edition");
    let located = sdk.located();
    // A File archive alone is not a core archive of either edition.
    sdk.artifact("lib/tvOS/libndifile_tvos.a");
    let error =
        build_support::static_library(&located, apple_target("tvos", "aarch64"), true).unwrap_err();
    assert!(error.contains("NDI_SDK_DIR"), "{error}");

    let standard = sdk.artifact("lib/tvOS/libndi_tvos.a");
    let selected =
        build_support::static_library(&located, apple_target("tvos", "aarch64"), false).unwrap();
    assert_eq!(
        (selected.archive, selected.edition),
        (standard, Edition::Standard)
    );
    let advanced = sdk.artifact("lib/tvOS/libndi_advanced_tvos.a");
    let selected =
        build_support::static_library(&located, apple_target("tvos", "aarch64"), false).unwrap();
    assert_eq!(
        (selected.archive, selected.edition),
        (advanced, Edition::Advanced)
    );
}

#[test]
fn declaration_scan_agrees_with_a_full_parse_of_the_vendored_declarations() {
    let bindings = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/sys/declarations.rs"
    ))
    .unwrap();
    let mut parsed = BTreeSet::new();
    for item in syn::parse_file(&bindings).unwrap().items {
        let syn::Item::ForeignMod(block) = item else {
            continue;
        };
        for item in block.items {
            if let syn::ForeignItem::Fn(function) = item {
                parsed.insert(function.sig.ident.to_string());
            }
        }
    }
    assert!(parsed.contains("NDIlib_initialize"));
    let scanned: BTreeSet<String> = build_support::declared_functions(&bindings)
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_eq!(scanned, parsed);
}

#[test]
fn static_map_requires_both_a_declaration_and_an_actual_archive_export() {
    let common = "unsafe extern \"C\" {\n    pub fn NDIlib_initialize() -> bool;\n}\nunsafe extern \"C\" {\n    pub fn NDIlib_optional(\n        value: i32,\n    );\n}\n";
    let advanced = "unsafe extern \"C\" {\n    pub fn NDIlib_advanced();\n}\n";
    let declarations = [("crate::a", common), ("crate::b", advanced)];
    let exports = |names: &[&str]| -> HashSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    };

    let map = build_support::static_symbol_map(
        &declarations,
        &exports(&[
            "NDIlib_initialize",
            "NDIlib_advanced",
            "NDIlib_not_declared",
        ]),
        Edition::Standard,
    )
    .unwrap();
    assert!(map.contains("b\"NDIlib_initialize\" => Some(crate::a::NDIlib_initialize as"));
    assert!(map.contains("b\"NDIlib_advanced\" => Some(crate::b::NDIlib_advanced as"));
    assert!(!map.contains("NDIlib_optional"));
    assert!(!map.contains("NDIlib_not_declared"));

    let error = build_support::static_symbol_map(
        &declarations,
        &exports(&["NDIlib_advanced"]),
        Edition::Standard,
    )
    .unwrap_err();
    assert!(error.contains("NDIlib_initialize"), "{error}");
}

#[test]
fn directory_preserves_actual_entry_spelling_on_every_host() {
    let sdk = Sdk::new("directory-spelling");
    for (index, name) in ["Include", "iNcLuDe", "include"].iter().enumerate() {
        let root = sdk.root().join(index.to_string());
        let actual = root.join(name);
        fs::create_dir_all(&actual).unwrap();
        assert_eq!(directory(&root, "include", "Include").unwrap(), actual);
    }
    assert!(directory(&sdk.root().join("0"), "lib", "Lib")
        .unwrap_err()
        .contains("SDK directory missing"));
}

#[test]
fn directory_rejects_distinct_matching_aliases() {
    let sdk = Sdk::new("directory-ambiguity");
    // Different alias names let every filesystem represent two distinct
    // candidates, including hosts that cannot create both lib and Lib.
    fs::create_dir_all(sdk.root().join("lib")).unwrap();
    fs::create_dir_all(sdk.root().join("Libraries")).unwrap();
    assert!(directory(sdk.root(), "lib", "Libraries")
        .unwrap_err()
        .contains("Ambiguous SDK layout"));
}

#[cfg(unix)]
#[test]
fn directory_accepts_same_target_aliases_with_declared_preference() {
    let sdk = Sdk::new("directory-alias");
    fs::create_dir_all(sdk.root().join("Libraries")).unwrap();
    std::os::unix::fs::symlink("Libraries", sdk.root().join("lib")).unwrap();
    assert_eq!(
        directory(sdk.root(), "lib", "Libraries").unwrap(),
        sdk.root().join("lib")
    );
    assert_eq!(
        directory(sdk.root(), "Libraries", "lib").unwrap(),
        sdk.root().join("Libraries")
    );
}

fn apple_archive(platform: u32) -> Vec<u8> {
    let mut object = vec![0u8; 56];
    for (offset, value) in [
        (0, 0xfeed_facf),
        (4, 0x0100_000c),
        (16, 1),
        (20, 24),
        (32, 0x32),
        (36, 24),
        (40, platform),
        (44, 16 << 16),
    ] {
        object[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    let mut data = b"!<arch>\n".to_vec();
    let header = format!(
        "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
        "object.o/",
        "0",
        "0",
        "0",
        "644",
        object.len()
    );
    assert_eq!(header.len(), 60);
    data.extend(header.as_bytes());
    data.extend(object);
    data
}

#[test]
fn apple_static_validator_distinguishes_device_from_simulator_not_just_cpu() {
    let sdk = Sdk::new("apple-slice");
    let p = sdk.artifact("lib/iOS/test.a");
    fs::write(&p, apple_archive(2)).unwrap();
    assert_eq!(
        inspect_apple(&p, "aarch64", "ios")
            .unwrap()
            .deployment_floor,
        16 << 16
    );
    fs::write(&p, apple_archive(7)).unwrap();
    assert!(inspect_apple(&p, "aarch64", "ios")
        .map(|archive| archive.deployment_floor)
        .unwrap_err()
        .contains("device/simulator"));
    // Match the expected platform even when the object CPU itself is right.
    let simulator = apple_target("ios", "x86_64");
    fs::write(&p, apple_cpu_archive(0x0100_0007, 2)).unwrap();
    assert!(build_apple::inspect(&p, simulator.arch, simulator.platform)
        .unwrap_err()
        .contains("device/simulator"));
    fs::write(&p, apple_cpu_archive(0x0100_0007, 7)).unwrap();
    assert!(build_apple::inspect(&p, simulator.arch, simulator.platform).is_ok());
    fs::write(&p, &apple_archive(2)[..45]).unwrap();
    assert!(inspect_apple(&p, "aarch64", "ios").is_err());
}

#[test]
fn static_addresses_include_only_defined_external_symbols() {
    let sdk = Sdk::new("apple-symbols");
    let p = sdk.artifact("lib/iOS/symbols.a");
    let strings = b"\0_NDIlib_initialize\0_NDIlib_optional_missing\0_NDIlib_private\0";
    let mut object = vec![0u8; 128];
    object[..56].copy_from_slice(&apple_archive(2)[68..]);
    for (offset, value) in [
        (16, 2u32),
        (20, 48),
        (56, 2),
        (60, 24),
        (64, 80),
        (68, 3),
        (72, 128),
        (76, strings.len() as u32),
    ] {
        object[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    for (offset, name, kind) in [
        (80, "_NDIlib_initialize", 0x0f),
        (96, "_NDIlib_optional_missing", 0x01),
        (112, "_NDIlib_private", 0x0e),
    ] {
        let index = strings
            .windows(name.len())
            .position(|s| s == name.as_bytes())
            .unwrap() as u32;
        object[offset..offset + 4].copy_from_slice(&index.to_le_bytes());
        object[offset + 4] = kind;
    }
    object.extend(strings);
    let mut archive = b"!<arch>\n".to_vec();
    let header = format!(
        "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
        "object.o/",
        "0",
        "0",
        "0",
        "644",
        object.len()
    );
    archive.extend(header.as_bytes());
    archive.extend(&object);
    if object.len() % 2 == 1 {
        archive.push(b'\n');
    }
    fs::write(&p, archive).unwrap();
    let inventory = inspect_apple(&p, "aarch64", "ios").unwrap();
    assert_eq!(
        inventory.symbols,
        std::collections::HashSet::from(["NDIlib_initialize".to_owned()])
    );
}

fn apple_cpu_archive(cpu: u32, platform: u32) -> Vec<u8> {
    let mut archive = apple_archive(platform);
    archive[72..76].copy_from_slice(&cpu.to_le_bytes());
    archive
}

fn fat_apple_archives(slices: &[(u32, Vec<u8>)], wide: bool, big: bool) -> Vec<u8> {
    fn integer(data: &mut [u8], at: usize, value: u64, bytes: usize, big: bool) {
        let encoded = if big {
            value.to_be_bytes()
        } else {
            value.to_le_bytes()
        };
        data[at..at + bytes].copy_from_slice(if big {
            &encoded[8 - bytes..]
        } else {
            &encoded[..bytes]
        });
    }
    let stride = if wide { 32 } else { 20 };
    let mut fat = vec![0; 8 + slices.len() * stride];
    integer(
        &mut fat,
        0,
        if wide { 0xcafe_babf } else { 0xcafe_babe },
        4,
        big,
    );
    integer(&mut fat, 4, slices.len() as u64, 4, big);
    for (index, (cpu, archive)) in slices.iter().enumerate() {
        let base = 8 + index * stride;
        let offset = fat.len() as u64;
        integer(&mut fat, base, u64::from(*cpu), 4, big);
        integer(&mut fat, base + 8, offset, if wide { 8 } else { 4 }, big);
        integer(
            &mut fat,
            base + if wide { 16 } else { 12 },
            archive.len() as u64,
            if wide { 8 } else { 4 },
            big,
        );
        fat.extend(archive);
    }
    fat
}

#[test]
fn apple_thinning_selects_validated_bytes_for_both_fat_widths_and_byte_orders() {
    let sdk = Sdk::new("apple-fat-materialization");
    let source = sdk.artifact("original.a");
    let output = sdk.artifact("selected.a");
    let arm = apple_cpu_archive(0x0100_000c, 2);
    let intel = apple_cpu_archive(0x0100_0007, 7);
    for wide in [false, true] {
        for big in [false, true] {
            for (arch, expected) in [("aarch64", &arm), ("x86_64", &intel)] {
                let original = fat_apple_archives(
                    &[(0x0100_0007, intel.clone()), (0x0100_000c, arm.clone())],
                    wide,
                    big,
                );
                fs::write(&source, &original).unwrap();
                let inspected = inspect_apple(&source, arch, "ios").unwrap();
                assert_eq!(inspected.deployment_floor, 16 << 16);
                inspected.write_thin(&output).unwrap();
                assert_eq!(fs::read(&output).unwrap(), *expected);
                assert_eq!(fs::read(&source).unwrap(), original);
                // Materialization must use the bytes that were inspected, not
                // reread an SDK path that changed after validation.
                fs::write(&source, b"changed after inspection").unwrap();
                inspected.write_thin(&output).unwrap();
                assert_eq!(fs::read(&output).unwrap(), *expected);
            }
        }
    }
    fs::write(&source, &arm).unwrap();
    inspect_apple(&source, "aarch64", "ios")
        .unwrap()
        .write_thin(&output)
        .unwrap();
    assert_eq!(fs::read(output).unwrap(), arm);
}

#[test]
fn apple_fat_tables_reject_ambiguous_out_of_bounds_and_mismatched_slices() {
    let sdk = Sdk::new("apple-fat-invalid");
    let source = sdk.artifact("original.a");
    let arm = apple_cpu_archive(0x0100_000c, 2);
    let intel = apple_cpu_archive(0x0100_0007, 7);
    let valid = fat_apple_archives(
        &[(0x0100_000c, arm.clone()), (0x0100_0007, intel.clone())],
        true,
        true,
    );
    let mut bad = Vec::new();
    let mut count = valid.clone();
    count[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
    bad.push(count);
    let mut table_overlap = valid.clone();
    table_overlap[16..24].copy_from_slice(&8u64.to_be_bytes());
    bad.push(table_overlap);
    let mut overflow = valid.clone();
    overflow[16..24].copy_from_slice(&u64::MAX.to_be_bytes());
    bad.push(overflow);
    let mut oversized = valid.clone();
    oversized[24..32].copy_from_slice(&u64::MAX.to_be_bytes());
    bad.push(oversized);
    let mut overlap = valid.clone();
    overlap[48..56].copy_from_slice(&valid[16..24]);
    bad.push(overlap);
    let mut alignment = valid.clone();
    alignment[32..36].copy_from_slice(&64u32.to_be_bytes());
    bad.push(alignment);
    let mut reserved = valid.clone();
    reserved[36..40].copy_from_slice(&1u32.to_be_bytes());
    bad.push(reserved);
    bad.push(valid[..63].to_vec());
    bad.push(fat_apple_archives(
        &[(0x0100_000c, arm.clone()), (0x0100_000c, arm.clone())],
        false,
        true,
    ));
    bad.push(fat_apple_archives(
        &[(0x0100_0007, intel.clone())],
        false,
        false,
    ));
    bad.push(fat_apple_archives(&[(0x0100_000c, intel)], true, false));
    bad.push(fat_apple_archives(
        &[(0x0100_000c, apple_cpu_archive(0x0100_000c, 7))],
        false,
        true,
    ));
    for (index, bytes) in bad.iter().enumerate() {
        fs::write(&source, bytes).unwrap();
        assert!(
            inspect_apple(&source, "aarch64", "ios").is_err(),
            "accepted invalid fixture {index}"
        );
    }
}

#[test]
#[cfg(target_os = "linux")]
fn rustc_can_bundle_the_materialized_archive_without_an_apple_sdk() {
    // Native archive bundling does not execute or link the contained Mach-O
    // code. A real rustc rlib build catches accidentally copying the fat wrapper
    // even on the SDK-free Linux policy runner.
    let sdk = Sdk::new("apple-rustc-bundle");
    let original = sdk.artifact("universal.a");
    let thin = sdk.artifact("fixture.a");
    let arm = apple_cpu_archive(0x0100_000c, 2);
    let intel = apple_cpu_archive(0x0100_0007, 7);
    fs::write(
        &original,
        fat_apple_archives(&[(0x0100_000c, arm), (0x0100_0007, intel)], true, false),
    )
    .unwrap();
    inspect_apple(&original, "aarch64", "ios")
        .unwrap()
        .write_thin(&thin)
        .unwrap();
    let source = sdk.root().join("probe.rs");
    fs::write(
        &source,
        "#![no_std]\n#[link(name=\"fixture.a\",kind=\"static\",modifiers=\"+verbatim\")]\nextern \"C\" {}\npub fn probe() {}\n",
    )
    .unwrap();
    let result = std::process::Command::new("rustc")
        .args(["--edition=2021", "--crate-type=rlib", "--emit=link"])
        .arg(format!("-Lnative={}", sdk.root().display()))
        .arg(source)
        .arg("-o")
        .arg(sdk.root().join("probe.rlib"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
