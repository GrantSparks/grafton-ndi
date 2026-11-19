//! Receiver control forwarding and held-input clocked capture without an SDK.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(child, fixture);
use common::TestChild;

#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;
use runtime_fixture::read_fake_log;

#[cfg(feature = "advanced")]
use grafton_ndi::{
    advanced::MouseButton, MediaTypes, RuntimeFeature, ScanType, SettingChange, Source, StreamTier,
};
use grafton_ndi::{
    Error, FrameDefect, FrameSync, FrameSyncAudioRequest, LayoutDefect, Ndi, Receiver,
    ReceiverOptions, RuntimeOptions,
};
use std::time::Duration;

#[cfg(feature = "advanced")]
#[test]
fn kvm_release_precedes_connection_changes_on_the_same_receiver() {
    if let Some(library) = std::env::var_os("GRAFTON_KVM_CHILD") {
        exercise_kvm(library.into());
        return;
    }
    let directory = common::FixtureDir::create("kvm-controls");
    let library = directory.path().join("libkvm.so");
    runtime_fixture::FakeRuntime {
        cfgs: vec![
            "fake_objects",
            "fake_receiver_controls",
            "fake_capture",
            "fake_advanced",
        ],
        ..Default::default()
    }
    .build(&library);
    TestChild::new("kvm_release_precedes_connection_changes_on_the_same_receiver")
        .env("GRAFTON_KVM_CHILD", &library)
        .env("GRAFTON_FAKE_NDI_LOG", directory.path().join("kvm.log"))
        .run();
}

#[cfg(feature = "advanced")]
fn exercise_kvm(library: std::path::PathBuf) {
    let ndi = Ndi::with_options(&RuntimeOptions::new().library_path(library)).unwrap();
    let first = Source::new("STUDIO (A)").unwrap();
    let next = Source::new("STUDIO (B)").unwrap();
    let mut receiver = Receiver::new(&ndi, &ReceiverOptions::new(first.clone())).unwrap();
    if !ndi.capabilities().supports(RuntimeFeature::Kvm) {
        assert!(matches!(
            receiver.kvm(),
            Err(Error::UnsupportedCapability { .. })
        ));
        return;
    }
    let mut session = receiver.kvm().unwrap();
    let before = read_fake_log();
    let view = session.receiver();
    let _ = view.ptz();
    assert_eq!(read_fake_log(), before);
    assert_eq!(view.connection_target().as_ref(), Some(&first));
    view.set_tally(grafton_ndi::Tally::new(true, false));
    session.key_down(0xffe1).unwrap();
    session.mouse_down(MouseButton::Right).unwrap();
    session.touch(&[[0.25, 0.5]]).unwrap();
    session.touch(&[]).unwrap();
    session.touch(&[[0.25, 0.5]]).unwrap();
    std::env::set_var(
        "GRAFTON_FAKE_CAPTURE_SCRIPT",
        "kvm:video,audio,metadata,status,video",
    );
    let video = view.capture_video_ref(Duration::ZERO).unwrap().unwrap();
    let audio = view.capture_audio_ref(Duration::ZERO).unwrap().unwrap();
    let metadata = view.capture_metadata_ref(Duration::ZERO).unwrap().unwrap();
    assert!(view.capture_any_ref(Duration::ZERO).unwrap().is_some());
    let mixed = view.capture_any_ref(Duration::ZERO).unwrap();
    assert_eq!(
        (video.width(), audio.num_channels(), metadata.data()),
        (4, 2, "<metadata/>")
    );
    session.move_pointer(0.5, 0.75).unwrap();
    std::env::set_var("GRAFTON_FAKE_CONTROL_RESULT", "refuse");
    assert!(matches!(
        session.release_all(),
        Err(Error::NativeOperationFailed { .. })
    ));
    std::env::remove_var("GRAFTON_FAKE_CONTROL_RESULT");
    session.release_all().unwrap();
    drop((video, audio, metadata, mixed));
    drop(session);
    receiver.connect_to(&next);
    let mut session = receiver.kvm().unwrap();
    assert_eq!(session.receiver().connection_target().as_ref(), Some(&next));
    session.key_down(0xff0d).unwrap();
    // Drop releases only the second session's key, before the disconnect.
    drop(session);
    receiver.disconnect();
    receiver.reconnect();
    drop(receiver);

    let log = read_fake_log();
    let lines: Vec<_> = log.lines().collect();
    let id = lines
        .iter()
        .find_map(|line| line.strip_prefix("key-down:"))
        .unwrap()
        .split(':')
        .next()
        .unwrap();
    let mut events = Vec::new();
    for line in &lines {
        let (operation, rest) = line.split_once(':').unwrap_or((line, ""));
        if matches!(
            operation,
            "key-down"
                | "key-up"
                | "right-down"
                | "right-up"
                | "touch"
                | "touch-up"
                | "position"
                | "tally"
                | "capture-instance"
                | "connect-instance"
        ) {
            assert_eq!(rest.split(':').next(), Some(id), "{log}");
        }
        if matches!(
            operation,
            "key-down"
                | "key-up"
                | "right-down"
                | "right-up"
                | "touch"
                | "touch-up"
                | "receiver-connect"
        ) {
            events.push(operation);
        }
    }
    assert_eq!(
        events,
        [
            "key-down",
            "right-down",
            "touch",
            "touch-up", // Empty updates use the documented lift.
            "touch",
            "key-up",
            "right-up",
            "touch-up", // Refused releases.
            "key-up",
            "right-up",
            "touch-up", // Successful retries.
            "receiver-connect",
            "key-down",
            "key-up", // New session and Drop.
            "receiver-connect",
            "receiver-connect",
        ],
        "{log}"
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.starts_with("touch:") && line.ends_with(":0")),
        "{log}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("capture:"))
            .count(),
        5,
        "{log}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("tally:"))
            .count(),
        1,
        "{log}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("free-video"))
            .count(),
        2,
        "{log}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("free-audio"))
            .count(),
        1,
        "{log}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("free-metadata"))
            .count(),
        1,
        "{log}"
    );
    assert!(log.contains("receiver-connect:STUDIO (B)"), "{log}");
    assert!(
        log.rfind("key-up:").unwrap() < log.find("receiver-connect:<disconnected>").unwrap(),
        "{log}"
    );
}

