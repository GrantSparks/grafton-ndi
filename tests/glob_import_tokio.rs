//! A root glob import must coexist with common external crate names.
//!
//! The generated consumer supplies real external crates named `image`, `tokio`
//! and `smol`. Its unqualified imports must resolve without explicit aliases
//! that could hide an ambiguous root export.

#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(fixture);

#[path = "support/nested_cargo.rs"]
mod nested_cargo;

#[cfg(feature = "tokio")]
use grafton_ndi::*;
#[cfg(feature = "tokio")]
use tokio::runtime::Builder;

#[cfg(feature = "tokio")]
#[test]
fn glob_import_leaves_the_tokio_crate_nameable() {
    let runtime = Builder::new_current_thread().build().unwrap();
    assert_eq!(runtime.block_on(async { 1 + 1 }), 2);
    assert_eq!(PixelFormat::Bgra, PixelFormat::Bgra);
    let _ = std::mem::size_of::<asynchronous::tokio::AsyncReceiver>();
}

#[test]
fn root_glob_import_keeps_external_crates_unambiguous() {
    use std::{fs, path::PathBuf};

    let directory = common::FixtureDir::create("glob-import");
    let root = directory.path();
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dependency_path = manifest
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let features = [
        (cfg!(feature = "advanced"), "advanced"),
        (cfg!(feature = "file-io"), "file-io"),
        (cfg!(feature = "image-encoding"), "image-encoding"),
        (cfg!(feature = "tokio"), "tokio"),
        (cfg!(feature = "smol"), "smol"),
    ]
    .into_iter()
    .filter(|(enabled, _)| *enabled)
    .map(|(_, feature)| format!("\"{feature}\""))
    .collect::<Vec<_>>()
    .join(", ");
    let dependency = format!(
        "{{ path = \"{dependency_path}\", default-features = false, features = [{features}] }}"
    );

    let mut members = vec!["valid".to_string()];
    for name in ["image", "tokio", "smol"] {
        let external = root.join(format!("external-{name}"));
        fs::create_dir_all(external.join("src")).unwrap();
        fs::write(
            external.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n"),
        )
        .unwrap();
        fs::write(external.join("src/lib.rs"), "pub const EXTERNAL: u8 = 1;\n").unwrap();
        members.push(format!("external-{name}"));

        let shadow = root.join(format!("shadow-{name}"));
        fs::create_dir_all(shadow.join("src")).unwrap();
        fs::write(
            shadow.join("Cargo.toml"),
            format!("[package]\nname = \"shadow-{name}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[dependencies]\nimplementation = {{ package = \"grafton-ndi\", {}\n", &dependency[1..]),
        )
        .unwrap();
        fs::write(
            shadow.join("src/lib.rs"),
            format!(
                "pub use implementation::*;\npub mod {name} {{ pub const EXTERNAL: u8 = 2; }}\n"
            ),
        )
        .unwrap();
        members.push(format!("shadow-{name}"));
        members.push(format!("bad-{name}"));
    }
    for name in ["valid", "bad-image", "bad-tokio", "bad-smol"] {
        let consumer = root.join(name);
        fs::create_dir_all(consumer.join("src")).unwrap();
        let api = if name == "valid" {
            format!("grafton-ndi = {dependency}")
        } else {
            let shadow = name.strip_prefix("bad-").unwrap();
            format!(
                "grafton-ndi = {{ package = \"shadow-{shadow}\", path = \"../shadow-{shadow}\" }}"
            )
        };
        fs::write(
            consumer.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[dependencies]\n{api}\nimage = {{ path = \"../external-image\" }}\ntokio = {{ path = \"../external-tokio\" }}\nsmol = {{ path = \"../external-smol\" }}\n"),
        )
        .unwrap();
        fs::write(
            consumer.join("src/main.rs"),
            "use grafton_ndi::*;\nuse image::EXTERNAL as IMAGE;\nuse tokio::EXTERNAL as TOKIO;\nuse smol::EXTERNAL as SMOL;\nfn main() { let _ = (PixelFormat::Bgra, IMAGE, TOKIO, SMOL); }\n",
        )
        .unwrap();
    }
    let members = members
        .into_iter()
        .map(|name| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ");
    fs::write(
        root.join("Cargo.toml"),
        format!("[workspace]\nresolver = \"2\"\nmembers = [{members}]\n"),
    )
    .unwrap();
    fs::copy(manifest.join("Cargo.lock"), root.join("Cargo.lock")).unwrap();
    for name in ["valid", "bad-image", "bad-tokio", "bad-smol"] {
        let output = nested_cargo::command(
            "external-consumer",
            "check",
            Some(env!("GRAFTON_NDI_TARGET")),
            Some(root),
        )
        .args(["--package", name])
        .output()
        .expect("compile the glob import consumer");
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        if name == "valid" {
            assert!(
                output.status.success(),
                "external crate names became ambiguous:\n{diagnostics}"
            );
        } else {
            assert!(
                !output.status.success(),
                "the fixture accepted an ambiguous root export: {name}"
            );
            assert!(
                diagnostics.contains("E0659"),
                "the fixture failed for a different reason:\n{diagnostics}"
            );
        }
    }
}
