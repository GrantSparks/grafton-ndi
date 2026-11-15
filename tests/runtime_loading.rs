//! Loader tests run each selection scenario in its own process: successful module
//! selection is intentionally irreversible, while failed selection is retryable.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;

use grafton_ndi::{
    capabilities::{CapabilityStatus, CapabilityUnavailable, RuntimeFeature, SdkEdition},
    runtime::{Runtime, RuntimeOptions},
    Error, LibraryKind, LoadFailure, LoadOrigin, LoadRefusal, Ndi, SelectionConflict,
};
use runtime_fixture::FakeRuntime;
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
        for directory in [
            "bundle",
            "runtime",
            "loader",
            "empty",
            "mislabelled",
            "unloadable",
            "preloaded",
            "global-named",
        ] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        let fixtures = Self {
            root: root.to_owned(),
        };
        FakeRuntime {
            cfgs: vec!["fake_constructor"],
            ..Default::default()
        }
        .build(&fixtures.complete());
        FakeRuntime {
            cfgs: vec!["fake_advanced"],
            ..Default::default()
        }
        .build(&fixtures.preloaded());
        FakeRuntime {
            soname: Some(STANDARD_NAME),
            ..Default::default()
        }
        .build(&fixtures.named_global());
        std::os::unix::fs::symlink(fixtures.preloaded(), fixtures.alias("symlink")).unwrap();
        fs::hard_link(fixtures.preloaded(), fixtures.alias("hardlink")).unwrap();
        #[cfg(target_arch = "x86_64")]
        FakeRuntime {
            cfgs: vec!["fake_plt_runtime"],
            ..Default::default()
        }
        .build(&fixtures.plt());
        FakeRuntime {
            cfgs: vec!["fake_omit_required"],
            ..Default::default()
        }
        .build(&fixtures.missing());
        // A dependency may export an entry point the module itself lacks. A
        // lookup through the module's handle finds it; the loader must not.
        let dependency = runtime_fixture::run(
            runtime_fixture::rustc("fake_dependency.rs", &root.join("libndi_dependency.so")),
            &root.join("libndi_dependency.so"),
        );
        FakeRuntime {
            cfgs: vec!["fake_omit_required"],
            dependency: Some(&dependency),
            ..Default::default()
        }
        .build(&fixtures.foreign());
        FakeRuntime {
            cfgs: vec!["fake_receiver_v4"],
            ..Default::default()
        }
        .build(&fixtures.newer_standard());
        FakeRuntime {
            cfgs: vec!["fake_advanced"],
            ..Default::default()
        }
        .build(&fixtures.advanced());
        FakeRuntime {
            cfgs: vec!["fake_advanced", "fake_configured_creation"],
            version: Some("6.4.0.0"),
            ..Default::default()
        }
        .build(&fixtures.configured_only());
        FakeRuntime {
            version: Some("7.0.0.1"),
            ..Default::default()
        }
        .build(&fixtures.next_major());
        FakeRuntime {
            cfgs: vec!["fake_long_version"],
            ..Default::default()
        }
        .build(&fixtures.long_version());
        FakeRuntime {
            cfgs: vec!["fake_constructor"],
            version: Some(BESIDE_EXECUTABLE),
            ..Default::default()
        }
        .build(&fixtures.bundle().join(STANDARD_NAME));
        FakeRuntime {
            cfgs: vec!["fake_constructor"],
            version: Some(IN_RUNTIME_DIRECTORY),
            ..Default::default()
        }
        .build(&fixtures.runtime().join(STANDARD_NAME));
        FakeRuntime {
            cfgs: vec!["fake_constructor"],
            version: Some(ON_LOADER_PATH),
            ..Default::default()
        }
        .build(&fixtures.loader().join(STANDARD_NAME));
        // A Standard module under the Advanced runtime's file name.
        fs::copy(
            fixtures.complete(),
            fixtures.mislabelled().join(ADVANCED_NAME),
        )
        .unwrap();
        // A file under that name which is not a library at all. The platform
        // loader stops at the first file of the wanted name on its path, so
        // this decides its search whatever the host has installed.
        fs::write(
            fixtures.unloadable().join(ADVANCED_NAME),
            "not a shared library\n",
        )
        .unwrap();
        fixtures
    }

    fn complete(&self) -> PathBuf {
        self.root.join("libndi_complete.so")
    }
    fn preloaded(&self) -> PathBuf {
        self.root.join("preloaded/libndi_preloaded.so")
    }
    fn named_global(&self) -> PathBuf {
        self.root.join("global-named").join(STANDARD_NAME)
    }
    fn alias(&self, kind: &str) -> PathBuf {
        self.root.join(format!("libndi_preloaded_{kind}.so"))
    }
    #[cfg(target_arch = "x86_64")]
    fn plt(&self) -> PathBuf {
        self.root.join("libndi_plt.so")
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
    /// An Advanced 6.4 module with the configured creation functions of the
    /// router, advertisers and listeners, but none of the core functions
    /// those objects go on to use.
    fn configured_only(&self) -> PathBuf {
        self.root.join("libndi_configured_only.so")
    }
    fn next_major(&self) -> PathBuf {
        self.root.join("libndi_next_major.so")
    }
    /// A module whose version text is longer than one SDK string may be.
    fn long_version(&self) -> PathBuf {
        self.root.join("libndi_long_version.so")
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
    fn unloadable(&self) -> PathBuf {
        self.root.join("unloadable")
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
    /// A fixture placed in the global symbol scope before the child starts.
    preloaded: Option<&'static str>,
}

const fn scenario(name: &'static str) -> Scenario {
    Scenario {
        name,
        bundled_executable: false,
        runtime_directory: None,
        loader_directory: None,
        preloaded: None,
    }
}

const SCENARIOS: &[Scenario] = &[
    scenario("relative_path_selects_nothing"),
    scenario("retry_and_capabilities"),
    scenario("conflict"),
    scenario("advanced_request_needs_advanced_entry_points"),
    scenario("newer_standard_runtime_is_accepted"),
    scenario("advanced_module_satisfies_a_standard_request"),
    scenario("later_major_version_keeps_version_gated_features"),
    scenario("overlong_version_text_is_refused"),
    scenario("configured_objects_are_refused_for_their_own_capability"),
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
        loader_directory: Some("unloadable"),
        ..scenario("failure_names_every_candidate")
    },
    Scenario {
        runtime_directory: Some("runtime"),
        ..scenario("selecting_and_starting_are_separate_steps")
    },
    scenario("a_failed_start_is_classified_and_can_be_retried"),
    Scenario {
        bundled_executable: true,
        preloaded: Some("preloaded/libndi_preloaded.so"),
        ..scenario("foreign_preload_refuses_default_search_before_constructors")
    },
    Scenario {
        runtime_directory: Some("runtime"),
        loader_directory: Some("loader"),
        preloaded: Some("preloaded/libndi_preloaded.so"),
        ..scenario("foreign_preload_refuses_runtime_directory_before_constructors")
    },
    Scenario {
        preloaded: Some("preloaded/libndi_preloaded.so"),
        ..scenario("foreign_preload_refuses_explicit_path_before_constructors")
    },
    Scenario {
        preloaded: Some("preloaded/libndi_preloaded.so"),
        ..scenario("the_preloaded_module_and_its_aliases_are_accepted")
    },
    Scenario {
        preloaded: Some("preloaded/libndi_preloaded.so"),
        ..scenario("the_preloaded_module_may_be_selected_through_a_hardlink")
    },
    Scenario {
        // Despite a different file on the loader path, the bare name resolves
        // to the already loaded module carrying that SONAME.
        loader_directory: Some("loader"),
        preloaded: Some("global-named/libndi.so.6"),
        ..scenario("a_bare_name_may_select_the_preloaded_module")
    },
    #[cfg(target_arch = "x86_64")]
    Scenario {
        preloaded: Some("libndi_plt.so"),
        ..scenario("a_global_runtime_captures_another_modules_plt_self_call")
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
            .env_remove("LD_LIBRARY_PATH")
            .env_remove("LD_PRELOAD")
            .env(
                "GRAFTON_REJECTED_CONSTRUCTOR_MARKER",
                root.join(format!("{}.constructor", scenario.name)),
            );
        if let Some(preloaded) = scenario.preloaded {
            command.env("LD_PRELOAD", root.join(preloaded));
        }
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
fn search_selects() -> (Ndi, String) {
    let ndi = Ndi::with_options(&standard()).unwrap();
    let version = ndi.info().raw_version().to_owned();
    (ndi, version)
}

/// Whether the file at `library` is mapped into this process.
fn is_loaded(library: &Path) -> bool {
    fs::read_to_string("/proc/self/maps")
        .unwrap()
        .contains(library.to_str().unwrap())
}

fn load_failure(result: Result<impl std::fmt::Debug, Error>) -> LoadFailure {
    match result {
        Err(Error::RuntimeLoadFailed(reason)) => {
            assert_eq!(reason.library(), LibraryKind::Runtime);
            assert!(!reason.attempts().is_empty());
            reason
        }
        Err(other) => panic!("expected a load failure, got {other}"),
        Ok(_) => panic!("expected a load failure, but a runtime loaded"),
    }
}

fn child(name: &str, fixtures: &Fixtures) {
    let complete = fixtures.complete();
    let options = explicit(complete.clone());
    match name {
        "foreign_preload_refuses_default_search_before_constructors"
        | "foreign_preload_refuses_runtime_directory_before_constructors" => {
            for reason in [
                load_failure(Ndi::new()),
                load_failure(Runtime::select(&RuntimeOptions::default())),
            ] {
                assert!(!reason.loaded_any());
                let origin = if name.contains("default_search") {
                    LoadOrigin::Executable
                } else {
                    LoadOrigin::RuntimeDirectory
                };
                let attempt = reason
                    .attempts()
                    .iter()
                    .find(|attempt| {
                        attempt.origin() == origin
                            && attempt.location().file_name().unwrap() == STANDARD_NAME
                    })
                    .unwrap();
                assert_eq!(
                    attempt.refusal(),
                    &LoadRefusal::ForeignRuntimeInScope {
                        module: fixtures.preloaded()
                    }
                );
                assert!(!attempt.loaded());
                assert!(!is_loaded(attempt.location()));
            }
            assert!(!PathBuf::from(
                std::env::var_os("GRAFTON_REJECTED_CONSTRUCTOR_MARKER").unwrap()
            )
            .exists());
            assert!(!Ndi::is_running());
        }
        "foreign_preload_refuses_explicit_path_before_constructors" => {
            for error in [
                Ndi::with_options(&options).unwrap_err(),
                Runtime::select(&options).unwrap_err(),
            ] {
                match error {
                    Error::RuntimeConflict { kind, detail } => {
                        assert_eq!(kind, SelectionConflict::ForeignRuntimeInScope);
                        assert!(detail.contains(complete.to_str().unwrap()));
                        assert!(detail.contains(fixtures.preloaded().to_str().unwrap()));
                    }
                    other => panic!("expected a global-scope conflict, got {other}"),
                }
            }
            assert!(!is_loaded(&complete));
            assert!(!PathBuf::from(
                std::env::var_os("GRAFTON_REJECTED_CONSTRUCTOR_MARKER").unwrap()
            )
            .exists());
            // Refusal leaves selection open for the global runtime itself.
            drop(Ndi::with_options(&explicit(fixtures.preloaded())).unwrap());
        }
        "the_preloaded_module_and_its_aliases_are_accepted"
        | "the_preloaded_module_may_be_selected_through_a_hardlink" => {
            let alias = if name.contains("hardlink") {
                "hardlink"
            } else {
                "symlink"
            };
            for path in [
                fixtures.alias(alias),
                fixtures.alias("hardlink"),
                fixtures.preloaded(),
            ] {
                let runtime = Runtime::select(&explicit(path)).unwrap();
                assert_eq!(
                    runtime.info().library_path(),
                    Some(fixtures.preloaded().as_path())
                );
                drop(runtime.initialize().unwrap());
            }
        }
        "a_bare_name_may_select_the_preloaded_module" => {
            let runtime = Runtime::select(&standard()).unwrap();
            assert_eq!(
                runtime.info().library_path(),
                Some(fixtures.named_global().as_path())
            );
            assert!(!is_loaded(&fixtures.loader().join(STANDARD_NAME)));
            drop(runtime.initialize().unwrap());
        }
        #[cfg(target_arch = "x86_64")]
        "a_global_runtime_captures_another_modules_plt_self_call" => {
            // Bypass selection only in this binding probe. Opening a second
            // copy demonstrates why the production loader must refuse it.
            type Create = unsafe extern "C" fn(*const std::ffi::c_void) -> *mut std::ffi::c_void;
            type CreateV4 = unsafe extern "C" fn(
                *const std::ffi::c_void,
                *const std::ffi::c_void,
            ) -> *mut std::ffi::c_void;
            // A copy has its own instance marker and relocation slots.
            let other = fixtures.root.join("libndi_plt_other.so");
            fs::copy(fixtures.plt(), &other).unwrap();
            // SAFETY: these test-owned modules implement the signatures above;
            // their returned markers are compared only, never dereferenced.
            unsafe {
                let global = libloading::Library::new(fixtures.plt()).unwrap();
                let candidate = libloading::Library::new(&other).unwrap();
                let global_v4 = global.get::<CreateV4>(b"NDIlib_recv_create_v4\0").unwrap();
                let own_v4 = candidate
                    .get::<CreateV4>(b"NDIlib_recv_create_v4\0")
                    .unwrap();
                let trampoline = candidate.get::<Create>(b"NDIlib_recv_create_v3\0").unwrap();
                let global_instance = global_v4(std::ptr::null(), std::ptr::null());
                let own_instance = own_v4(std::ptr::null(), std::ptr::null());
                assert_ne!(global_instance, own_instance);
                assert_eq!(trampoline(std::ptr::null()), global_instance);
            }
        }
        "relative_path_selects_nothing" => {
            let relative = RuntimeOptions::new().library_path("libndi.so.6");
            assert!(matches!(
                Runtime::select(&relative),
                Err(Error::InvalidInput(_))
            ));
            assert!(matches!(
                Ndi::with_options(&relative),
                Err(Error::InvalidInput(_))
            ));
            assert!(!Ndi::is_running());
            assert!(!fixtures.log().exists());
            drop(Ndi::with_options(&options).unwrap());
        }
        "retry_and_capabilities" => {
            let reason = load_failure(Ndi::with_options(&explicit(fixtures.missing())));
            assert!(reason.loaded_any());
            let attempt = &reason.attempts()[0];
            assert_eq!(attempt.origin(), LoadOrigin::Explicit);
            assert_eq!(attempt.location(), fixtures.missing());
            assert_eq!(
                attempt.refusal(),
                &LoadRefusal::MissingEntryPoint {
                    symbol: "NDIlib_send_set_failover".into(),
                    required: true,
                    detail: None
                }
            );
            assert!(!Ndi::is_running());
            let ndi = Ndi::with_options(&options).unwrap();
            assert!(Ndi::is_running());
            assert_eq!(ndi.info().library_path(), Some(complete.as_path()));
            assert_eq!(ndi.info().edition(), SdkEdition::Standard);
            assert_eq!(ndi.info().version().unwrap().minor(), 3);
            let capabilities = ndi.capabilities();
            assert_eq!(
                (capabilities.status(RuntimeFeature::ReceiveDelay)).reason(),
                Some(CapabilityUnavailable::SymbolUnavailable)
            );
            assert_eq!(
                (capabilities.status(RuntimeFeature::Planar444Video)).reason(),
                Some(CapabilityUnavailable::RuntimeTooOld)
            );
            assert!(ndi.runtime().is_supported_cpu());
            assert!(ndi.info().raw_version().contains("6.3.2.0"));
            drop(ndi);
            assert!(!Ndi::is_running());
            drop(Ndi::with_options(&options).unwrap());
            let log = fs::read_to_string(fixtures.log()).unwrap();
            assert_eq!(log, "initialize\ndestroy\ninitialize\ndestroy\n");
        }
        "conflict" => {
            let ndi = Ndi::with_options(&options).unwrap();
            assert!(matches!(
                Ndi::with_options(&explicit(fixtures.missing())),
                Err(Error::RuntimeConflict {
                    kind: SelectionConflict::DifferentLibrary,
                    ..
                })
            ));
            assert!(matches!(
                Ndi::with_options(&RuntimeOptions::default().edition(SdkEdition::Advanced)),
                Err(Error::RuntimeConflict {
                    kind: SelectionConflict::EditionNotMet,
                    ..
                })
            ));
            // The selected module satisfies a request that names no other one.
            drop(Ndi::with_options(&standard()).unwrap());
            drop(ndi);
        }
        "advanced_request_needs_advanced_entry_points" => {
            // A caller's request cannot turn a Standard module into an Advanced
            // runtime, whatever the file is called.
            for standard_module in [complete.clone(), fixtures.mislabelled().join(ADVANCED_NAME)] {
                let reason = load_failure(Ndi::with_options(
                    &explicit(standard_module).edition(SdkEdition::Advanced),
                ));
                assert!(reason.loaded_any());
                assert_eq!(reason.attempts()[0].origin(), LoadOrigin::Explicit);
                assert_eq!(reason.attempts()[0].refusal(), &LoadRefusal::EditionNotMet);
            }
            // The failed request leaves selection open.
            let ndi = Ndi::with_options(&options).unwrap();
            assert_eq!(ndi.info().edition(), SdkEdition::Standard);
        }
        "newer_standard_runtime_is_accepted" => {
            // A Standard runtime that gains an entry point today found only in
            // the Advanced SDK is still a usable Standard runtime.
            let ndi = Ndi::with_options(&explicit(fixtures.newer_standard())).unwrap();
            assert_eq!(ndi.info().edition(), SdkEdition::Standard);
            assert!(Ndi::is_running());
        }
        "advanced_module_satisfies_a_standard_request" => {
            let advanced = fixtures.advanced();
            let ndi = Ndi::with_options(&explicit(advanced.clone())).unwrap();
            assert_eq!(ndi.info().edition(), SdkEdition::Advanced);
            // The edition is detected, so a later Advanced request agrees.
            drop(Ndi::with_options(&explicit(advanced).edition(SdkEdition::Advanced)).unwrap());
            // A Standard request is met by the selected Advanced module too.
            drop(Ndi::with_options(&standard()).unwrap());
        }
        "later_major_version_keeps_version_gated_features" => {
            let ndi = Ndi::with_options(&explicit(fixtures.next_major())).unwrap();
            assert_eq!(ndi.info().version().unwrap().major(), 7);
            for feature in [
                RuntimeFeature::Planar444Video,
                RuntimeFeature::ReceiveBest444,
                RuntimeFeature::ReceiveVideoOnly,
            ] {
                assert!(ndi.capabilities().supports(feature), "{feature:?}");
            }
        }
        "overlong_version_text_is_refused" => {
            // Its terminator lies past the limit: the module is refused for
            // that, rather than read until the terminator however far away.
            let reason = load_failure(Ndi::with_options(&explicit(fixtures.long_version())));
            assert!(reason.loaded_any());
            assert!(matches!(
                reason.attempts()[0].refusal(),
                LoadRefusal::UnreadableVersion { .. }
            ));
            // A refused module selects nothing.
            drop(Ndi::with_options(&options).unwrap());
        }
        "configured_objects_are_refused_for_their_own_capability" => {
            let ndi = Ndi::with_options(&explicit(fixtures.configured_only())).unwrap();
            assert_eq!(ndi.info().edition(), SdkEdition::Advanced);
            #[cfg(feature = "advanced")]
            configured_objects_are_refused_for_their_own_capability(&ndi);
            // Nothing was created, through either entry point.
            let log = fs::read_to_string(fixtures.log()).unwrap();
            assert_eq!(log, "initialize\n");
        }
        "process_lifetime" => {
            let ndi = Ndi::with_options(&options).unwrap();
            let token = ndi.process_lifetime();
            assert_eq!(token.ndi().info(), ndi.info());
            drop(ndi);
            assert!(Ndi::is_running());
            let log = fs::read_to_string(fixtures.log()).unwrap();
            assert_eq!(log, "initialize\n");
        }
        "foreign_symbol" => {
            let reason = load_failure(Ndi::with_options(&explicit(fixtures.foreign())));
            assert!(reason.loaded_any());
            assert_eq!(
                reason.attempts()[0].refusal(),
                &LoadRefusal::EntryPointOutsideModule {
                    symbol: "NDIlib_send_set_failover".into(),
                    module: fixtures.foreign()
                }
            );
            drop(Ndi::with_options(&options).unwrap());
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
                        let ndi = Ndi::with_options(&options).unwrap();
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
            assert!(!Ndi::is_running());
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
            let reason = load_failure(Ndi::with_options(&explicit(absent.clone())));
            assert!(!reason.loaded_any());
            assert_eq!(reason.attempts().len(), 1);
            let attempt = &reason.attempts()[0];
            assert_eq!(attempt.location(), absent);
            assert_eq!(attempt.origin(), LoadOrigin::Explicit);
            assert!(matches!(
                attempt.refusal(),
                LoadRefusal::PlatformLoader { .. }
            ));
            assert!(matches!(
                Ndi::with_options(&standard().library_path("libndi.so.6")),
                Err(Error::InvalidInput(_))
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
            let reason = load_failure(Ndi::with_options(
                &RuntimeOptions::default().edition(SdkEdition::Advanced),
            ));
            let beside_executable = std::env::current_exe()
                .unwrap()
                .with_file_name(ADVANCED_NAME);
            let in_runtime_directory = fixtures.mislabelled().join(ADVANCED_NAME);
            assert!(reason.loaded_any());
            assert_eq!(reason.attempts().len(), 3);
            let attempts = reason.attempts();
            assert_eq!(attempts[0].location(), beside_executable);
            assert_eq!(attempts[0].origin(), LoadOrigin::Executable);
            assert_eq!(attempts[0].refusal(), &LoadRefusal::Absent);
            assert!(!attempts[0].loaded());
            assert_eq!(attempts[1].location(), in_runtime_directory);
            assert_eq!(attempts[1].origin(), LoadOrigin::RuntimeDirectory);
            assert_eq!(attempts[1].refusal(), &LoadRefusal::EditionNotMet);
            assert!(attempts[1].loaded());
            assert_eq!(attempts[2].location(), Path::new(ADVANCED_NAME));
            assert_eq!(attempts[2].origin(), LoadOrigin::LoaderSearch);
            assert!(matches!(
                attempts[2].refusal(),
                LoadRefusal::PlatformLoader { .. }
            ));
            assert!(!attempts[2].loaded());
            // Nothing was selected, so a usable runtime can still be chosen.
            drop(Ndi::with_options(&options).unwrap());
        }
        "selecting_and_starting_are_separate_steps" => {
            // A usable runtime is installed where the default search finds it,
            // yet asking a question loads nothing.
            let installed = fixtures.runtime().join(STANDARD_NAME);
            assert!(!Ndi::is_running());
            assert!(!is_loaded(&installed));

            let runtime = Runtime::select(&standard()).unwrap();
            assert!(is_loaded(&installed));
            assert_eq!(runtime.info().library_path(), Some(installed.as_path()));
            assert!(runtime.info().raw_version().ends_with(IN_RUNTIME_DIRECTORY));
            assert!(runtime.is_supported_cpu());
            assert!(!runtime
                .capabilities()
                .supports(RuntimeFeature::ReceiveDelay));
            // Selected and inspected, and still not started.
            assert!(!Ndi::is_running());
            assert!(!fixtures.log().exists());

            let ndi = runtime.initialize().unwrap();
            assert!(Ndi::is_running());
            assert_eq!(fs::read_to_string(fixtures.log()).unwrap(), "initialize\n");
            // Every handle refers to the one selection.
            let again = Runtime::select(&standard()).unwrap();
            assert!(std::ptr::eq(again.info(), runtime.info()));
            assert!(std::ptr::eq(ndi.info(), runtime.info()));
            assert!(std::ptr::eq(
                ndi.runtime().capabilities(),
                runtime.capabilities()
            ));
            // A handle obtained from another handle shares the running runtime.
            let another = ndi.runtime().initialize().unwrap();
            drop(ndi);
            assert!(Ndi::is_running());
            drop(another);
            assert!(!Ndi::is_running());
            assert_eq!(
                fs::read_to_string(fixtures.log()).unwrap(),
                "initialize\ndestroy\n"
            );
        }
        "a_failed_start_is_classified_and_can_be_retried" => {
            // The fake reads these variables on each call. This process runs
            // only this scenario, so nothing else reads the environment.
            let runtime = Runtime::select(&options).unwrap();
            std::env::set_var("GRAFTON_FAKE_NDI_INITIALIZE", "fail");
            assert!(runtime.is_supported_cpu());
            assert!(matches!(
                runtime.initialize(),
                Err(Error::InitializationFailed(_))
            ));

            std::env::set_var("GRAFTON_FAKE_NDI_CPU", "unsupported");
            assert!(!runtime.is_supported_cpu());
            assert!(matches!(runtime.initialize(), Err(Error::UnsupportedCpu)));
            // The one-call shortcut reports the same cause.
            assert!(matches!(
                Ndi::with_options(&options),
                Err(Error::UnsupportedCpu)
            ));
            assert!(!Ndi::is_running());

            // Neither failure is permanent.
            std::env::remove_var("GRAFTON_FAKE_NDI_INITIALIZE");
            std::env::remove_var("GRAFTON_FAKE_NDI_CPU");
            let ndi = runtime.initialize().unwrap();
            assert!(Ndi::is_running());
            drop(ndi);
            assert_eq!(
                fs::read_to_string(fixtures.log()).unwrap(),
                "initialize\ninitialize\ninitialize\ninitialize\ndestroy\n"
            );
        }
        _ => panic!("unknown loader scenario {name}"),
    }
}

/// A configured router, advertiser or listener goes on to use the functions
/// of a core capability, which `ndi` lacks. Creating one is refused for its
/// own capability, exactly as its status says, with the core capability's
/// reason and evidence, and before any native call.
#[cfg(feature = "advanced")]
fn configured_objects_are_refused_for_their_own_capability(ndi: &Ndi) {
    use grafton_ndi::{
        advanced::AdvancedConfig,
        discovery::{
            AdvertiserOptions, ListenerOptions, RecvAdvertiser, RecvListener, SendAdvertiser,
            SendListener,
        },
        routing::{Router, RouterOptions},
    };
    let config = AdvancedConfig::parse("{}").unwrap();
    let advertisers = AdvertiserOptions::default()
        .servers("127.0.0.1")
        .config(config.clone());
    let listeners = ListenerOptions::default()
        .server("127.0.0.1")
        .config(config.clone());
    let router = RouterOptions::new("configured").config(config);
    // A build with the declarations accepts the configured sender, which the
    // module exports in full; one without them refuses every configured
    // object for its declarations first.
    let declared = ndi
        .capabilities()
        .supports(RuntimeFeature::AdvancedSenderConfiguration);
    for (feature, required, first_missing, error) in [
        (
            RuntimeFeature::AdvancedRouterConfiguration,
            RuntimeFeature::Routing,
            "NDIlib_routing_create",
            Router::new(ndi, &router).err(),
        ),
        (
            RuntimeFeature::AdvancedSenderAdvertiserConfiguration,
            RuntimeFeature::SenderAdvertising,
            "NDIlib_send_advertiser_create",
            SendAdvertiser::new(ndi, &advertisers).err(),
        ),
        (
            RuntimeFeature::AdvancedReceiverAdvertiserConfiguration,
            RuntimeFeature::ReceiverAdvertising,
            "NDIlib_recv_advertiser_create",
            RecvAdvertiser::new(ndi, &advertisers).err(),
        ),
        (
            RuntimeFeature::AdvancedSenderListenerConfiguration,
            RuntimeFeature::SenderDiscovery,
            "NDIlib_send_listener_create",
            SendListener::new(ndi, &listeners).err(),
        ),
        (
            RuntimeFeature::AdvancedReceiverListenerConfiguration,
            RuntimeFeature::ReceiverDiscovery,
            "NDIlib_recv_listener_create",
            RecvListener::new(ndi, &listeners).err(),
        ),
    ] {
        assert_eq!(
            (ndi.capabilities().status(required)).reason(),
            Some(CapabilityUnavailable::SymbolUnavailable),
            "{required:?}"
        );
        let status = ndi.capabilities().status(feature);
        let expected = if declared {
            CapabilityUnavailable::SymbolUnavailable
        } else {
            CapabilityUnavailable::BuildAbiUnavailable
        };
        assert_eq!(status.reason(), Some(expected), "{feature:?}");
        for error in [error, ndi.require_feature(feature).err()] {
            let (refused, refusal) = match error {
                Some(Error::UnsupportedCapability { feature, refusal }) => (feature, refusal),
                other => panic!("{feature:?}: expected an unsupported capability, got {other:?}"),
            };
            assert_eq!((refused, refusal.reason()), (feature, expected));
            assert_eq!(status, CapabilityStatus::Unavailable(refusal));
            if declared {
                // The evidence is the core capability's: its first function.
                assert!(
                    matches!(refusal, grafton_ndi::CapabilityRefusal::SymbolUnavailable(symbol) if symbol.to_str().unwrap() == first_missing),
                    "{feature:?}"
                );
            }
        }
    }
}