#[cfg(feature = "advanced")]
#[test]
fn controls_keep_the_same_receiver_and_synchronizer() {
    if let Some(library) = std::env::var_os("GRAFTON_CONTROLS_CHILD") {
        exercise(std::path::PathBuf::from(library));
        return;
    }
    let directory = common::FixtureDir::create("framesync-controls");
    for advanced in [false, true] {
        let library = directory.path().join(format!("libcontrols-{advanced}.so"));
        let mut fake = runtime_fixture::FakeRuntime {
            cfgs: vec!["fake_objects", "fake_receiver_controls"],
            ..Default::default()
        };
        if advanced {
            fake.cfgs.push("fake_advanced");
        }
        fake.build(&library);
        TestChild::new("controls_keep_the_same_receiver_and_synchronizer")
            .env("GRAFTON_CONTROLS_CHILD", &library)
            .env(
                "GRAFTON_FAKE_NDI_LOG",
                directory.path().join(format!("controls-{advanced}.log")),
            )
            .run();
    }
}

#[cfg(feature = "advanced")]
fn exercise(library: std::path::PathBuf) {
    let ndi = Ndi::with_options(&RuntimeOptions::new().library_path(library)).unwrap();
    let source = Source::new("STUDIO (Clocked)").unwrap();
    let receiver = Receiver::new(&ndi, &ReceiverOptions::new(source.clone())).unwrap();
    let mut sync = FrameSync::new(receiver).unwrap();
    assert_eq!(sync.receiver().connection_target().as_ref(), Some(&source));
    let before = sync
        .capture_video_ref(ScanType::Progressive)
        .unwrap()
        .unwrap();
    assert_eq!(before.picture_aspect_ratio().get(), 1.25);
    drop(before);

    if ndi
        .capabilities()
        .supports(RuntimeFeature::ReceiverStreamTier)
    {
        assert_eq!(
            sync.controls()
                .set_stream_tier(StreamTier::Preview)
                .unwrap(),
            SettingChange::Applied
        );
        assert_eq!(
            sync.controls().set_streams(MediaTypes::AUDIO).unwrap(),
            SettingChange::Applied
        );
        std::env::set_var("GRAFTON_FAKE_CONTROL_RESULT", "refuse");
        assert_eq!(
            sync.controls()
                .set_stream_tier(StreamTier::Program)
                .unwrap(),
            SettingChange::Unchanged
        );
        assert_eq!(
            sync.controls().set_streams(MediaTypes::ALL).unwrap(),
            SettingChange::Unchanged
        );
        std::env::remove_var("GRAFTON_FAKE_CONTROL_RESULT");
        let (capture, controls) = sync.split();
        let mut session = controls.kvm().unwrap();
        assert!(session.is_supported());
        session.mouse_down(MouseButton::Left).unwrap();
        session.key_down(0xffe1).unwrap();
        let video = capture
            .capture_video_ref(ScanType::Field0)
            .unwrap()
            .unwrap();
        let audio = capture
            .capture_audio_ref(FrameSyncAudioRequest::capture_at(48_000, 2, 8).unwrap())
            .unwrap();
        assert_eq!(video.scan_type(), ScanType::Field0);
        assert_eq!(video.metadata(), Some("<clock/>"));
        assert!(audio.channels().flatten().all(|sample| *sample == 0.25));
        assert_eq!(
            capture.audio_input_format().unwrap().unwrap().sample_rate(),
            48_000
        );
        assert_eq!(capture.audio_queue_depth().unwrap(), 42);
        assert!(matches!(
            session.key_down(u32::MAX),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            session.move_pointer(f32::NAN, 0.5),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            session.set_clipboard_text("bad\0text"),
            Err(Error::InvalidCString(_))
        ));
        session.move_pointer(0.5, 0.75).unwrap();
        std::env::set_var("GRAFTON_FAKE_CONTROL_RESULT", "refuse");
        assert!(matches!(
            session.release_all(),
            Err(Error::NativeOperationFailed { .. })
        ));
        std::env::remove_var("GRAFTON_FAKE_CONTROL_RESULT");
        // Drop retries the two inputs still held; this fixture never sends
        // local keyboard or mouse events.
        drop(session);
        drop((video, audio));
    } else {
        assert!(matches!(
            sync.controls().set_stream_tier(StreamTier::Preview),
            Err(Error::UnsupportedCapability { .. })
        ));
        assert!(matches!(
            sync.controls().set_streams(MediaTypes::ALL),
            Err(Error::UnsupportedCapability { .. })
        ));
        assert!(matches!(
            sync.controls().kvm(),
            Err(Error::UnsupportedCapability { .. })
        ));
    }
    let after = sync.capture_video(ScanType::Progressive).unwrap().unwrap();
    assert_eq!(after.timecode().ticks(), 1234);
    let receiver = sync.into_receiver();
    assert_eq!(receiver.connection_target().as_ref(), Some(&source));
    drop(receiver);

    let log = read_fake_log();
    let created: Vec<_> = log
        .lines()
        .filter(|line| line.starts_with("fs-create:"))
        .collect();
    assert_eq!(created.len(), 1, "{log}");
    let ids: Vec<_> = created[0].split(':').collect();
    let (receiver, synchronizer) = (ids[1], ids[2]);
    for line in log.lines() {
        if line.starts_with("tier:")
            || line.starts_with("streams:")
            || line.starts_with("key-")
            || line.starts_with("left-")
            || line.starts_with("position:")
        {
            assert_eq!(line.split(':').nth(1), Some(receiver), "{line}");
        }
        if line.starts_with("fs-video:") || line.starts_with("fs-audio:") {
            assert_eq!(line.split(':').nth(1), Some(receiver), "{line}");
            assert_eq!(line.split(':').nth(2), Some(synchronizer), "{line}");
        }
    }
    let captures = log
        .lines()
        .filter(|line| line.starts_with("fs-video:"))
        .count();
    assert_eq!(
        log.lines()
            .filter(|line| line.starts_with("fs-video-free:"))
            .count(),
        captures,
        "{log}"
    );
    let audio_captures = log
        .lines()
        .filter(|line| line.starts_with("fs-audio:"))
        .count();
    assert_eq!(
        log.lines()
            .filter(|line| line.starts_with("fs-audio-free:"))
            .count(),
        audio_captures,
        "{log}"
    );
    assert!(
        log.find("fs-destroy:").unwrap() < log.find("receiver-destroy").unwrap(),
        "{log}"
    );
    if captures == 3 {
        assert!(
            log.contains(&format!("streams:{receiver}:false:true:false")),
            "{log}"
        );
        assert_eq!(
            log.lines()
                .filter(|line| line.starts_with("key-up:"))
                .count(),
            2,
            "{log}"
        );
        assert_eq!(
            log.lines()
                .filter(|line| line.starts_with("left-up:"))
                .count(),
            2,
            "{log}"
        );
        assert_eq!(
            log.lines()
                .filter(|line| line.starts_with("position:"))
                .count(),
            1,
            "{log}"
        );
    }
}

