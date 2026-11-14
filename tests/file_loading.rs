//! How the File library is found, checked and pinned, against stand-in
//! libraries. Each scenario runs in its own process, because a selected
//! library stays selected and loaded for the life of the process.
#![cfg(all(target_os = "linux", not(ndi_link_static), feature = "file-io"))]

#[path = "support/fixture.rs"]
mod fixture;
#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;

use fixture::FixtureDir;
use grafton_ndi::{
    capabilities::{CapabilityStatus, CapabilityUnavailable, RuntimeFeature, SdkEdition},
    file::{FileApi, FileFamily, FileRuntimeOptions, PlayerOptions, RecorderOptions, SourceSpec},
    runtime::RuntimeOptions,
    Error, Ndi,
};
use runtime_fixture::FakeRuntime;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// The library names the loader searches for on this target. The vendor's
/// runtimes also carry theirs as their SONAME, by which its File library names
/// the Advanced one as a dependency.
const ADVANCED_CORE: &str = "libndi_advanced.so.6";
const STANDARD_CORE: &str = "libndi.so.6";
const FILE_LIBRARY: &str = "libndifile.so.6";

/// Compile the fake File library to `output`, linked against the runtime at
/// `core` the way the vendor's is: it imports the runtime's entry points and
/// looks for the runtime beside itself.
fn build_file_library(output: &Path, core: &Path, cfgs: &[&str]) -> PathBuf {
    let mut rustc = runtime_fixture::rustc("fake_file_library.rs", output);
    for cfg in cfgs {
        rustc.args(["--cfg", cfg]);
    }
    rustc
        .arg(runtime_fixture::link_argument(core.as_os_str()))
        .arg("-Clink-arg=-Wl,--disable-new-dtags,-rpath,$ORIGIN");
    runtime_fixture::run(rustc, output)
}

/// One arrangement of libraries: a directory, the build number its Advanced
/// runtime reports, and whether that runtime carries the vendor's SONAME.
struct Arrangement {
    directory: &'static str,
    build: u32,
    named: bool,
}

/// A runtime with its File library beside it, as the SDK installs them.
const PAIRED: Arrangement = Arrangement {
    directory: "paired",
    build: 11,
    named: true,
};
/// A runtime alone; a File library for it is in the runtime directory.
const BARE: Arrangement = Arrangement {
    directory: "bare",
    build: 12,
    named: true,
};
/// A second runtime with its own File library.
const OTHER: Arrangement = Arrangement {
    directory: "other",
    build: 13,
    named: true,
};
/// A runtime the platform loader cannot match by name, with a File library
/// that names it by path.
const UNNAMED: Arrangement = Arrangement {
    directory: "unnamed",
    build: 14,
    named: false,
};
/// As [`UNNAMED`], with no File library anywhere that could bind to it.
const LONELY: Arrangement = Arrangement {
    directory: "lonely",
    build: 15,
    named: false,
};

/// The libraries the scenarios use.
struct Fixtures {
    root: PathBuf,
}

impl Fixtures {
    fn build(root: &Path) -> Self {
        let fixtures = Self {
            root: root.to_owned(),
        };
        for directory in ["standard", "runtime", "partial"] {
            fs::create_dir(root.join(directory)).unwrap();
        }
        let paired = fixtures.build_core(&PAIRED);
        build_file_library(&fixtures.file(PAIRED.directory), &paired, &[]);
        let bare = fixtures.build_core(&BARE);
        build_file_library(&fixtures.file("runtime"), &bare, &[]);
        let other = fixtures.build_core(&OTHER);
        build_file_library(&fixtures.file(OTHER.directory), &other, &[]);
        let unnamed = fixtures.build_core(&UNNAMED);
        build_file_library(&fixtures.file(UNNAMED.directory), &unnamed, &[]);
        fixtures.build_core(&LONELY);
        // A File library for the paired runtime that lacks a recording entry point.
        build_file_library(
            &fixtures.file("partial"),
            &paired,
            &["fake_omit_recording_entry"],
        );
        FakeRuntime::default().build(&fixtures.standard_core());
        fixtures
    }

