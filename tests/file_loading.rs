//! How the File library is found, checked and pinned, against stand-in
//! libraries. Each scenario runs in its own process, because a selected
//! library stays selected and loaded for the life of the process.
#![cfg(all(target_os = "linux", not(ndi_link_static), feature = "file-io"))]

#[path = "support/fixture.rs"]
mod fixture;

use fixture::FakeRuntime;
use grafton_ndi::{
    capabilities::SdkEdition,
    file::{FileApi, FileError, FileRuntimeOptions, PlayerOptions, RecorderOptions, SourceSpec},
    runtime::RuntimeOptions,
    Error, NDI,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// The library names the loader searches for on this target.
const ADVANCED_CORE: &str = "libndi_advanced.so.6";
const STANDARD_CORE: &str = "libndi.so.6";
const FILE_LIBRARY: &str = "libndifile.so.6";

/// Compile the fake File library to `output`, bound to the runtime at `core`
/// the way the vendor's is: by a dependency the platform loader resolves.
fn build_file_library(output: &Path, core: &Path, cfgs: &[&str]) -> PathBuf {
    let mut rustc = fixture::rustc("fake_file_library.rs", output);
    for cfg in cfgs {
        rustc.args(["--cfg", cfg]);
    }
    rustc
        .arg("-Clink-arg=-Wl,--no-as-needed")
        .arg(fixture::link_argument(core.as_os_str()))
        .arg("-Clink-arg=-Wl,-rpath,$ORIGIN");
    fixture::run(rustc, output)
}

/// The libraries the scenarios use, each directory holding one arrangement.
struct Fixtures {
    root: PathBuf,
}

impl Fixtures {
    fn build(root: &Path) -> Self {
        let fixtures = Self {
            root: root.to_owned(),
        };
        for directory in ["paired", "bare", "other", "standard", "runtime", "partial"] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        // An Advanced runtime with its File library beside it.
        let paired = FakeRuntime::new()
            .with("fake_advanced")
            .build(&fixtures.core("paired"));
        build_file_library(&fixtures.file("paired"), &paired, &[]);
        // An Advanced runtime alone; its File library is in the runtime directory.
        let bare = FakeRuntime::new()
            .with("fake_advanced")
            .build(&fixtures.core("bare"));
        build_file_library(&fixtures.file("runtime"), &bare, &[]);
        // A second Advanced runtime, and a File library bound to that one.
        let other = FakeRuntime::new()
            .with("fake_advanced")
            .build(&fixtures.core("other"));
        build_file_library(&fixtures.file("other"), &other, &[]);
        // A File library for the paired runtime that lacks a recording entry point.
        build_file_library(
            &fixtures.file("partial"),
            &paired,
            &["fake_omit_recording_entry"],
        );
        FakeRuntime::new().build(&fixtures.standard_core());
        fixtures
    }

    fn core(&self, directory: &str) -> PathBuf {
        self.root.join(directory).join(ADVANCED_CORE)
    }
    fn file(&self, directory: &str) -> PathBuf {
        self.root.join(directory).join(FILE_LIBRARY)
    }
    fn standard_core(&self) -> PathBuf {
        self.root.join("standard").join(STANDARD_CORE)
    }
}

/// A scenario and the directory it names in `NDI_RUNTIME_DIR_V6`, if any.
const SCENARIOS: &[(&str, Option<&str>)] = &[
    ("beside_the_runtime_first_and_pinned", Some("runtime")),
    (
        "runtime_directory_when_not_beside_the_runtime",
        Some("runtime"),
    ),
    (
        "standard_runtime_is_refused_before_anything_is_opened",
        None,
    ),
    ("library_bound_to_another_runtime_is_refused", None),
    ("failure_names_every_location", None),
    ("incomplete_family_is_unavailable_alone", None),
];

#[test]
fn file_library_loader_contract() {
    if let Ok(name) = std::env::var("GRAFTON_FILE_LOADER_CHILD") {
        let root = PathBuf::from(std::env::var_os("GRAFTON_FILE_LOADER_FIXTURES").unwrap());
        child(&name, &Fixtures { root });
        return;
    }
    let root = std::env::temp_dir().join(format!("grafton-ndi-file-loader-{}", std::process::id()));
    Fixtures::build(&root);
    for (scenario, runtime_directory) in SCENARIOS {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "file_library_loader_contract", "--nocapture"])
            .env("GRAFTON_FILE_LOADER_CHILD", scenario)
            .env("GRAFTON_FILE_LOADER_FIXTURES", &root)
            .env_remove("NDI_RUNTIME_DIR_V6")
            .env_remove("LD_LIBRARY_PATH");
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

/// Select the runtime at `core`.
fn select(core: PathBuf, edition: SdkEdition) -> NDI {
    NDI::with_options(
        &RuntimeOptions::default()
            .library_path(core)
            .edition(edition),
    )
    .unwrap()
}

fn explicit(path: PathBuf) -> FileRuntimeOptions {
    FileRuntimeOptions::default().library_path(path)
}

fn unavailable(result: Result<FileApi, FileError>) -> String {
    match result {
        Err(FileError::Unavailable(reason)) => reason,
        Err(other) => panic!("expected the File library to be unavailable, got {other}"),
        Ok(api) => panic!("expected the File library to be unavailable, but loaded {api:?}"),
    }
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
            let ndi = select(fixtures.core("paired"), SdkEdition::Advanced);
            let api = FileApi::open(&ndi, &defaults).unwrap();
            let selected = fixtures.file("paired");
            assert_eq!(api.library_path(), Some(selected.as_path()));
            // The call reaches the library, which refuses every file.
            assert!(matches!(
                api.probe("recording.mkv"),
                Err(FileError::Rejected(_))
            ));

            // A different library cannot replace the selected one.
            assert!(matches!(
                FileApi::open(&ndi, &explicit(fixtures.file("other"))),
                Err(FileError::Core(Error::RuntimeConflict(_)))
            ));
            assert!(matches!(
                FileApi::open(&ndi, &explicit(PathBuf::from(FILE_LIBRARY))),
                Err(FileError::Core(Error::InvalidConfiguration(_)))
            ));
            FileApi::open(&ndi, &explicit(selected.clone())).unwrap();

            // Dropping every handle unloads nothing, so entry points held by
            // anything still running stay valid, and the selection stands.
            drop(api);
            drop(ndi);
            assert!(!NDI::is_running());
            assert!(is_loaded(selected.to_str().unwrap()));
            let ndi = select(fixtures.core("paired"), SdkEdition::Advanced);
            let api = FileApi::open(&ndi, &defaults).unwrap();
            assert_eq!(api.library_path(), Some(selected.as_path()));
            assert!(matches!(
                api.probe("recording.mkv"),
                Err(FileError::Rejected(_))
            ));
        }
        "runtime_directory_when_not_beside_the_runtime" => {
            let ndi = select(fixtures.core("bare"), SdkEdition::Advanced);
            let api = FileApi::open(&ndi, &defaults).unwrap();
            assert_eq!(api.library_path(), Some(fixtures.file("runtime").as_path()));
        }
        "standard_runtime_is_refused_before_anything_is_opened" => {
            let ndi = select(fixtures.standard_core(), SdkEdition::Standard);
            // A File library that exists and would load.
            let reason = unavailable(FileApi::open(&ndi, &explicit(fixtures.file("paired"))));
            assert!(reason.contains("Advanced SDK runtime"), "{reason}");
            assert!(!is_loaded(FILE_LIBRARY));
            assert!(!is_loaded(ADVANCED_CORE));
            // The runtime itself is unaffected.
            assert!(NDI::is_running());
            drop(ndi);
        }
        "library_bound_to_another_runtime_is_refused" => {
            let ndi = select(fixtures.core("paired"), SdkEdition::Advanced);
            let reason = unavailable(FileApi::open(&ndi, &explicit(fixtures.file("other"))));
            assert!(
                reason.contains("bound to an NDI runtime other than the selected one"),
                "{reason}"
            );
            assert!(
                reason.contains(fixtures.file("other").to_str().unwrap()),
                "{reason}"
            );
            // Nothing was selected, so the right library can still be.
            let api = FileApi::open(&ndi, &defaults).unwrap();
            assert_eq!(api.library_path(), Some(fixtures.file("paired").as_path()));
        }
        "failure_names_every_location" => {
            let ndi = select(fixtures.core("bare"), SdkEdition::Advanced);
            let reason = unavailable(FileApi::open(&ndi, &defaults));
            let beside_executable = std::env::current_exe()
                .unwrap()
                .with_file_name(FILE_LIBRARY);
            for expected in [
                format!(
                    "{} (next to the NDI runtime): not found",
                    fixtures.file("bare").display()
                ),
                format!(
                    "{} (next to the executable): not found",
                    beside_executable.display()
                ),
                format!("{FILE_LIBRARY} (system library search): "),
                "cannot open shared object file".to_owned(),
            ] {
                assert!(
                    reason.contains(&expected),
                    "missing `{expected}` in: {reason}"
                );
            }
            // A failed search leaves selection open.
            FileApi::open(&ndi, &explicit(fixtures.file("runtime"))).unwrap();
        }
        "incomplete_family_is_unavailable_alone" => {
            let ndi = select(fixtures.core("paired"), SdkEdition::Advanced);
            let api = FileApi::open(&ndi, &explicit(fixtures.file("partial"))).unwrap();
            let options = RecorderOptions::new(SourceSpec::new("a source").unwrap());
            match api.create_recorder(options) {
                Err(FileError::Unavailable(reason)) => {
                    assert!(reason.contains("NDIlib_file_recorder_rollover"), "{reason}");
                }
                other => panic!("expected recording to be unavailable: {other:?}"),
            }
            // Playback is complete in this library and still reaches it.
            assert!(matches!(
                api.create_player(PlayerOptions::new("a player").unwrap()),
                Err(FileError::Rejected(_))
            ));
        }
        _ => panic!("unknown File loader scenario {name}"),
    }
}
