//! Exact-name collisions inserted between preflight and the native open.
//! The interposer targets the Linux x86-64 File SDK's open64 call. It does
//! not establish behavior on other platforms or pin mutable parent paths.
#![cfg(all(
    target_os = "linux",
    target_arch = "x86_64",
    not(ndi_link_static),
    feature = "file-io"
))]
#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(runtime, prerequisites);
#[path = "support/fixture.rs"]
mod fixture;
use fixture::FixtureDir;
use grafton_ndi::{
    file::{
        FileApi, FileNaming, FileRuntimeOptions, FinishStatus, RecorderEventKind, RecorderOptions,
        RecorderState, RecordingTarget, RecordingTimecode, SourceSpec,
    },
    Error, SelectionConflict,
};
use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};
const SENTINEL: &[u8] = b"GRAFTON_RACE_SENTINEL\n";

#[test]
fn exact_name_races_preserve_files_and_report_native_failures() {
    let Some(ndi) = common::ndi() else {
        return;
    };
    let api = match FileApi::open(&ndi, &FileRuntimeOptions::new()) {
        Ok(api) => api,
        Err(Error::RuntimeLoadFailed(reason)) if !reason.loaded_any() => {
            common::skip(common::Requirement::File, &reason.to_string());
            return;
        }
        Err(Error::RuntimeConflict {
            kind: SelectionConflict::FileNeedsAdvancedRuntime,
            detail,
        }) => {
            common::skip(common::Requirement::File, &detail);
            return;
        }
        Err(error) => panic!("{error}"),
    };
    if let Ok(operation) = std::env::var("GRAFTON_FILE_RACE_OPERATION") {
        let target = PathBuf::from(std::env::var_os("GRAFTON_FILE_RACE_TARGET").unwrap());
        let options = RecorderOptions::new(SourceSpec::from_url("127.0.0.1:59999").unwrap())
            .naming(FileNaming::Exact)
            .drop_wait(Duration::from_secs(1));
        let mut recorder = api.create_recorder(&options).unwrap();
        let target = RecordingTarget::new(&target).unwrap();
        if operation == "start" {
            assert!(matches!(
                recorder.start(&target, RecordingTimecode::Preserve),
                Err(Error::NativeOperationFailed { .. })
            ));
            assert!(!recorder.is_recording());
        } else {
            let first = PathBuf::from(std::env::var_os("GRAFTON_FILE_RACE_FIRST").unwrap());
            recorder
                .start(
                    &RecordingTarget::new(first).unwrap(),
                    RecordingTimecode::Preserve,
                )
                .unwrap();
            // Let the native session publish its initial open before rollover.
            for _ in 0..5 {
                recorder.poll_event(Duration::from_millis(20)).unwrap();
            }
            recorder.rollover(&target).unwrap();
            // With no incoming packets the SDK defers the cut. Stopping
            // flushes that pending request and exposes its native open error.
            recorder.request_stop();
            let deadline = Instant::now() + Duration::from_secs(5);
            let (mut open_failed, mut error) = (false, false);
            let mut events = Vec::new();
            while Instant::now() < deadline && !(open_failed && error && !recorder.is_recording()) {
                if let Some(event) = recorder.poll_event(Duration::from_millis(20)).unwrap() {
                    events.push(event.kind());
                    open_failed |= event.kind() == RecorderEventKind::OpenFailed;
                    error |= event.kind() == RecorderEventKind::Error;
                }
            }
            assert!(
                open_failed && error && !recorder.is_recording(),
                "events={events:?}, recording={}, state={:?}",
                recorder.is_recording(),
                recorder.state()
            );
            let FinishStatus::Closed(report) = recorder.finish(Duration::from_secs(2)).unwrap()
            else {
                panic!("session not closed")
            };
            assert_eq!(report.closed_files(), 1);
            assert!(report.error_events() >= 2);
            assert_eq!(recorder.state(), RecorderState::Closed);
        }
        return;
    }
    let directory = FixtureDir::create("file-exact-race");
    let interposer = directory.path().join("race.so");
    let output = Command::new("cc")
        .args(["-std=c11", "-shared", "-fPIC"])
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/file_open_race.c"
        ))
        .arg("-o")
        .arg(&interposer)
        .arg("-ldl")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for operation in ["start", "rollover"] {
        for kind in ["regular", "symlink", "dangling"] {
            let root = directory.path().join(format!("{operation}-{kind}"));
            fs::create_dir(&root).unwrap();
            let target = root.join("target.mkv");
            let victim = root.join("victim.mkv");
            if kind == "symlink" {
                fs::write(&victim, SENTINEL).unwrap();
            }
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "exact_name_races_preserve_files_and_report_native_failures",
                    "--nocapture",
                ])
                .env("LD_PRELOAD", &interposer)
                .env("GRAFTON_FILE_RACE_OPERATION", operation)
                .env("GRAFTON_FILE_RACE_TARGET", &target)
                .env("GRAFTON_FILE_RACE_FIRST", root.join("first.mkv"));
            if kind != "regular" {
                command.env("GRAFTON_FILE_RACE_SYMLINK", &victim);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{operation} {kind}: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            match kind {
                "regular" => assert_eq!(fs::read(&target).unwrap(), SENTINEL),
                "symlink" => {
                    assert_eq!(fs::read(&victim).unwrap(), SENTINEL);
                    assert_eq!(fs::read_link(&target).unwrap(), victim);
                }
                _ => {
                    assert!(!victim.exists());
                    assert_eq!(fs::read_link(&target).unwrap(), victim);
                }
            }
        }
    }
}
