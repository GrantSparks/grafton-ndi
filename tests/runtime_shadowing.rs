//! What the search reports when the platform loader answers a request for a
//! library name with a module that is already loaded.
//!
//! A library that is opened and then rejected stays loaded. The loader matches
//! a later request for a bare name against the names loaded modules carry, so
//! it returns the rejected module again instead of searching its path.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

#[path = "support/fixture.rs"]
mod fixture;

use fixture::FakeRuntime;
use grafton_ndi::{capabilities::SdkEdition, runtime::RuntimeOptions, Error, NDI};
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
];

#[test]
fn loader_search_reports_what_the_platform_resolved() {
    if let Ok(name) = std::env::var("GRAFTON_SHADOW_CHILD") {
        let root = PathBuf::from(std::env::var_os("GRAFTON_SHADOW_FIXTURES").unwrap());
        child(&name, &root);
        return;
    }
    let root = std::env::temp_dir().join(format!("grafton-ndi-shadow-{}", std::process::id()));
    for directory in ["rejected", "usable"] {
        fs::create_dir_all(root.join(directory)).unwrap();
    }
    // Two different libraries that carry the same name.
    FakeRuntime::new()
        .with("fake_omit_required")
        .soname(NAME)
        .build(&root.join("rejected").join(NAME));
    FakeRuntime::new()
        .soname(NAME)
        .build(&root.join("usable").join(NAME));

    for (scenario, runtime_directory, loader_directory) in SCENARIOS {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "loader_search_reports_what_the_platform_resolved",
                "--nocapture",
            ])
            .env("GRAFTON_SHADOW_CHILD", scenario)
            .env("GRAFTON_SHADOW_FIXTURES", &root)
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
    fs::remove_dir_all(root).unwrap();
}

fn standard() -> RuntimeOptions {
    RuntimeOptions::default().edition(SdkEdition::Standard)
}

fn load_failure() -> String {
    match NDI::with_options(&standard()) {
        Err(Error::RuntimeLoadFailed(reason)) => reason,
        Err(other) => panic!("expected a load failure, got {other}"),
        Ok(ndi) => panic!("expected a load failure, but loaded {:?}", ndi.info()),
    }
}

fn child(name: &str, root: &Path) {
    let rejected = root.join("rejected").join(NAME);
    let usable = root.join("usable").join(NAME);
    let missing_entry_point =
        "it does not export the required entry point NDIlib_send_set_failover";
    match name {
        "rejected_module_shadows_the_loader_search" => {
            let reason = load_failure();
            // The file in the runtime directory is rejected for its own defect.
            assert!(
                reason.contains(&format!(
                    "{} (NDI_RUNTIME_DIR_V6): {missing_entry_point}",
                    rejected.display()
                )),
                "{reason}"
            );
            // The search by name gets that same module back, and says so
            // rather than reporting the defect as another library's.
            assert!(
                reason.contains(&format!(
                    "{NAME} (system library search): the platform loader returned {}, which is \
                     already loaded and was rejected",
                    rejected.display()
                )),
                "{reason}"
            );
            assert!(reason.contains("pass an explicit library path"), "{reason}");
            // The usable library was never reached by name; naming its file
            // reaches it.
            let ndi = NDI::with_options(&standard().library_path(usable.clone())).unwrap();
            assert_eq!(ndi.info().library_path(), Some(usable.as_path()));
        }
        "loader_search_reports_the_file_it_resolved" => {
            let reason = load_failure();
            assert!(
                reason.contains(&format!(
                    "{NAME} (system library search) resolved to {}: {missing_entry_point}",
                    rejected.display()
                )),
                "{reason}"
            );
            // A second search meets the same loaded module again.
            let again = load_failure();
            assert!(
                again.contains(&format!(
                    "the platform loader returned {}, which is already loaded and was rejected",
                    rejected.display()
                )),
                "{again}"
            );
        }
        _ => panic!("unknown scenario {name}"),
    }
}
