// Exercise SDK discovery and static-archive contracts without an installed SDK
// or native runtime.
#[cfg(feature = "advanced")]
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
        "unsafe extern \"C\" { pub fn NDIlib_recv_set_bandwidth(); }",
    );
    assert!(!selection.complete);
    // Exercise the actual macro expansion, including discarded tokens which
    // deliberately refer to nonexistent types. A table family needs all of
    // its functions, while individual constants can be checked independently.
    let source = format!(
        r#"{}
        declared! {{ AbsentType {{ type Broken = NotAType; }} else {{}} }}
        fn main() {{
            assert_eq!(declared! {{ Present {{ 7 }} else {{ 0 }} }}, 7);
            assert!(declared! {{ ReceiverBandwidthControl {{ true }} else {{ false }} }});
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

/// A synthetic SDK installation in a private temporary directory.
struct Sdk(PathBuf);
impl Sdk {
    fn new(name: &str) -> Self {
        let p = std::env::temp_dir().join(format!("grafton-profile-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(p.join("Include")).unwrap();
        fs::write(p.join("Include/Processing.NDI.Lib.h"), "").unwrap();
        Self(p)
    }
    fn artifact(&self, relative: &str) -> PathBuf {
        let p = self.0.join(relative);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, []).unwrap();
        p
    }
    fn root(&self) -> &Path {
        &self.0
    }
    fn located(&self) -> build_support::Sdk {
        build_support::Sdk::open(self.root()).unwrap()
    }
}
impl Drop for Sdk {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
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
fn a_static_edition_is_embedded_in_the_spelling_the_loader_reads() {
    assert_eq!(Edition::Standard.as_str(), "standard");
    assert_eq!(Edition::Advanced.as_str(), "advanced");
}

#[cfg(feature = "advanced")]
#[test]
fn an_installation_is_advanced_when_it_has_the_advanced_header() {
    let sdk = Sdk::new("edition");
    assert!(!sdk.located().is_advanced());
    sdk.artifact("Include/Processing.NDI.Advanced.h");
    assert!(sdk.located().is_advanced());
}

#[cfg(feature = "advanced")]
#[test]
fn only_headers_without_the_mit_grant_are_generated_locally() {
    let sdk = Sdk::new("restricted");
    fs::write(
        sdk.root().join("Include/Processing.NDI.Lib.h"),
        "// Permission is hereby granted, free of charge, to any person\n",
    )
    .unwrap();
    fs::write(
        sdk.root().join("Include/Processing.NDI.Advanced.h"),
        "// This file is part of the NDI Advanced SDK and may not be distributed.\n",
    )
    .unwrap();
    sdk.artifact("Include/notes.txt");
    assert_eq!(
        sdk.located().restricted_headers().unwrap(),
        ["Processing.NDI.Advanced.h"]
    );
}

#[test]
fn only_apple_mobile_targets_link_statically() {
    for os in ["ios", "tvos", "visionos"] {
        assert!(build_support::links_statically(os), "{os}");
    }
    for os in ["linux", "android", "macos", "windows"] {
        assert!(!build_support::links_statically(os), "{os}");
    }
}

#[test]
fn mobile_file_is_the_sole_archive_provider_and_old_sdk_remains_usable() {
    let sdk = Sdk::new("mobile-file");
    let core = sdk.artifact("lib/iOS/libndi_ios.a");
    let selected = build_support::static_library(&sdk.located(), "ios", true).unwrap();
    assert_eq!(selected.archive, core);
    assert_eq!(selected.link_name, "ndi_ios");
    assert_eq!(selected.edition, Edition::Standard);

    let file = sdk.artifact("lib/iOS/libndifile_ios.a");
    let selected = build_support::static_library(&sdk.located(), "ios", true).unwrap();
    assert_eq!(selected.archive, file);
    assert_eq!(selected.link_name, "ndifile_ios");
    // Without the feature the File archive is never linked.
    let selected = build_support::static_library(&sdk.located(), "ios", false).unwrap();
    assert_eq!(selected.archive, core);

    assert!(build_support::static_library(&sdk.located(), "tvos", true).is_err());
    assert!(build_support::static_library(&sdk.located(), "linux", true).is_err());
}

#[test]
fn the_static_archive_of_the_sdk_decides_the_edition() {
    let sdk = Sdk::new("mobile-edition");
    let located = sdk.located();
    // A File archive alone is not a core archive of either edition.
    sdk.artifact("lib/tvOS/libndifile_tvos.a");
    let error = build_support::static_library(&located, "tvos", true).unwrap_err();
    assert!(error.contains("NDI_SDK_DIR"), "{error}");

    let standard = sdk.artifact("lib/tvOS/libndi_tvos.a");
    let selected = build_support::static_library(&located, "tvos", false).unwrap();
    assert_eq!(
        (selected.archive, selected.edition),
        (standard, Edition::Standard)
    );
    let advanced = sdk.artifact("lib/tvOS/libndi_advanced_tvos.a");
    let selected = build_support::static_library(&located, "tvos", false).unwrap();
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
    )
    .unwrap();
    assert!(map.contains("b\"NDIlib_initialize\" => Some(crate::a::NDIlib_initialize as"));
    assert!(map.contains("b\"NDIlib_advanced\" => Some(crate::b::NDIlib_advanced as"));
    assert!(!map.contains("NDIlib_optional"));
    assert!(!map.contains("NDIlib_not_declared"));

    let error = build_support::static_symbol_map(&declarations, &exports(&["NDIlib_advanced"]))
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
        build_apple::inspect(&p, "aarch64", "ios")
            .unwrap()
            .deployment_floor,
        16 << 16
    );
    fs::write(&p, apple_archive(7)).unwrap();
    assert!(build_apple::inspect(&p, "aarch64", "ios")
        .map(|archive| archive.deployment_floor)
        .unwrap_err()
        .contains("device/simulator"));
    fs::write(&p, &apple_archive(2)[..45]).unwrap();
    assert!(build_apple::inspect(&p, "aarch64", "ios").is_err());
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
    let inventory = build_apple::inspect(&p, "aarch64", "ios").unwrap();
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
                let inspected = build_apple::inspect(&source, arch, "ios").unwrap();
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
    build_apple::inspect(&source, "aarch64", "ios")
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
            build_apple::inspect(&source, "aarch64", "ios").is_err(),
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
    build_apple::inspect(&original, "aarch64", "ios")
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
