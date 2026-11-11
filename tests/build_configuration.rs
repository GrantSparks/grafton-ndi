// Exercise target/artifact contracts without an installed SDK or native runtime.
#[path = "../build_apple.rs"]
mod build_apple;
#[path = "../build_support.rs"]
mod build_support;
use build_support::{resolve_installation, SelectionOptions};
use std::{
    fs,
    path::{Path, PathBuf},
};

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
}
impl Drop for Sdk {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn android_windows_host_casing_and_advanced_names_are_resolved() {
    let sdk = Sdk::new("android-casing");
    sdk.artifact("Lib/arm64-v8a/libndi_advanced.so");
    let i = resolve_installation(
        sdk.root(),
        "android",
        "aarch64",
        "aarch64-linux-android",
        SelectionOptions::default(),
    )
    .unwrap();
    assert_eq!(i.edition, "advanced");
    assert_eq!(i.library_name, "libndi_advanced.so");
    assert_eq!(i.include_dir, sdk.root().join("Include"));
    assert_eq!(i.linkage, "dynamic");
    assert_eq!(i.profile, "android-arm64");
}

#[test]
fn windows_and_macos_advanced_are_real_modules_not_renamed_standard() {
    for (name, os, arch, target, relative, runtime) in [
        (
            "winadv",
            "windows",
            "x86_64",
            "x86_64-pc-windows-msvc",
            "Lib/x64/Processing.NDI.Lib.Advanced.x64.lib",
            "Processing.NDI.Lib.Advanced.x64.dll",
        ),
        (
            "macadv",
            "macos",
            "aarch64",
            "aarch64-apple-darwin",
            "lib/macOS/libndi_advanced.dylib",
            "libndi_advanced.dylib",
        ),
    ] {
        let sdk = Sdk::new(name);
        sdk.artifact(relative);
        let i = resolve_installation(sdk.root(), os, arch, target, SelectionOptions::default())
            .unwrap();
        assert_eq!(i.library_name, runtime);
        assert_eq!(i.edition, "advanced");
        assert_eq!(i.linkage, "dynamic");
        assert!(resolve_installation(
            sdk.root(),
            os,
            arch,
            target,
            SelectionOptions {
                edition: Some("standard"),
                ..Default::default()
            }
        )
        .is_err());
    }
}

#[test]
fn embedded_profile_cannot_be_guessed_or_cross_float_libc_boundaries() {
    let sdk = Sdk::new("explicit-profile");
    sdk.artifact("lib/arm-himix100-linux/libndi_advanced.so");
    let target = "armv7-unknown-linux-uclibceabi";
    assert!(resolve_installation(
        sdk.root(),
        "linux",
        "arm",
        target,
        SelectionOptions::default()
    )
    .is_err());
    let options = SelectionOptions {
        profile: Some("arm-himix100-linux"),
        ..Default::default()
    };
    let i = resolve_installation(sdk.root(), "linux", "arm", target, options).unwrap();
    assert_eq!(i.profile, "arm-himix100-linux");
    for wrong in [
        "armv7-unknown-linux-gnueabihf",
        "armv7-unknown-linux-gnueabi",
        "armeb-unknown-linux-gnueabi",
    ] {
        assert!(
            resolve_installation(sdk.root(), "linux", "arm", wrong, options)
                .unwrap_err()
                .contains("requires")
        );
    }
    assert!(resolve_installation(
        sdk.root(),
        "linux",
        "arm",
        target,
        SelectionOptions {
            edition: Some("standard"),
            ..options
        }
    )
    .is_err());
}

#[test]
fn mobile_file_is_the_sole_archive_provider_and_old_sdk_remains_usable() {
    let sdk = Sdk::new("mobile-file");
    sdk.artifact("lib/iOS/libndi_ios.a");
    let options = SelectionOptions {
        file_io: true,
        ..Default::default()
    };
    let core =
        resolve_installation(sdk.root(), "ios", "aarch64", "aarch64-apple-ios", options).unwrap();
    assert_eq!(core.static_provider, "core");
    assert_eq!(core.link_name, "ndi_ios");
    let file = sdk.artifact("lib/iOS/libndifile_ios.a");
    let i =
        resolve_installation(sdk.root(), "ios", "aarch64", "aarch64-apple-ios", options).unwrap();
    assert_eq!(i.static_provider, "file");
    assert_eq!(i.link_artifact, file);
    assert_eq!(i.link_name, "ndifile_ios");
    assert_eq!(i.linkage, "static");
    assert!(i.library_name.is_empty());
    assert!(resolve_installation(
        sdk.root(),
        "ios",
        "aarch64",
        "aarch64-apple-ios-sim",
        options
    )
    .is_err());
    assert!(build_support::library_subdirs("visionos", "x86_64", "x86_64-apple-visionos").is_err());
}

#[test]
fn manifest_is_unique_and_cannot_select_another_edition_silently() {
    let profiles: Vec<_> = build_support::profiles().collect();
    let ids: std::collections::HashSet<_> = profiles.iter().map(|p| p.id).collect();
    assert_eq!(profiles.len(), ids.len());
    assert_eq!(profiles.iter().filter(|p| p.os == "linux").count(), 26);
    assert!(profiles
        .iter()
        .all(|p| !p.target.contains("musl") && !p.target.contains("aarch64_be")));
    let sdk = Sdk::new("edition");
    sdk.artifact("lib/x86_64-linux-gnu/libndi.so");
    sdk.artifact("lib/x86_64-linux-gnu/libndi_advanced.so");
    let i = resolve_installation(
        sdk.root(),
        "linux",
        "x86_64",
        "x86_64-unknown-linux-gnu",
        SelectionOptions {
            edition: Some("standard"),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(i.edition, "standard");
    assert_eq!(i.library_name, "libndi.so.6");
    assert!(resolve_installation(
        sdk.root(),
        "linux",
        "x86_64",
        "x86_64-unknown-linux-gnu",
        SelectionOptions {
            edition: Some("typo"),
            ..Default::default()
        }
    )
    .is_err());
}

#[test]
fn elf_validation_rejects_wrong_endian_class_and_arm_float_abi() {
    let sdk = Sdk::new("elf-abi");
    let p = sdk.artifact("lib/test.so");
    let mut elf = [0u8; 52];
    elf[..4].copy_from_slice(b"\x7fELF");
    elf[4] = 1;
    elf[5] = 1;
    elf[18] = 40;
    elf[36..40].copy_from_slice(&0x0500_0400u32.to_le_bytes());
    fs::write(&p, elf).unwrap();
    assert!(
        build_support::validate_elf(&p, "linux", "arm", "armv7-unknown-linux-gnueabihf").is_ok()
    );
    assert!(
        build_support::validate_elf(&p, "linux", "arm", "armv7-unknown-linux-gnueabi").is_err()
    );
    assert!(
        build_support::validate_elf(&p, "linux", "aarch64", "aarch64-unknown-linux-gnu").is_err()
    );
    elf[5] = 2;
    fs::write(&p, elf).unwrap();
    assert!(
        build_support::validate_elf(&p, "linux", "arm", "armv7-unknown-linux-gnueabihf").is_err()
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
        build_apple::validate(&p, "aarch64", "ios").unwrap(),
        16 << 16
    );
    fs::write(&p, apple_archive(7)).unwrap();
    assert!(build_apple::validate(&p, "aarch64", "ios")
        .unwrap_err()
        .contains("device/simulator"));
    fs::write(&p, &apple_archive(2)[..45]).unwrap();
    assert!(build_apple::validate(&p, "aarch64", "ios").is_err());
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
