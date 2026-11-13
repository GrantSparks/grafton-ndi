//! Native selection/recording acceptance in its own SDK process and CI sandbox.
#![cfg(feature = "file-io")]

mod common;
#[path = "support/fixture.rs"]
mod fixture;

use fixture::FixtureDir;
use grafton_ndi::{
    file::{
        FileNaming, FinishStatus, RecorderEventKind, RecorderOptions, RecordingTarget,
        RecordingTimecode, SourceSpec,
    },
    MediaTypes,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

const PATIENCE: Duration = Duration::from_secs(15);

#[test]
fn configured_discovery_preserves_endpoint_and_explicit_name_recording() {
    // This executable has one test and loads no SDK before installing its own
    // process configuration. IP queries provide loopback discovery without
    // changing system or concurrent test-process configuration.
    let fixture = FixtureDir::create("file-source-selection");
    let configuration = fixture.path().join("config");
    let temporary = fixture.path().join("tmp");
    std::fs::create_dir(&configuration).unwrap();
    std::fs::create_dir(&temporary).unwrap();
    // Only macOS needs a discovery address to suppress sender mDNS, matching its
    // offline runner. Leave it unset elsewhere: an unreachable Discovery Server
    // can block Windows receivers (see docs/troubleshooting.md).
    let config = if cfg!(target_os = "macos") {
        r#"{"ndi":{"networks":{"discovery":"127.0.0.1","ips":"127.0.0.1"},"groups":{"send":"Public","recv":"Public"}}}"#
    } else {
        r#"{"ndi":{"networks":{"ips":"127.0.0.1"},"groups":{"send":"Public","recv":"Public"}}}"#
    };
    std::fs::write(configuration.join("ndi-config.v1.json"), config).unwrap();
    std::env::set_var("NDI_CONFIG_DIR", &configuration);
    std::env::set_var("TMPDIR", &temporary);
    std::env::set_var("TMP", &temporary);
    std::env::set_var("TEMP", &temporary);
    let Some((ndi, api)) = common::file_api() else {
        return;
    };
    let group = format!("grafton-selected-{}", std::process::id());
    let mut senders = Vec::new();
    for (label, dimensions, groups) in [
        ("wrong-feed", (320, 180), Some(group.clone())),
        ("selected-feed", (640, 360), Some(group.clone())),
        ("logical-feed", (480, 270), None),
    ] {
        let mut options = grafton_ndi::SenderOptions::new(format!(
            "grafton-selected-{}-{label}",
            std::process::id()
        ));
        if let Some(groups) = groups {
            options = options.groups(groups);
        }
        let sender = grafton_ndi::Sender::new(&ndi, &options).unwrap();
        senders.push((sender, dimensions));
    }
    let selected_name = senders[1].0.source().unwrap().name().to_owned();
    let logical_name = senders[2].0.source().unwrap().name().to_owned();
    let running = Arc::new(AtomicBool::new(true));
    thread::scope(|scope| {
        for (sender, (width, height)) in senders {
            let running = Arc::clone(&running);
            scope.spawn(move || {
                let frame = grafton_ndi::VideoFrame::builder()
                    .resolution(width, height)
                    .build()
                    .unwrap();
                while running.load(Ordering::Acquire) {
                    sender.send_video(&frame).unwrap();
                    thread::sleep(Duration::from_millis(10));
                }
            });
        }
        struct Stop(Arc<AtomicBool>);
        impl Drop for Stop {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _stop = Stop(Arc::clone(&running));
        let finder = grafton_ndi::Finder::new(
            &ndi,
            &grafton_ndi::FinderOptions::new()
                .groups(&group)
                .extra_ips("127.0.0.1"),
        )
        .unwrap();
        let selected = finder
            .find_source_by_name(&selected_name, PATIENCE)
            .unwrap()
            .unwrap();
        let address = match selected.address().expect("discovered endpoint") {
            grafton_ndi::SourceAddress::Ip(address) | grafton_ndi::SourceAddress::Url(address) => {
                address
            }
        };
        let spec = SourceSpec::try_from(&selected).unwrap();
        assert_eq!(spec.name(), None);
        assert_eq!(spec.url(), Some(address.as_str()));
        // The child config gives native default discovery a loopback IP hint;
        // deliberate name selection must work without per-Finder configuration.
        let default_finder =
            grafton_ndi::Finder::new(&ndi, &grafton_ndi::FinderOptions::new()).unwrap();
        let named = default_finder
            .find_source_by_name(&logical_name, PATIENCE)
            .unwrap()
            .expect("default discovery did not find the logical sender");
        assert_eq!(named.name(), logical_name);
        let policies = [
            ("endpoint", spec, (640, 360)),
            (
                "logical",
                SourceSpec::new(&logical_name).unwrap(),
                (480, 270),
            ),
        ];
        // Distinguishable recorded dimensions establish which feed delivered the
        // media, rather than merely checking that native creation succeeded.
        for (label, spec, expected) in policies {
            let file = fixture.path().join(format!("{label}.mkv"));
            let options = RecorderOptions::new(spec.clone())
                .naming(FileNaming::Exact)
                .proxy(false)
                .tracks(MediaTypes::VIDEO);
            let mut recorder = api.create_recorder(&options).unwrap();
            assert_eq!(recorder.last_accepted_source(), Some(&spec));
            recorder.set_source(&spec).unwrap();
            assert_eq!(recorder.last_accepted_source(), Some(&spec));
            recorder
                .start(
                    &RecordingTarget::new(&file).unwrap(),
                    RecordingTimecode::Preserve,
                )
                .unwrap();
            let deadline = Instant::now() + PATIENCE;
            let first = loop {
                assert!(
                    Instant::now() < deadline,
                    "no FirstFrame for {label} selection"
                );
                let Some(event) = recorder.poll_event(Duration::from_millis(50)).unwrap() else {
                    continue;
                };
                match event.kind() {
                    RecorderEventKind::FirstFrame => break event,
                    RecorderEventKind::Error | RecorderEventKind::OpenFailed => {
                        panic!("{label} selection failed: {event:?}")
                    }
                    _ => {}
                }
            };
            let track = &first.source_format().video_tracks()[0];
            assert_eq!((track.width(), track.height()), expected);
            thread::sleep(Duration::from_millis(150));
            assert!(recorder.stats().unwrap().unwrap().video_frames() > 0);
            let FinishStatus::Closed(report) = recorder.finish(PATIENCE).unwrap() else {
                panic!("the selected recording was not finalized");
            };
            assert_eq!(report.error_events(), 0);
            assert_eq!(report.empty_files(), 0);
            let info = api.probe(&file).unwrap();
            let track = &info.source_format().video_tracks()[0];
            assert_eq!((track.width(), track.height()), expected);
            assert!(info.duration().is_final());
            eprintln!(
                "verified {label} recording: {}x{} with video frames and a finalized probe",
                expected.0, expected.1
            );
        }
        // Pair an unavailable endpoint with the public name that just delivered
        // media, so a hidden name fallback would record the logical feed.
        let unavailable = grafton_ndi::Source::new(&logical_name)
            .with_address(grafton_ndi::SourceAddress::Ip("127.0.0.1:9".into()));
        let spec = SourceSpec::try_from(&unavailable).unwrap();
        assert_eq!(spec.name(), None);
        assert_eq!(spec.url(), Some("127.0.0.1:9"));
        let mut recorder = api
            .create_recorder(
                &RecorderOptions::new(spec.clone())
                    .naming(FileNaming::Exact)
                    .proxy(false)
                    .tracks(MediaTypes::VIDEO),
            )
            .unwrap();
        assert_eq!(recorder.last_accepted_source(), Some(&spec));
        recorder
            .start(
                &RecordingTarget::new(fixture.path().join("unavailable.mkv")).unwrap(),
                RecordingTimecode::Preserve,
            )
            .unwrap();
        thread::sleep(Duration::from_millis(500));
        assert!(recorder
            .stats()
            .unwrap()
            .is_none_or(|stats| stats.video_frames() == 0));
        let FinishStatus::Closed(report) = recorder.finish(PATIENCE).unwrap() else {
            panic!("the unavailable endpoint recording was not finalized");
        };
        assert_eq!(report.empty_files(), 1);
        eprintln!("verified unavailable endpoint: no fallback to the reachable logical sender");
    });
}
