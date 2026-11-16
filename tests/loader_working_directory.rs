//! Constructor markers distinguish refusing an untrusted search location from
//! opening a library and rejecting it after its initializer has already run.
#![cfg(all(any(windows, target_os = "macos"), not(ndi_link_static)))]

#[path = "support/fixture.rs"]
mod fixture;
#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;

use fixture::FixtureDir;
use grafton_ndi::{Error, LoadFailure, Runtime, RuntimeOptions};
use runtime_fixture::FakeRuntime;
use std::{fs, path::Path, process::Command};

const STANDARD: &str = if cfg!(all(windows, target_pointer_width = "64")) {
    "Processing.NDI.Lib.x64.dll"
} else if cfg!(windows) {
    "Processing.NDI.Lib.x86.dll"
} else {
    "libndi.dylib"
};
const ADVANCED: &str = if cfg!(all(windows, target_pointer_width = "64")) {
    "Processing.NDI.Lib.Advanced.x64.dll"
} else if cfg!(windows) {
    "Processing.NDI.Lib.Advanced.x86.dll"
} else {
    "libndi_advanced.dylib"
};
const FILE: &str = if cfg!(all(windows, target_pointer_width = "64")) {
    "Processing.NDI.Lib.File.x64.dll"
} else if cfg!(windows) {
    "Processing.NDI.Lib.File.x86.dll"
} else {
    "libndifile.dylib"
};

fn failed(error: Error, working: &Path) -> LoadFailure {
    let Error::RuntimeLoadFailed(failure) = error else {
        panic!("expected a search failure, got {error:?}");
    };
    assert!(
        failure.attempts().iter().all(|attempt| {
            !attempt
                .resolved_path()
                .is_some_and(|path| path.starts_with(working))
        }),
        "{failure}"
    );
    failure
}

#[test]
fn working_directory_libraries_never_run_constructors() {
    if let Ok(scenario) = std::env::var("GRAFTON_WORKING_DIRECTORY_SCENARIO") {
        let root = std::env::var_os("GRAFTON_WORKING_DIRECTORY_ROOT").unwrap();
        let root = Path::new(&root);
        let marker = root.join("marker");
        let working = root.join("working");
        match scenario.as_str() {
            "runtime" => {
                let error = Runtime::select(&RuntimeOptions::new()).unwrap_err();
                failed(error, &working);
            }
            #[cfg(feature = "file-io")]
            "file" => {
                use grafton_ndi::{
                    file::{FileApi, FileApiOptions},
                    Ndi,
                };
                let ndi = Ndi::with_options(
                    &RuntimeOptions::new().library_path(root.join("trusted").join(ADVANCED)),
                )
                .unwrap();
                let error = FileApi::new(&ndi, &FileApiOptions::new()).unwrap_err();
                failed(error, &working);
            }
            "path" => {
                let runtime = Runtime::select(&RuntimeOptions::new()).unwrap();
                assert_eq!(
                    runtime
                        .info()
                        .library_path()
                        .unwrap()
                        .canonicalize()
                        .unwrap(),
                    root.join("loader").join(STANDARD).canonicalize().unwrap(),
                );
            }
            "detector" => {
                // Prove the marker can detect a load on this platform. A
                // missing-symbol refusal happens only after its constructor.
                assert!(Runtime::select(
                    &RuntimeOptions::new().library_path(working.join(STANDARD)),
                )
                .is_err());
                assert!(marker.is_file(), "constructor detector did not run");
                return;
            }
            _ => panic!("unknown scenario {scenario}"),
        }
        assert!(!marker.exists(), "working-directory constructor ran");
        return;
    }

    let directory = FixtureDir::create("working-directory-loader");
    let root = directory.path();
    for name in ["bin", "working", "trusted", "loader"] {
        fs::create_dir(root.join(name)).unwrap();
    }
    let working = root.join("working");
    runtime_fixture::run(
        runtime_fixture::rustc("rejected_constructor.rs", &working.join(STANDARD)),
        &working.join(STANDARD),
    );
    for name in [ADVANCED, FILE] {
        fs::copy(working.join(STANDARD), working.join(name)).unwrap();
    }
    FakeRuntime {
        cfgs: vec!["fake_advanced"],
        ..Default::default()
    }
    .build(&root.join("trusted").join(ADVANCED));
    FakeRuntime::default().build(&root.join("loader").join(STANDARD));
    let executable = root
        .join("bin")
        .join(if cfg!(windows) { "probe.exe" } else { "probe" });
    fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();

    for scenario in ["runtime", "file", "path", "detector"] {
        if (scenario == "file" && !cfg!(feature = "file-io"))
            || (scenario == "path" && !cfg!(windows))
        {
            continue;
        }
        let marker = root.join("marker");
        let _ = fs::remove_file(&marker);
        let mut command = Command::new(&executable);
        command
            .args([
                "--exact",
                "working_directory_libraries_never_run_constructors",
                "--nocapture",
            ])
            .current_dir(&working)
            .env("GRAFTON_WORKING_DIRECTORY_SCENARIO", scenario)
            .env("GRAFTON_WORKING_DIRECTORY_ROOT", root)
            .env("GRAFTON_REJECTED_CONSTRUCTOR_MARKER", marker)
            .env_remove("NDI_RUNTIME_DIR_V6")
            .env_remove("DYLD_LIBRARY_PATH")
            .env_remove("DYLD_FALLBACK_LIBRARY_PATH")
            .env(
                "PATH",
                if scenario == "path" {
                    root.join("loader")
                } else {
                    root.join("empty")
                },
            );
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{scenario}:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