    fn build_core(&self, arrangement: &Arrangement) -> PathBuf {
        fs::create_dir(self.root.join(arrangement.directory)).unwrap();
        let version = format!("6.4.0.{}", arrangement.build);
        let mut runtime = FakeRuntime {
            cfgs: vec!["fake_advanced"],
            version: Some(&version),
            ..Default::default()
        };
        if arrangement.named {
            runtime.soname = Some(ADVANCED_CORE);
        }
        runtime.build(&self.core(arrangement))
    }

    fn core(&self, arrangement: &Arrangement) -> PathBuf {
        self.root.join(arrangement.directory).join(ADVANCED_CORE)
    }
    fn file(&self, directory: &str) -> PathBuf {
        self.root.join(directory).join(FILE_LIBRARY)
    }
    fn standard_core(&self) -> PathBuf {
        self.root.join("standard").join(STANDARD_CORE)
    }
}

/// How the parent launches one scenario.
struct Scenario {
    name: &'static str,
    /// Directory for `NDI_RUNTIME_DIR_V6`, relative to the fixture root.
    runtime_directory: Option<&'static str>,
    /// A runtime to put in the process's global symbol scope with `LD_PRELOAD`.
    preloaded: Option<&'static Arrangement>,
}

const fn scenario(name: &'static str) -> Scenario {
    Scenario {
        name,
        runtime_directory: None,
        preloaded: None,
    }
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        runtime_directory: Some("runtime"),
        ..scenario("beside_the_runtime_first_and_pinned")
    },
    Scenario {
        runtime_directory: Some("runtime"),
        ..scenario("runtime_directory_when_not_beside_the_runtime")
    },
    scenario("standard_runtime_is_refused_before_anything_is_opened"),
    scenario("library_whose_dependency_is_another_runtime_is_refused"),
    Scenario {
        preloaded: Some(&UNNAMED),
        ..scenario("another_runtime_in_the_global_scope_is_refused_before_anything_is_opened")
    },
    scenario("failure_names_every_location"),
    scenario("incomplete_family_is_unavailable_alone"),
];

#[test]
fn file_library_loader_contract() {
    if let Ok(name) = std::env::var("GRAFTON_FILE_LOADER_CHILD") {
        let root = PathBuf::from(std::env::var_os("GRAFTON_FILE_LOADER_FIXTURES").unwrap());
        child(&name, &Fixtures { root });
        return;
    }
    let directory = FixtureDir::create("file-loader");
    let fixtures = Fixtures::build(directory.path());
    for scenario in SCENARIOS {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "file_library_loader_contract", "--nocapture"])
            .env("GRAFTON_FILE_LOADER_CHILD", scenario.name)
            .env("GRAFTON_FILE_LOADER_FIXTURES", directory.path())
            .env_remove("NDI_RUNTIME_DIR_V6")
            .env_remove("LD_LIBRARY_PATH");
        if let Some(runtime_directory) = scenario.runtime_directory {
            command.env(
                "NDI_RUNTIME_DIR_V6",
                directory.path().join(runtime_directory),
            );
        }
        if let Some(arrangement) = scenario.preloaded {
            command.env("LD_PRELOAD", fixtures.core(arrangement));
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
}

/// Select the Advanced runtime of `arrangement`.
fn select(fixtures: &Fixtures, arrangement: &Arrangement) -> Ndi {
    Ndi::with_options(
        &RuntimeOptions::default()
            .library_path(fixtures.core(arrangement))
            .edition(SdkEdition::Advanced),
    )
    .unwrap()
}

fn explicit(path: PathBuf) -> FileRuntimeOptions {
    FileRuntimeOptions::default().library_path(path)
}

/// Why no File library could be loaded.
fn unavailable(result: Result<FileApi, Error>) -> String {
    match result {
        Err(Error::RuntimeLoadFailed(reason)) => reason,
        Err(other) => panic!("expected the File library to be unavailable, got {other}"),
        Ok(api) => panic!("expected the File library to be unavailable, but loaded {api:?}"),
    }
}