#[path = "../src/test_alloc.rs"]
#[allow(dead_code)]
mod test_alloc;

fn unconnected(ndi: &Ndi) -> Receiver {
    Receiver::new(ndi, &ReceiverOptions::disconnected()).unwrap()
}

fn count(calls: &str, prefix: &str) -> usize {
    calls
        .lines()
        .filter(|line| line.starts_with(prefix))
        .count()
}

#[test]
fn framesync_recovery_and_audio_use_the_original_native_owners() {
    if let Some(library) = std::env::var_os("GRAFTON_FRAMESYNC_BOUNDARIES") {
        let ndi = Ndi::with_options(&RuntimeOptions::new().library_path(library)).unwrap();
        for recover in [false, true] {
            let begin = read_fake_log().len();
            let mut receiver = unconnected(&ndi);
            for destroyed in 1..=3 {
                receiver = FrameSync::new(receiver).unwrap().into_receiver();
                let log = read_fake_log();
                assert_eq!(count(&log[begin..], "fs-destroy:"), destroyed);
                assert_eq!(count(&log[begin..], "receiver-destroy"), 0);
                // Recovery restores raw capture, only after disposing of the
                // synchronizer. The fake records the receiver handle used.
                assert!(receiver.capture_video(Duration::ZERO).unwrap().is_none());
            }
            let sync = FrameSync::new(receiver).unwrap();
            let before = read_fake_log();
            let _ = format!("{sync:?}");
            #[cfg(feature = "advanced")]
            let sync = {
                let mut sync = sync;
                let (capture, _) = sync.split();
                let _ = format!("{capture:?}");
                sync
            };
            assert_eq!(read_fake_log(), before, "Debug queried the synchronizer");
            assert_eq!(sync.audio_queue_depth().unwrap(), 42);
            assert_eq!(
                count(&read_fake_log()[before.len()..], "fs-audio-queue:"),
                1
            );

            for (channels, samples) in [(2, 8_388_609), (2, 536_870_912), (1, 1_073_741_825)] {
                let before = read_fake_log();
                assert!(FrameSyncAudioRequest::capture_at(48_000, channels, samples)
                    .and_then(|request| sync.capture_audio_ref(request).map(drop))
                    .is_err());
                assert!(FrameSyncAudioRequest::capture_at(48_000, channels, samples)
                    .and_then(|request| sync.capture_audio(request))
                    .is_err());
                assert_eq!(
                    read_fake_log(),
                    before,
                    "an invalid request made a native call"
                );
            }
            for (rate, channels, samples) in [(48_000, 2, 1024), (44_100, 6, 480)] {
                let before = read_fake_log().len();
                let request = FrameSyncAudioRequest::capture_at(rate, channels, samples).unwrap();
                let (frame, allocations) =
                    test_alloc::allocation_count(|| sync.capture_audio_ref(request).unwrap());
                assert_eq!(allocations, 0);
                assert!(frame.channels().flatten().all(|&sample| sample == 0.25));
                assert_eq!(frame.channels().count(), channels as usize);
                assert_eq!(count(&read_fake_log()[before..], "fs-audio-free:"), 0);
                drop(frame);
                assert_eq!(count(&read_fake_log()[before..], "fs-audio-free:"), 1);
                let frame = sync.capture_audio(request).unwrap();
                assert_eq!(
                    (
                        frame.sample_rate(),
                        frame.num_channels(),
                        frame.num_samples()
                    ),
                    (rate, channels, samples)
                );
                let log = read_fake_log();
                assert_eq!(count(&log[before..], "fs-audio:"), 2);
                assert_eq!(count(&log[before..], "fs-audio-free:"), 2);
            }

            let before = read_fake_log().len();
            let (format, allocations) = test_alloc::allocation_count(|| sync.audio_input_format());
            assert_eq!(format.unwrap().unwrap().sample_rate(), 48_000);
            assert_eq!(allocations, 0);
            let log = read_fake_log();
            assert_eq!(count(&log[before..], "fs-audio:"), 1);
            assert_eq!(count(&log[before..], "fs-audio-free:"), 1);

            if recover {
                drop(sync.into_receiver());
            } else {
                drop(sync);
            }
            let log = read_fake_log();
            let log = &log[begin..];
            assert_eq!(count(log, "fs-create:"), 4);
            assert_eq!(count(log, "fs-destroy:"), 4);
            assert_eq!(count(log, "receiver-destroy"), 1);
            let receiver = log
                .lines()
                .find_map(|line| line.strip_prefix("fs-create:"))
                .unwrap()
                .split(':')
                .next()
                .unwrap();
            assert!(log
                .lines()
                .filter(|line| line.starts_with("fs-create:"))
                .all(|line| line.split(':').nth(1) == Some(receiver)));
            assert_eq!(
                log.lines()
                    .filter(|line| *line == format!("capture-instance:{receiver}"))
                    .count(),
                3
            );
            assert!(log.rfind("fs-destroy:").unwrap() < log.find("receiver-destroy").unwrap());
        }
        return;
    }
    let directory = common::FixtureDir::create("framesync-native-boundaries");
    let library = runtime_fixture::FakeRuntime {
        cfgs: vec!["fake_objects", "fake_receiver_controls", "fake_capture"],
        ..Default::default()
    }
    .build(&directory.join("libframesync.so"));
    TestChild::new("framesync_recovery_and_audio_use_the_original_native_owners")
        .env("GRAFTON_FRAMESYNC_BOUNDARIES", library)
        .env("GRAFTON_FAKE_NDI_LOG", directory.join("framesync.log"))
        .run();
}

