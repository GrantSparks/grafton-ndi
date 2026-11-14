//! Prerequisite helpers distinguish absent code from loaded but refused modules.
#![cfg(all(target_os = "linux", not(ndi_link_static), feature = "file-io"))]

#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(runtime, file);
#[path = "support/fixture.rs"]
mod fixture;
#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;

use fixture::FixtureDir;
use grafton_ndi::{Ndi, RuntimeOptions};
use runtime_fixture::FakeRuntime;
use std::{fs, path::PathBuf, process::Command};

#[test]
fn prerequisite_helpers_fail_when_native_code_loaded_but_was_refused() {
    if let Ok(scenario) = std::env::var("GRAFTON_SELECTION_POLICY_CASE") {
        let root = PathBuf::from(std::env::var_os("GRAFTON_SELECTION_POLICY_ROOT").unwrap());
        if scenario.starts_with("file-") {
            let _ndi = Ndi::with_options(
                &RuntimeOptions::new().library_path(root.join("trusted/libndi_advanced.so.6")),
            )
            .unwrap();
            assert!(common::file_api().is_none());
        } else {
            assert!(common::ndi().is_none());
        }
        return;
    }
    let directory = FixtureDir::create("selection-policy");
    let root = directory.path();
    for name in ["trusted", "executable", "refused", "absent", "loader"] {
        fs::create_dir(root.join(name)).unwrap();
    }
    FakeRuntime {
        cfgs: vec!["fake_advanced"],
        soname: Some("libndi_advanced.so.6"),
        ..Default::default()
    }
    .build(&root.join("trusted/libndi_advanced.so.6"));
    FakeRuntime {
        cfgs: vec!["fake_omit_required"],
        ..Default::default()
    }
    .build(&root.join("refused/libndi_advanced.so.6"));
    for name in ["libndi.so.6", "libndi_advanced.so.6"] {
        fs::write(root.join("loader").join(name), b"not a shared library").unwrap();
    }
    let file = root.join("refused/libndifile.so.6");
    runtime_fixture::run(runtime_fixture::rustc("fake_dependency.rs", &file), &file);
    fs::write(root.join("loader/libndifile.so.6"), b"not a shared library").unwrap();
    let executable = root.join("executable/probe");
    fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    for scenario in [
        "runtime-refused",
        "runtime-absent",
        "file-refused",
        "file-absent",
    ] {
        let output = Command::new(&executable)
            .args([
                "--exact",
                "prerequisite_helpers_fail_when_native_code_loaded_but_was_refused",
                "--nocapture",
            ])
            .env("GRAFTON_SELECTION_POLICY_CASE", scenario)
            .env("GRAFTON_SELECTION_POLICY_ROOT", root)
            .env(
                "NDI_RUNTIME_DIR_V6",
                root.join(if scenario.ends_with("refused") {
                    "refused"
                } else {
                    "absent"
                }),
            )
            .env("LD_LIBRARY_PATH", root.join("loader"))
            .env_remove("LD_PRELOAD")
            .env_remove("GRAFTON_NDI_REQUIRE_RUNTIME")
            .env_remove("GRAFTON_NDI_EXPECTED_RUNTIME")
            .env_remove("GRAFTON_NDI_EXPECTED_VERSION")
            .env_remove("GRAFTON_NDI_EXPECTED_EDITION")
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            scenario.ends_with("absent"),
            "{scenario}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