/// Why the File library cannot accompany the selected runtime.
fn conflicting(result: Result<FileApi, Error>) -> String {
    match result {
        Err(Error::RuntimeConflict(reason)) => reason,
        Err(other) => panic!("expected the selected runtime to be refused, got {other}"),
        Ok(api) => panic!("expected the selected runtime to be refused, but loaded {api:?}"),
    }
}

/// The build number of the runtime the loaded File library's imports are
/// bound to, which the fake library reports through a probe.
fn bound_runtime(api: &FileApi) -> u32 {
    api.probe("recording.mkv").unwrap().track_count()
}

/// Whether a file whose path contains `name` is mapped into this process.
fn is_loaded(name: &str) -> bool {
    fs::read_to_string("/proc/self/maps")
        .unwrap()
        .lines()
        .any(|line| line.contains(name))
}

fn child(name: &str, fixtures: &Fixtures) {
    let defaults = FileRuntimeOptions::default();
    match name {
        "beside_the_runtime_first_and_pinned" => {
            let ndi = select(fixtures, &PAIRED);
            let api = FileApi::open(&ndi, &defaults).unwrap();
            let selected = fixtures.file(PAIRED.directory);
            assert_eq!(api.library_path(), Some(selected.as_path()));
            assert_eq!(bound_runtime(&api), PAIRED.build);

            // A different library cannot replace the selected one.
            assert!(matches!(
                FileApi::open(&ndi, &explicit(fixtures.file(OTHER.directory))),
                Err(Error::RuntimeConflict(_))
            ));
            assert!(matches!(
                FileApi::open(&ndi, &explicit(PathBuf::from(FILE_LIBRARY))),
                Err(Error::InvalidInput(_))
            ));
            FileApi::open(&ndi, &explicit(selected.clone())).unwrap();

            // Dropping every handle unloads nothing, so entry points held by
            // anything still running stay valid, and the selection stands.
            drop(api);
            drop(ndi);
            assert!(!Ndi::is_running());
            assert!(is_loaded(selected.to_str().unwrap()));
            let ndi = select(fixtures, &PAIRED);
            let api = FileApi::open(&ndi, &defaults).unwrap();
            assert_eq!(api.library_path(), Some(selected.as_path()));
            assert_eq!(bound_runtime(&api), PAIRED.build);
        }
        "runtime_directory_when_not_beside_the_runtime" => {
            let ndi = select(fixtures, &BARE);
            let api = FileApi::open(&ndi, &defaults).unwrap();
            assert_eq!(api.library_path(), Some(fixtures.file("runtime").as_path()));
            // It has no runtime beside it: the loader bound it to the selected
            // one, which is already loaded under the name it asks for.
            assert_eq!(bound_runtime(&api), BARE.build);
        }
        "standard_runtime_is_refused_before_anything_is_opened" => {
            let ndi = Ndi::with_options(
                &RuntimeOptions::default()
                    .library_path(fixtures.standard_core())
                    .edition(SdkEdition::Standard),
            )
            .unwrap();
            // A File library that exists and would load.
            let reason = conflicting(FileApi::open(
                &ndi,
                &explicit(fixtures.file(PAIRED.directory)),
            ));
            assert!(reason.contains("Advanced SDK runtime"), "{reason}");
            assert!(!is_loaded(FILE_LIBRARY));
            assert!(!is_loaded(ADVANCED_CORE));
            // The runtime itself is unaffected.
            assert!(Ndi::is_running());
            drop(ndi);
        }
        "library_whose_dependency_is_another_runtime_is_refused" => {
            // The selected runtime cannot be matched by name, so this File
            // library's dependency is satisfied by the runtime beside it.
            let ndi = select(fixtures, &UNNAMED);
            let foreign = fixtures.file(OTHER.directory);
            let reason = unavailable(FileApi::open(&ndi, &explicit(foreign.clone())));
            assert!(
                reason.contains("bound to an NDI runtime other than the selected one"),
                "{reason}"
            );
            assert!(reason.contains(foreign.to_str().unwrap()), "{reason}");
            // Nothing was selected, so the right library can still be.
            let api = FileApi::open(&ndi, &defaults).unwrap();
            assert_eq!(
                api.library_path(),
                Some(fixtures.file(UNNAMED.directory).as_path())
            );
            assert_eq!(bound_runtime(&api), UNNAMED.build);
        }
        "another_runtime_in_the_global_scope_is_refused_before_anything_is_opened" => {
            // Another runtime was loaded with the program, under a name the
            // File library does not ask for. The library's own dependency is
            // the selected runtime, but the platform loader binds its imports
            // from the global scope first: to the other runtime.
            let ndi = select(fixtures, &PAIRED);
            let preloaded = fixtures.core(&UNNAMED);
            let reason = conflicting(FileApi::open(&ndi, &defaults));
            assert!(reason.contains("global symbol scope"), "{reason}");
            assert!(reason.contains(preloaded.to_str().unwrap()), "{reason}");
            assert!(!is_loaded(FILE_LIBRARY));
        }
        "failure_names_every_location" => {
            // No File library can bind to this runtime, so the outcome does
            // not depend on what the machine has installed.
            let ndi = select(fixtures, &LONELY);
            let reason = unavailable(FileApi::open(&ndi, &defaults));
            let beside_executable = std::env::current_exe()
                .unwrap()
                .with_file_name(FILE_LIBRARY);
            for expected in [
                format!(
                    "{} (next to the NDI runtime): not found",
                    fixtures.file(LONELY.directory).display()
                ),
                format!(
                    "{} (next to the executable): not found",
                    beside_executable.display()
                ),
                format!("{FILE_LIBRARY} (system library search)"),
            ] {
                assert!(
                    reason.contains(&expected),
                    "missing `{expected}` in: {reason}"
                );
            }
            // A failed search leaves selection open: it fails the same way again.
            unavailable(FileApi::open(&ndi, &defaults));
        }
        "incomplete_family_is_unavailable_alone" => {
            let ndi = select(fixtures, &PAIRED);
            // A library that lacks one family is selected for the other.
            let api = FileApi::open(&ndi, &explicit(fixtures.file("partial"))).unwrap();
            assert_eq!(
                api.status(FileFamily::Recording),
                CapabilityStatus::Unavailable(CapabilityUnavailable::SymbolUnavailable)
            );
            assert!(api.supports(FileFamily::Playback));
            let options = RecorderOptions::new(SourceSpec::new("a source").unwrap());
            match api.create_recorder(&options) {
                Err(Error::UnsupportedCapability {
                    feature,
                    reason,
                    detail,
                }) => {
                    assert_eq!(feature, RuntimeFeature::File(FileFamily::Recording));
                    assert_eq!(reason, CapabilityUnavailable::SymbolUnavailable);
                    assert_eq!(detail, "NDIlib_file_recorder_rollover");
                }
                other => panic!("expected recording to be unavailable: {other:?}"),
            }
            // The library stays selected, and the family stays unavailable.
            let reopened = FileApi::open(&ndi, &defaults).unwrap();
            assert_eq!(
                reopened.library_path(),
                Some(fixtures.file("partial").as_path())
            );
            assert!(!reopened.supports(FileFamily::Recording));
            // The runtime's own capabilities claim nothing about the library.
            assert_eq!(
                ndi.capabilities()
                    .status(RuntimeFeature::File(FileFamily::Playback)),
                CapabilityStatus::Unavailable(CapabilityUnavailable::SeparateLibrary)
            );
            // Playback is complete in this library and still reaches it.
            assert_eq!(bound_runtime(&api), PAIRED.build);
            assert!(matches!(
                api.create_player(&PlayerOptions::new("a player")),
                Err(Error::NativeOperationFailed { .. })
            ));
        }
        _ => panic!("unknown File loader scenario {name}"),
    }
}