#[test]
fn framesync_audio_requires_the_requested_shape() {
    if let Some(library) = std::env::var_os("GRAFTON_FRAMESYNC_AUDIO_CHILD") {
        let ndi = Ndi::with_options(&RuntimeOptions::new().library_path(library)).unwrap();
        let sync = FrameSync::new(unconnected(&ndi)).unwrap();
        #[cfg(feature = "advanced")]
        let mut sync = sync;
        let request = FrameSyncAudioRequest::capture_at(48_000, 2, 8).unwrap();
        let mismatch = FrameDefect::FrameSyncAudioMismatch;
        for (mode, defect) in [
            ("rate", mismatch.clone()),
            ("channels", mismatch.clone()),
            ("samples", mismatch),
            (
                "invalid-rate",
                FrameDefect::Layout(LayoutDefect::AudioRate((-1).into())),
            ),
        ] {
            std::env::set_var("GRAFTON_FAKE_FRAMESYNC_AUDIO", mode);
            let check = |capture: &dyn Fn() -> grafton_ndi::Result<()>| {
                let before = read_fake_log().len();
                let (result, allocations) = test_alloc::allocation_count(capture);
                let log = read_fake_log();
                assert_eq!(count(&log[before..], "fs-audio:"), 1, "{mode}");
                assert_eq!(count(&log[before..], "fs-audio-free:"), 1, "{mode}");
                assert!(
                    matches!(&result, Err(Error::InvalidFrame(actual)) if actual == &defect),
                    "{mode}: {result:?}"
                );
                assert_eq!(allocations, 0, "{mode}");
            };
            check(&|| sync.capture_audio_ref(request).map(drop));
            check(&|| sync.capture_audio(request).map(drop));
            #[cfg(feature = "advanced")]
            {
                let (capture, _) = sync.split();
                check(&|| capture.capture_audio_ref(request).map(drop));
                check(&|| capture.capture_audio(request).map(drop));
            }
        }

        std::env::set_var("GRAFTON_FAKE_FRAMESYNC_AUDIO", "padded");
        let before = read_fake_log().len();
        let (frame, allocations) = test_alloc::allocation_count(|| sync.capture_audio_ref(request));
        let frame = frame.unwrap();
        assert_eq!(allocations, 0);
        assert_eq!(frame.channel_stride_bytes(), 12 * size_of::<f32>());
        assert!(frame.data().is_none());
        assert_eq!(frame.channels().count(), 2);
        assert!(frame.channels().all(|channel| channel == [0.25; 8]));
        assert_eq!(count(&read_fake_log()[before..], "fs-audio-free:"), 0);
        drop(frame);
        assert_eq!(count(&read_fake_log()[before..], "fs-audio-free:"), 1);
        return;
    }

    let directory = common::FixtureDir::create("framesync-audio-shape");
    let library = runtime_fixture::FakeRuntime {
        cfgs: vec!["fake_objects", "fake_receiver_controls", "fake_capture"],
        ..Default::default()
    }
    .build(&directory.join("libframesync.so"));
    TestChild::new("framesync_audio_requires_the_requested_shape")
        .env("GRAFTON_FRAMESYNC_AUDIO_CHILD", library)
        .env("GRAFTON_FAKE_NDI_LOG", directory.join("framesync.log"))
        .run();
}

