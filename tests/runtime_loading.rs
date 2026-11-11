//! Loader tests run each selection scenario in its own process: successful module
//! selection is intentionally irreversible, while failed selection is retryable.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

#[path = "support/fixture.rs"]
mod fixture;

use fixture::FakeRuntime;
use grafton_ndi::{
    capabilities::{CapabilityStatus, CapabilityUnavailable, RuntimeFeature, SdkEdition},
    runtime::RuntimeOptions,
    Error, NDI,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// The runtime library names the loader searches for on this target.
const STANDARD_NAME: &str = "libndi.so.6";
const ADVANCED_NAME: &str = "libndi_advanced.so.6";

/// Version texts that identify which fixture a search selected.
const BESIDE_EXECUTABLE: &str = "6.9.9.1";
const IN_RUNTIME_DIRECTORY: &str = "6.8.8.1";
const ON_LOADER_PATH: &str = "6.7.7.1";

/// Every fixture a scenario may need, built once by the parent process.
struct Fixtures {
    root: PathBuf,
}

impl Fixtures {
    fn build(root: &Path) -> Self {
        for directory in ["bundle", "runtime", "loader", "empty", "mislabelled"] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        let fixtures = Self {
            root: root.to_owned(),
        };
        FakeRuntime::new().build(&fixtures.complete());
        FakeRuntime::new()
            .with("fake_omit_required")
            .build(&fixtures.missing());
        // A dependency may export an entry point the module itself lacks. A
        // lookup through the module's handle finds it; the loader must not.
        let dependency = fixture::build_dependency(&root.join("libndi_dependency.so"));
        FakeRuntime::new()
            .with("fake_omit_required")
            .depending_on(&dependency)
            .build(&fixtures.foreign());
        FakeRuntime::new()
            .with("fake_receiver_v4")
            .build(&fixtures.newer_standard());
        FakeRuntime::new()
            .with("fake_advanced")
            .build(&fixtures.advanced());
        FakeRuntime::new()
            .version("7.0.0.1")
            .build(&fixtures.next_major());
        FakeRuntime::new()
            .version(BESIDE_EXECUTABLE)
            .build(&fixtures.bundle().join(STANDARD_NAME));
        FakeRuntime::new()
            .version(IN_RUNTIME_DIRECTORY)
            .build(&fixtures.runtime().join(STANDARD_NAME));
        FakeRuntime::new()
            .version(ON_LOADER_PATH)
            .build(&fixtures.loader().join(STANDARD_NAME));
        // A Standard module under the Advanced runtime's file name.
        fs::copy(
            fixtures.complete(),
            fixtures.mislabelled().join(ADVANCED_NAME),
        )
        .unwrap();
        fixtures
    }

    fn complete(&self) -> PathBuf {
        self.root.join("libndi_complete.so")
    }
    fn missing(&self) -> PathBuf {
        self.root.join("libndi_missing.so")
    }
    fn foreign(&self) -> PathBuf {
        self.root.join("libndi_foreign.so")
    }
    fn newer_standard(&self) -> PathBuf {
        self.root.join("libndi_newer_standard.so")
    }
    fn advanced(&self) -> PathBuf {
        self.root.join("libndi_fake_advanced.so")
    }
    fn next_major(&self) -> PathBuf {
        self.root.join("libndi_next_major.so")
    }
    fn bundle(&self) -> PathBuf {
        self.root.join("bundle")
    }
    fn runtime(&self) -> PathBuf {
        self.root.join("runtime")
    }
    fn loader(&self) -> PathBuf {
        self.root.join("loader")
    }
    fn empty(&self) -> PathBuf {
        self.root.join("empty")
    }
    fn mislabelled(&self) -> PathBuf {
        self.root.join("mislabelled")
    }
    fn log(&self) -> PathBuf {
        PathBuf::from(std::env::var_os("GRAFTON_FAKE_NDI_LOG").unwrap())
    }
}

/// How the parent launches one scenario.
struct Scenario {
    name: &'static str,
    /// Run a copy of this test binary placed beside a bundled runtime.
    bundled_executable: bool,
    /// Directory for `NDI_RUNTIME_DIR_V6`, relative to the fixture root.
    runtime_directory: Option<&'static str>,
    /// Directory for `LD_LIBRARY_PATH`, relative to the fixture root.
    loader_directory: Option<&'static str>,
}

const fn scenario(name: &'static str) -> Scenario {
    Scenario {
        name,
        bundled_executable: false,
        runtime_directory: None,
        loader_directory: None,
    }
}

const SCENARIOS: &[Scenario] = &[
    scenario("retry_and_capabilities"),
    scenario("conflict"),
    scenario("advanced_request_needs_advanced_entry_points"),
    scenario("newer_standard_runtime_is_accepted"),
    scenario("advanced_module_satisfies_a_standard_request"),
    scenario("later_major_version_keeps_version_gated_features"),
    scenario("process_lifetime"),
    scenario("no_imports"),
    scenario("foreign_symbol"),
    scenario("concurrent_selection"),
    Scenario {
        runtime_directory: Some("runtime"),
        loader_directory: Some("loader"),
        ..scenario("explicit_path_is_exclusive")
    },
    Scenario {
        runtime_directory: Some("empty"),
        loader_directory: Some("loader"),
        ..scenario("empty_runtime_directory_falls_back_to_the_loader")
    },
    Scenario {
        runtime_directory: Some("runtime"),
        loader_directory: Some("loader"),
        ..scenario("runtime_directory_precedes_the_loader")
    },
    Scenario {
        bundled_executable: true,
        runtime_directory: Some("runtime"),
        loader_directory: Some("loader"),
        ..scenario("bundled_runtime_precedes_the_runtime_directory")
    },
    Scenario {
        runtime_directory: Some("mislabelled"),
        ..scenario("failure_names_every_candidate")
    },
];

#[test]
fn selected_runtime_loader_contract() {
    if let Ok(name) = std::env::var("GRAFTON_LOADER_CHILD") {
        let root = PathBuf::from(std::env::var_os("GRAFTON_LOADER_FIXTURES").unwrap());
        child(&name, &Fixtures { root });
        return;
    }
    let root = std::env::temp_dir().join(format!("grafton-ndi-loader-{}", std::process::id()));
    let fixtures = Fixtures::build(&root);
    let test_binary = std::env::current_exe().unwrap();
    let bundled_binary = fixtures.bundle().join("application");
    fs::copy(&test_binary, &bundled_binary).unwrap();

    for scenario in SCENARIOS {
        let executable = if scenario.bundled_executable {
            &bundled_binary
        } else {
            &test_binary
        };
        let mut command = Command::new(executable);
        command
            .args(["--exact", "selected_runtime_loader_contract", "--nocapture"])
            .env("GRAFTON_LOADER_CHILD", scenario.name)
            .env("GRAFTON_LOADER_FIXTURES", &root)
            .env(
                "GRAFTON_FAKE_NDI_LOG",
                root.join(format!("{}.log", scenario.name)),
            )
            .env_remove("NDI_RUNTIME_DIR_V6")
            .env_remove("LD_LIBRARY_PATH");
        if let Some(directory) = scenario.runtime_directory {
            command.env("NDI_RUNTIME_DIR_V6", root.join(directory));
        }
        if let Some(directory) = scenario.loader_directory {
            command.env("LD_LIBRARY_PATH", root.join(directory));
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}: {}\n{}",
            scenario.name,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(root).unwrap();
}

/// The Standard edition, named explicitly so these tests do not depend on the
/// default edition this build happened to select.
fn standard() -> RuntimeOptions {
    RuntimeOptions::default().edition(SdkEdition::Standard)
}

fn explicit(path: PathBuf) -> RuntimeOptions {
    standard().library_path(path)
}

/// The runtime the search order selects, with its version text.
fn search_selects() -> (NDI, String) {
    let ndi = NDI::with_options(&standard()).unwrap();
    let version = ndi.info().raw_version().to_owned();
    (ndi, version)
}

fn load_failure(result: Result<NDI, Error>) -> String {
    match result {
        Err(Error::RuntimeLoadFailed(reason)) => reason,
        Err(other) => panic!("expected a load failure, got {other}"),
        Ok(_) => panic!("expected a load failure, but a runtime loaded"),
    }
}

fn child(name: &str, fixtures: &Fixtures) {
    let complete = fixtures.complete();
    let options = explicit(complete.clone());
    match name {
        "retry_and_capabilities" => {
            let reason = load_failure(NDI::with_options(&explicit(fixtures.missing())));
            assert!(reason.contains("NDIlib_send_set_failover"), "{reason}");
            assert!(!NDI::is_running());
            let ndi = NDI::with_options(&options).unwrap();
            assert!(NDI::is_running());
            assert_eq!(ndi.info().library_path(), Some(complete.as_path()));
            assert_eq!(ndi.info().edition(), SdkEdition::Standard);
            assert_eq!(ndi.info().version().unwrap().minor(), 3);
            let capabilities = ndi.capabilities();
            assert_eq!(
                capabilities.status(RuntimeFeature::ReceiveDelay),
                CapabilityStatus::Unavailable(CapabilityUnavailable::SymbolUnavailable)
            );
            assert_eq!(
                capabilities.status(RuntimeFeature::Planar444Video),
                CapabilityStatus::Unavailable(CapabilityUnavailable::RuntimeTooOld)
            );
            assert!(NDI::try_is_supported_cpu().unwrap());
            assert!(NDI::version().unwrap().contains("6.3.2.0"));
            drop(ndi);
            assert!(!NDI::is_running());
            drop(NDI::with_options(&options).unwrap());
            let log = fs::read_to_string(fixtures.log()).unwrap();
            assert_eq!(log, "initialize\ndestroy\ninitialize\ndestroy\n");
        }
        "conflict" => {
            let ndi = NDI::with_options(&options).unwrap();
            assert!(matches!(
                NDI::with_options(&explicit(fixtures.missing())),
                Err(Error::RuntimeConflict(_))
            ));
            assert!(matches!(
                NDI::with_options(&RuntimeOptions::default().edition(SdkEdition::Advanced)),
                Err(Error::RuntimeConflict(_))
            ));
            // The selected module satisfies a request that names no other one.
            drop(NDI::with_options(&standard()).unwrap());
            drop(ndi);
        }
        "advanced_request_needs_advanced_entry_points" => {
            // A caller's request cannot turn a Standard module into an Advanced
            // runtime, whatever the file is called.
            for standard_module in [complete.clone(), fixtures.mislabelled().join(ADVANCED_NAME)] {
                let reason = load_failure(NDI::with_options(
                    &explicit(standard_module).edition(SdkEdition::Advanced),
                ));
                assert!(reason.contains("Advanced SDK entry points"), "{reason}");
            }
            // The failed request leaves selection open.
            let ndi = NDI::with_options(&options).unwrap();
            assert_eq!(ndi.info().edition(), SdkEdition::Standard);
        }
        "newer_standard_runtime_is_accepted" => {
            // A Standard runtime that gains an entry point today found only in
            // the Advanced SDK is still a usable Standard runtime.
            let ndi = NDI::with_options(&explicit(fixtures.newer_standard())).unwrap();
            assert_eq!(ndi.info().edition(), SdkEdition::Standard);
            assert!(NDI::is_running());
        }
        "advanced_module_satisfies_a_standard_request" => {
            let advanced = fixtures.advanced();
            let ndi = NDI::with_options(&explicit(advanced.clone())).unwrap();
            assert_eq!(ndi.info().edition(), SdkEdition::Advanced);
            // The edition is detected, so a later Advanced request agrees.
            drop(NDI::with_options(&explicit(advanced).edition(SdkEdition::Advanced)).unwrap());
        }
        "later_major_version_keeps_version_gated_features" => {
            let ndi = NDI::with_options(&explicit(fixtures.next_major())).unwrap();
            assert_eq!(ndi.info().version().unwrap().major(), 7);
            for feature in [
                RuntimeFeature::Planar444Video,
                RuntimeFeature::ReceiveBest444,
                RuntimeFeature::ReceiveVideoOnly,
            ] {
                assert!(ndi.capabilities().supports(feature), "{feature:?}");
            }
        }
        "process_lifetime" => {
            let ndi = NDI::with_options(&options).unwrap();
            let token = ndi.process_lifetime();
            assert_eq!(token.ndi().info(), ndi.info());
            drop(ndi);
            assert!(NDI::is_running());
            let log = fs::read_to_string(fixtures.log()).unwrap();
            assert_eq!(log, "initialize\n");
        }
        "foreign_symbol" => {
            let reason = load_failure(NDI::with_options(&explicit(fixtures.foreign())));
            assert!(
                reason.contains("NDIlib_send_set_failover resolves outside"),
                "{reason}"
            );
            drop(NDI::with_options(&options).unwrap());
        }
        "concurrent_selection" => {
            use std::sync::{Arc, Barrier};
            let ready = Arc::new(Barrier::new(17));
            let release = Arc::new(Barrier::new(17));
            let threads: Vec<_> = (0..16)
                .map(|_| {
                    let options = options.clone();
                    let ready = ready.clone();
                    let release = release.clone();
                    std::thread::spawn(move || {
                        let ndi = NDI::with_options(&options).unwrap();
                        ready.wait();
                        release.wait();
                        drop(ndi);
                    })
                })
                .collect();
            ready.wait();
            let log = fs::read_to_string(fixtures.log()).unwrap();
            assert_eq!(log, "initialize\n");
            release.wait();
            for thread in threads {
                thread.join().unwrap();
            }
            assert!(!NDI::is_running());
        }
        "no_imports" => {
            let output = Command::new("readelf")
                .args(["--wide", "--dyn-syms"])
                .arg(std::env::current_exe().unwrap())
                .output()
                .unwrap();
            assert!(output.status.success());
            let symbols = String::from_utf8(output.stdout).unwrap();
            assert!(
                !symbols
                    .lines()
                    .any(|line| line.contains(" UND ") && line.contains("NDIlib_")),
                "mandatory NDI symbol import: {symbols}"
            );
        }
        "explicit_path_is_exclusive" => {
            // Usable runtimes exist in the runtime directory and on the loader
            // path, but an explicit path names the only acceptable one.
            let absent = fixtures.empty().join(STANDARD_NAME);
            let reason = load_failure(NDI::with_options(&explicit(absent.clone())));
            assert!(reason.contains(absent.to_str().unwrap()), "{reason}");
            assert!(reason.contains("No such file or directory"), "{reason}");
            assert!(
                !reason.contains(fixtures.runtime().to_str().unwrap()),
                "{reason}"
            );
            assert!(matches!(
                NDI::with_options(&standard().library_path("libndi.so.6")),
                Err(Error::InvalidConfiguration(_))
            ));
        }
        "empty_runtime_directory_falls_back_to_the_loader" => {
            let (ndi, version) = search_selects();
            assert!(version.ends_with(ON_LOADER_PATH), "{version}");
            assert_eq!(
                ndi.info().library_path(),
                Some(fixtures.loader().join(STANDARD_NAME).as_path())
            );
        }
        "runtime_directory_precedes_the_loader" => {
            let (ndi, version) = search_selects();
            assert!(version.ends_with(IN_RUNTIME_DIRECTORY), "{version}");
            assert_eq!(
                ndi.info().library_path(),
                Some(fixtures.runtime().join(STANDARD_NAME).as_path())
            );
        }
        "bundled_runtime_precedes_the_runtime_directory" => {
            let (ndi, version) = search_selects();
            assert!(version.ends_with(BESIDE_EXECUTABLE), "{version}");
            assert_eq!(
                ndi.info().library_path(),
                Some(fixtures.bundle().join(STANDARD_NAME).as_path())
            );
        }
        "failure_names_every_candidate" => {
            let reason = load_failure(NDI::with_options(
                &RuntimeOptions::default().edition(SdkEdition::Advanced),
            ));
            let beside_executable = std::env::current_exe()
                .unwrap()
                .with_file_name(ADVANCED_NAME);
            let in_runtime_directory = fixtures.mislabelled().join(ADVANCED_NAME);
            for expected in [
                format!(
                    "{} (next to the executable): not found",
                    beside_executable.display()
                ),
                format!(
                    "{} (NDI_RUNTIME_DIR_V6): it does not export the Advanced SDK entry points",
                    in_runtime_directory.display()
                ),
                format!("{ADVANCED_NAME} (system library search): "),
                "cannot open shared object file".to_owned(),
            ] {
                assert!(
                    reason.contains(&expected),
                    "missing `{expected}` in: {reason}"
                );
            }
            // Nothing was selected, so a usable runtime can still be chosen.
            drop(NDI::with_options(&options).unwrap());
        }
        _ => panic!("unknown loader scenario {name}"),
    }
}
