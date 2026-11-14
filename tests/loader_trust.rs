//! Library constructors run before validation; explicit selection avoids
//! untrusted search candidates. All search directories here are test-owned.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]
#[path = "support/fixture.rs"]
mod fixture;
#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;
use fixture::FixtureDir;
use grafton_ndi::{Ndi, RuntimeOptions};
use runtime_fixture::FakeRuntime;
use std::{fs, path::PathBuf, process::Command};

#[test]
fn relative_environment_directories_never_run_constructors() {
    let trusted_name = if cfg!(any(feature = "advanced", feature = "file-io")) {
        "libndi_advanced.so.6"
    } else {
        "libndi.so.6"
    };
    if let Ok(scenario) = std::env::var("GRAFTON_RELATIVE_TRUST_SCENARIO") {
        let root = PathBuf::from(std::env::var_os("GRAFTON_TRUST_ROOT").unwrap());
        let trusted = root.join("trusted").join(trusted_name);
        let ndi = if scenario == "runtime" {
            Ndi::new().unwrap()
        } else {
            Ndi::with_options(&RuntimeOptions::new().library_path(&trusted)).unwrap()
        };
        assert_eq!(ndi.info().library_path(), Some(trusted.as_path()));
        #[cfg(feature = "file-io")]
        if scenario == "file" {
            use grafton_ndi::file::{FileApi, FileRuntimeOptions};
            let file = FileApi::open(&ndi, &FileRuntimeOptions::new()).unwrap();
            assert_eq!(
                file.library_path(),
                Some(root.join("loader/libndifile.so.6").as_path())
            );
        }
        assert!(!root.join("marker").exists());
        return;
    }
    for scenario in ["runtime", "file"] {
        if scenario == "file" && !cfg!(feature = "file-io") {
            continue;
        }
        let directory = FixtureDir::create("relative-loader-trust");
        let root = directory.path();
        for name in ["trusted", "executable", "relative", "loader"] {
            fs::create_dir(root.join(name)).unwrap();
        }
        let trusted = root.join("trusted").join(trusted_name);
        FakeRuntime {
            cfgs: if cfg!(any(feature = "advanced", feature = "file-io")) {
                vec!["fake_advanced"]
            } else {
                Vec::new()
            },
            version: Some("6.4.0.2"),
            soname: Some(trusted_name),
            ..Default::default()
        }
        .build(&trusted);
        for name in ["libndi.so.6", "libndi_advanced.so.6", "libndifile.so.6"] {
            let rejected = root.join("relative").join(name);
            runtime_fixture::run(
                runtime_fixture::rustc("rejected_constructor.rs", &rejected),
                &rejected,
            );
        }
        #[cfg(feature = "file-io")]
        if scenario == "file" {
            let output = root.join("loader/libndifile.so.6");
            let mut rustc = runtime_fixture::rustc("fake_file_library.rs", &output);
            rustc.arg(runtime_fixture::link_argument(trusted.as_os_str()));
            runtime_fixture::run(rustc, &output);
        }
        let executable = root.join("executable/probe");
        fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
        let output = Command::new(&executable)
            .args([
                "--exact",
                "relative_environment_directories_never_run_constructors",
                "--nocapture",
            ])
            .current_dir(root)
            .env("GRAFTON_RELATIVE_TRUST_SCENARIO", scenario)
            .env("GRAFTON_TRUST_ROOT", root)
            .env("GRAFTON_REJECTED_CONSTRUCTOR_MARKER", root.join("marker"))
            .env("NDI_RUNTIME_DIR_V6", "relative")
            .env(
                "LD_LIBRARY_PATH",
                root.join(if scenario == "runtime" {
                    "trusted"
                } else {
                    "loader"
                }),
            )
            .env_remove("LD_PRELOAD")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{scenario}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn explicit_paths_avoid_rejected_search_candidates() {
    if let Ok(scenario) = std::env::var("GRAFTON_TRUST_SCENARIO") {
        let root = PathBuf::from(std::env::var_os("GRAFTON_TRUST_ROOT").unwrap());
        let marker = root.join("marker");
        let trusted = root.join("trusted/libndi_advanced.so.6");
        if scenario == "explicit-rejected" {
            assert!(Ndi::with_options(
                &RuntimeOptions::new().library_path(root.join("executable/libndi.so.6"))
            )
            .is_err());
            assert!(marker.exists());
            return;
        }
        let ndi = if scenario == "default" {
            Ndi::new().unwrap()
        } else {
            Ndi::with_options(&RuntimeOptions::new().library_path(&trusted)).unwrap()
        };
        assert_eq!(ndi.info().library_path(), Some(trusted.as_path()));
        assert_eq!(marker.exists(), scenario == "default");
        #[cfg(feature = "file-io")]
        if scenario.starts_with("file-") {
            use grafton_ndi::file::{FileApi, FileRuntimeOptions};
            let selected = if scenario == "file-companion" {
                root.join("trusted/libndifile.so.6")
            } else {
                root.join("file-runtime/libndifile.so.6")
            };
            let options = if scenario == "file-explicit" {
                FileRuntimeOptions::new().library_path(&selected)
            } else {
                FileRuntimeOptions::new()
            };
            let file = FileApi::open(&ndi, &options).unwrap();
            assert_eq!(file.library_path(), Some(selected.as_path()));
            assert_eq!(marker.exists(), scenario == "file-fallback");
        }
        return;
    }
    for scenario in [
        "default",
        "explicit",
        "explicit-rejected",
        "file-companion",
        "file-fallback",
        "file-explicit",
    ] {
        if scenario.starts_with("file-") && !cfg!(feature = "file-io") {
            continue;
        }
        let directory = FixtureDir::create("loader-trust");
        let root = directory.path();
        for name in ["trusted", "executable", "file-runtime"] {
            fs::create_dir(root.join(name)).unwrap();
        }
        let trusted = root.join("trusted/libndi_advanced.so.6");
        FakeRuntime {
            cfgs: vec!["fake_advanced"],
            version: Some("6.4.0.2"),
            soname: Some("libndi_advanced.so.6"),
            ..Default::default()
        }
        .build(&trusted);
        let executable = root.join("executable/probe");
        fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
        let rejected = root.join("executable/libndi.so.6");
        runtime_fixture::run(
            runtime_fixture::rustc("rejected_constructor.rs", &rejected),
            &rejected,
        );
        #[cfg(feature = "file-io")]
        if scenario.starts_with("file-") {
            let rejected = root.join("executable/libndifile.so.6");
            runtime_fixture::run(
                runtime_fixture::rustc("rejected_constructor.rs", &rejected),
                &rejected,
            );
            let output = root
                .join(if scenario == "file-companion" {
                    "trusted"
                } else {
                    "file-runtime"
                })
                .join("libndifile.so.6");
            let mut rustc = runtime_fixture::rustc("fake_file_library.rs", &output);
            rustc.arg(runtime_fixture::link_argument(trusted.as_os_str()));
            runtime_fixture::run(rustc, &output);
        }
        let output = Command::new(&executable)
            .args([
                "--exact",
                "explicit_paths_avoid_rejected_search_candidates",
                "--nocapture",
            ])
            .env("GRAFTON_TRUST_SCENARIO", scenario)
            .env("GRAFTON_TRUST_ROOT", root)
            .env("GRAFTON_REJECTED_CONSTRUCTOR_MARKER", root.join("marker"))
            .env(
                "NDI_RUNTIME_DIR_V6",
                root.join(if scenario.starts_with("file-") {
                    "file-runtime"
                } else {
                    "trusted"
                }),
            )
            .env_remove("LD_LIBRARY_PATH")
            .env_remove("LD_PRELOAD")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{scenario}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
