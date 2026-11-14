//! What the search reports when the platform loader answers a request for a
//! library name with a module that is already loaded.
//!
//! A library that is opened and then rejected stays loaded. The loader matches
//! a later request for a bare name against the names loaded modules carry, so
//! it returns the rejected module again instead of searching its path.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

#[path = "support/fixture.rs"]
mod fixture;
#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;

use fixture::FixtureDir;
use grafton_ndi::{
    capabilities::SdkEdition, runtime::RuntimeOptions, Error, LibraryKind, LoadFailure, LoadOrigin,
    LoadRefusal, Ndi,
};
use runtime_fixture::FakeRuntime;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// The name the Standard runtime is searched for and carries as its SONAME.
const NAME: &str = "libndi.so.6";

/// A scenario with its `NDI_RUNTIME_DIR_V6` and `LD_LIBRARY_PATH` directories.
const SCENARIOS: &[(&str, Option<&str>, &str)] = &[
    (
        "rejected_module_shadows_the_loader_search",
        Some("rejected"),
        "usable",
    ),
    (
        "loader_search_reports_the_file_it_resolved",
        None,
        "rejected",
    ),
    (
        "module_that_cannot_be_identified_shadows_the_loader_search",
        Some("anchorless"),
        "usable",
    ),
];

#[test]
fn loader_search_reports_what_the_platform_resolved() {
    if let Ok(name) = std::env::var("GRAFTON_SHADOW_CHILD") {
        let root = PathBuf::from(std::env::var_os("GRAFTON_SHADOW_FIXTURES").unwrap());
        child(&name, &root);
        return;
    }
    let fixtures = FixtureDir::create("shadow");
    let root = fixtures.path();
    for directory in ["rejected", "anchorless", "usable"] {
        fs::create_dir(root.join(directory)).unwrap();
    }
    // Different libraries that carry the same name.
    FakeRuntime {
        cfgs: vec!["fake_omit_required"],
        soname: Some(NAME),
        ..Default::default()
    }
    .build(&root.join("rejected").join(NAME));
    FakeRuntime {
        soname: Some(NAME),
        ..Default::default()
    }
    .build(&root.join("usable").join(NAME));
    // One that is no NDI runtime at all: it lacks even the symbol a runtime
    // is identified by.
    let anchorless = root.join("anchorless").join(NAME);
    let mut rustc = runtime_fixture::rustc("fake_dependency.rs", &anchorless);
    rustc.arg(format!("-Clink-arg=-Wl,-soname,{NAME}"));
    runtime_fixture::run(rustc, &anchorless);

    for (scenario, runtime_directory, loader_directory) in SCENARIOS {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "loader_search_reports_what_the_platform_resolved",
                "--nocapture",
            ])
            .env("GRAFTON_SHADOW_CHILD", scenario)
            .env("GRAFTON_SHADOW_FIXTURES", root)
            .env_remove("NDI_RUNTIME_DIR_V6")
            .env("LD_LIBRARY_PATH", root.join(loader_directory));
        if let Some(directory) = runtime_directory {
            command.env("NDI_RUNTIME_DIR_V6", root.join(directory));
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{scenario}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn standard() -> RuntimeOptions {
    RuntimeOptions::default().edition(SdkEdition::Standard)
}

fn load_failure() -> LoadFailure {
    match Ndi::with_options(&standard()) {
        Err(Error::RuntimeLoadFailed(reason)) => {
            assert_eq!(reason.library(), LibraryKind::Runtime);
            assert!(reason.loaded_any());
            reason
        }
        Err(other) => panic!("expected a load failure, got {other}"),
        Ok(ndi) => panic!("expected a load failure, but loaded {:?}", ndi.info()),
    }
}

fn child(name: &str, root: &Path) {
    let rejected = root.join("rejected").join(NAME);
    let usable = root.join("usable").join(NAME);
    let missing_entry_point = LoadRefusal::MissingEntryPoint {
        symbol: "NDIlib_send_set_failover".into(),
        required: true,
        detail: None,
    };
    match name {
        "rejected_module_shadows_the_loader_search" => {
            let reason = load_failure();
            let environment = reason
                .attempts()
                .iter()
                .find(|a| a.origin() == LoadOrigin::RuntimeDirectory)
                .unwrap();
            assert_eq!(environment.location(), rejected);
            assert_eq!(environment.refusal(), &missing_entry_point);
            assert!(environment.loaded());
            let loader = reason.attempts().last().unwrap();
            assert_eq!(loader.origin(), LoadOrigin::LoaderSearch);
            assert_eq!(
                loader.refusal(),
                &LoadRefusal::AlreadyRefused {
                    path: rejected.clone(),
                    refusal: Box::new(missing_entry_point.clone())
                }
            );
            // The usable library was never reached by name; naming its file
            // reaches it.
            let ndi = Ndi::with_options(&standard().library_path(usable.clone())).unwrap();
            assert_eq!(ndi.info().library_path(), Some(usable.as_path()));
        }
        "loader_search_reports_the_file_it_resolved" => {
            let reason = load_failure();
            let loader = reason.attempts().last().unwrap();
            assert_eq!(loader.origin(), LoadOrigin::LoaderSearch);
            assert_eq!(loader.resolved_path(), Some(rejected.as_path()));
            assert_eq!(loader.refusal(), &missing_entry_point);
            let again = load_failure();
            assert_eq!(
                again.attempts().last().unwrap().refusal(),
                &LoadRefusal::AlreadyRefused {
                    path: rejected.clone(),
                    refusal: Box::new(missing_entry_point.clone())
                }
            );
        }
        "module_that_cannot_be_identified_shadows_the_loader_search" => {
            let anchorless = root.join("anchorless").join(NAME);
            let reason = load_failure();
            let environment = reason
                .attempts()
                .iter()
                .find(|a| a.origin() == LoadOrigin::RuntimeDirectory)
                .unwrap();
            assert_eq!(environment.location(), anchorless);
            assert!(environment.loaded());
            assert!(
                matches!(environment.refusal(), LoadRefusal::MissingEntryPoint { symbol, required: false, .. } if symbol == "NDIlib_version")
            );
            let loader = reason.attempts().last().unwrap();
            assert_eq!(loader.origin(), LoadOrigin::LoaderSearch);
            assert_eq!(
                loader.refusal(),
                &LoadRefusal::AlreadyRefused {
                    path: anchorless,
                    refusal: Box::new(environment.refusal().clone())
                }
            );
            let ndi = Ndi::with_options(&standard().library_path(usable.clone())).unwrap();
            assert_eq!(ndi.info().library_path(), Some(usable.as_path()));
        }
        _ => panic!("unknown scenario {name}"),
    }
}