#[cfg(all(feature = "advanced", ndi_has_advanced_av_sync))]
#[test]
fn avsync_destroys_before_raw_capture_or_receiver_drop() {
    use grafton_ndi::advanced::AvSync;
    if let Some(library) = std::env::var_os("GRAFTON_AVSYNC_BOUNDARIES") {
        let ndi = Ndi::with_options(&RuntimeOptions::new().library_path(library)).unwrap();
        for recover in [false, true] {
            let begin = read_fake_log().len();
            let sync = AvSync::new(unconnected(&ndi)).unwrap();
            if recover {
                let receiver = sync.into_receiver();
                let log = read_fake_log();
                assert_eq!(count(&log[begin..], "avsync-destroy"), 1);
                assert_eq!(count(&log[begin..], "receiver-destroy"), 0);
                assert!(receiver.capture_audio(Duration::ZERO).unwrap().is_none());
            } else {
                drop(sync);
            }
            let log = read_fake_log();
            let log = &log[begin..];
            assert_eq!(count(log, "avsync-destroy"), 1);
            assert_eq!(count(log, "receiver-destroy"), 1);
            assert!(log.find("avsync-destroy").unwrap() < log.find("receiver-destroy").unwrap());
        }
        return;
    }
    let directory = common::FixtureDir::create("avsync-native-boundaries");
    let library = runtime_fixture::FakeRuntime {
        cfgs: vec![
            "fake_objects",
            "fake_receiver_controls",
            "fake_capture",
            "fake_advanced",
        ],
        version: Some("6.4.0.2"),
        ..Default::default()
    }
    .build(&directory.join("libavsync.so"));
    TestChild::new("avsync_destroys_before_raw_capture_or_receiver_drop")
        .env("GRAFTON_AVSYNC_BOUNDARIES", library)
        .env("GRAFTON_FAKE_NDI_LOG", directory.join("avsync.log"))
        .run();
}
