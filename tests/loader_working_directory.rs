//! Constructor markers distinguish refusing an untrusted search location from
//! opening a library and rejecting it after its initializer has already run.
#![cfg(all(any(windows, target_os = "macos"), not(ndi_link_static)))]

#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(child, fixture);
use common::TestChild;

#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;

use grafton_ndi::{Error, LoadFailure, LoadOrigin, LoadRefusal, Runtime, RuntimeOptions};
use runtime_fixture::FakeRuntime;
use std::{
    fs,
    path::{Path, PathBuf},
};

fn canonical_fixture_path(path: &Path) -> PathBuf {
    let path = path.canonicalize().unwrap();
    #[cfg(windows)]
    {
        // Fixture names are ordinary, short paths. Strip the known prefix on
        // the expected side so the reported side must itself be ordinary.
        let text = path.to_str().unwrap();
        if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{unc}"));
        }
        PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(text))
    }
    #[cfg(not(windows))]
    path
}

fn assert_ordinary_path(path: &Path) {
    #[cfg(windows)]
    assert!(
        !matches!(path.components().next(), Some(std::path::Component::Prefix(prefix)) if prefix.kind().is_verbatim()),
        "{path:?}"
    );
    #[cfg(not(windows))]
    let _ = path;
}

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
    let working = canonical_fixture_path(working);
    for path in failure
        .attempts()
        .iter()
        .filter_map(|attempt| attempt.resolved_path())
    {
        assert_ordinary_path(path);
    }
    assert!(
        failure.attempts().iter().all(|attempt| {
            !attempt
                .resolved_path()
                .is_some_and(|path| path.starts_with(&working))
        }),
        "{failure}"
    );
    failure
}

fn assert_absent_explicit(error: Error, working: &Path, absent: &Path) {
    let failure = failed(error, working);
    let [attempt] = failure.attempts() else {
        panic!("expected only the explicit attempt, got {failure}");
    };
    assert_eq!(attempt.location(), absent);
    assert_eq!(attempt.origin(), LoadOrigin::Explicit);
    assert_eq!(attempt.refusal(), &LoadRefusal::Absent);
    assert!(!attempt.loaded());
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
                        attempt.origin()
                            == if scenario.starts_with("dyld-fallback-") {
                                grafton_ndi::LoadOrigin::DyldFallbackLibraryDirectory
                            } else {
                                grafton_ndi::LoadOrigin::DyldLibraryDirectory
                            }
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
            "absent-explicit-runtime" => {
                let absent = root.join("missing").join(STANDARD);
                let error =
                    Runtime::select(&RuntimeOptions::new().library_path(&absent)).unwrap_err();
                assert_absent_explicit(error, &working, &absent);
            }
            #[cfg(feature = "file-io")]
            "absent-explicit-file" => {
                use grafton_ndi::{
                    file::{FileApi, FileApiOptions},
                    Ndi,
                };
                let ndi = Ndi::with_options(
                    &RuntimeOptions::new().library_path(root.join("trusted").join(ADVANCED)),
                )
                .unwrap();
                let absent = root.join("missing").join(FILE);
                let error =
                    FileApi::new(&ndi, &FileApiOptions::new().library_path(&absent)).unwrap_err();
                assert_absent_explicit(error, &working, &absent);
            }
            "path" | "dyld-absolute" => {
                let runtime = Runtime::select(&RuntimeOptions::new()).unwrap();
                let reported = runtime.info().library_path().unwrap();
                assert_ordinary_path(reported);
                assert_eq!(
                    reported,
                    canonical_fixture_path(&root.join("loader").join(STANDARD)),
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

    let directory = common::FixtureDir::create("working-directory-loader");
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
        "absent-explicit-runtime",
        "absent-explicit-file",
        "dyld-relative-runtime",
        "dyld-empty-runtime",
        "dyld-mixed-runtime",
        "dyld-trailing-empty-runtime",
        "dyld-relative-file",
        "dyld-empty-file",
        "dyld-absolute",
        "dyld-fallback-relative-runtime",
        "dyld-fallback-empty-runtime",
        "dyld-fallback-relative-file",
    ] {
        if (scenario.ends_with("file") && !cfg!(feature = "file-io"))
            || (scenario == "path" && !cfg!(windows))
            || (scenario.starts_with("dyld-") && !cfg!(target_os = "macos"))
        {
            continue;
        }
        let marker = root.join("marker");
        let _ = fs::remove_file(&marker);
        let mut command = TestChild::at(
            if scenario.starts_with("dyld-") && scenario != "dyld-absolute" {
                &trusted_executable
            } else {
                &executable
            },
            "working_directory_libraries_never_run_constructors",
        );
        command
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
        if cfg!(target_os = "macos") && scenario.starts_with("absent-explicit-") {
            // This directory contains libraries with the absent paths' leaf
            // names. Their constructor would prove a native fallback ran.
            command.env("DYLD_FALLBACK_LIBRARY_PATH", &working);
        }
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
            command.env(
                if scenario.starts_with("dyld-fallback-") {
                    "DYLD_FALLBACK_LIBRARY_PATH"
                } else {
                    "DYLD_LIBRARY_PATH"
                },
                value,
            );
        }
        command.run();
    }
}
