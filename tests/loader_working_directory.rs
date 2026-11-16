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
            #[cfg(target_os = "macos")]
            scenario if scenario.starts_with("dyld-") && scenario != "dyld-absolute" => {
                let error = if scenario.ends_with("-file") {
                    #[cfg(feature = "file-io")]
                    {
                        use grafton_ndi::{
                            file::{FileApi, FileApiOptions},
                            Ndi,
                        };
                        // The core's unique leaf has no DYLD override. Its
                        // companion File name does, and is also planted
                        // beside it to detect any premature companion open.
                        let ndi = Ndi::with_options(
                            &RuntimeOptions::new()
                                .library_path(root.join("trusted-file/core-for-file.dylib")),
                        )
                        .unwrap();
                        FileApi::new(&ndi, &FileApiOptions::new()).unwrap_err()
                    }
                    #[cfg(not(feature = "file-io"))]
                    panic!("File scenario requires file-io")
                } else {
                    Runtime::select(&RuntimeOptions::new()).unwrap_err()
                };
                let failure = failed(error, &working);
                assert!(!failure.loaded_any(), "{failure}");
                assert!(!failure.attempts().is_empty(), "{failure}");
                assert!(
                    failure.attempts().iter().all(|attempt| {
                        attempt.origin() == grafton_ndi::LoadOrigin::DyldLibraryDirectory
                            && attempt.refusal()
                                == &grafton_ndi::LoadRefusal::RelativeSearchDirectory
                    }),
                    "{failure}"
                );
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
            "path" | "dyld-absolute" => {
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
    for name in [
        "bin",
        "trusted-bin",
        "working",
        "working/relative",
        "trusted",
        "trusted-file",
        "loader",
    ] {
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
    for name in [STANDARD, ADVANCED, FILE] {
        fs::copy(working.join(STANDARD), working.join("relative").join(name)).unwrap();
    }
    FakeRuntime {
        cfgs: vec!["fake_advanced"],
        ..Default::default()
    }
    .build(&root.join("trusted").join(ADVANCED));
    fs::copy(
        root.join("trusted").join(ADVANCED),
        root.join("trusted-bin").join(ADVANCED),
    )
    .unwrap();
    #[cfg(target_os = "macos")]
    FakeRuntime {
        cfgs: vec!["fake_advanced"],
        ..Default::default()
    }
    .build(&root.join("trusted-file/core-for-file.dylib"));
    fs::copy(working.join(STANDARD), root.join("trusted-file").join(FILE)).unwrap();
    FakeRuntime::default().build(&root.join("loader").join(STANDARD));
    let executable = root
        .join("bin")
        .join(if cfg!(windows) { "probe.exe" } else { "probe" });
    fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    let trusted_executable = root.join("trusted-bin").join("probe");
    fs::copy(std::env::current_exe().unwrap(), &trusted_executable).unwrap();

    for scenario in [
        "runtime",
        "file",
        "path",
        "detector",
        "dyld-relative-runtime",
        "dyld-empty-runtime",
        "dyld-mixed-runtime",
        "dyld-trailing-empty-runtime",
        "dyld-relative-file",
        "dyld-empty-file",
        "dyld-absolute",
    ] {
        if (scenario.ends_with("file") && !cfg!(feature = "file-io"))
            || (scenario == "path" && !cfg!(windows))
            || (scenario.starts_with("dyld-") && !cfg!(target_os = "macos"))
        {
            continue;
        }
        let marker = root.join("marker");
        let _ = fs::remove_file(&marker);
        let mut command = Command::new(
            if scenario.starts_with("dyld-") && scenario != "dyld-absolute" {
                &trusted_executable
            } else {
                &executable
            },
        );
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
        if scenario.starts_with("dyld-") {
            let value = if scenario == "dyld-absolute" {
                root.join("loader").into_os_string()
            } else if scenario.contains("relative") {
                "relative".into()
            } else if scenario.contains("mixed") {
                std::env::join_paths([root.join("loader"), "relative".into()]).unwrap()
            } else if scenario.contains("trailing-empty") {
                std::env::join_paths([root.join("loader"), "".into()]).unwrap()
            } else {
                "".into()
            };
            command.env("DYLD_LIBRARY_PATH", value);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{scenario}:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
