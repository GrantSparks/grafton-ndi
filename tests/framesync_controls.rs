//! Receiver control forwarding and held-input clocked capture without an SDK.
#![cfg(all(feature = "advanced", target_os = "linux", not(ndi_link_static)))]

#[path = "support/fixture.rs"]
mod fixture;
#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;

use grafton_ndi::{
    advanced::MouseButton, Error, FrameSync, FrameSyncAudioRequest, MediaTypes, Ndi, Receiver,
    ReceiverOptions, RuntimeFeature, RuntimeOptions, ScanType, SettingChange, Source, StreamTier,
};
use std::{fs, process::Command, time::Duration};

#[test]
fn kvm_release_precedes_connection_changes_on_the_same_receiver() {
    if let Some(library) = std::env::var_os("GRAFTON_KVM_CHILD") {
        exercise_kvm(library.into());
        return;
    }
    let directory = fixture::FixtureDir::create("kvm-controls");
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
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "kvm_release_precedes_connection_changes_on_the_same_receiver",
            "--nocapture",
        ])
        .env("GRAFTON_KVM_CHILD", &library)
        .env("GRAFTON_FAKE_NDI_LOG", directory.path().join("kvm.log"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn exercise_kvm(library: std::path::PathBuf) {
    let ndi = Ndi::with_options(&RuntimeOptions::new().library_path(library)).unwrap();
    let first = Source::new("STUDIO (A)");
    let next = Source::new("STUDIO (B)");
    let mut receiver = Receiver::new(&ndi, &ReceiverOptions::new(first.clone())).unwrap();
    if !ndi.capabilities().supports(RuntimeFeature::Kvm) {
        assert!(matches!(
            receiver.kvm(),
            Err(Error::UnsupportedCapability { .. })
        ));
        return;
    }
    let log_path = std::env::var_os("GRAFTON_FAKE_NDI_LOG").unwrap();
    let mut session = receiver.kvm().unwrap();
    let before = fs::read_to_string(&log_path).unwrap();
    let view = session.receiver();
    let _ = (view.video(), view.audio(), view.metadata(), view.ptz());
    assert_eq!(fs::read_to_string(&log_path).unwrap(), before);
    assert_eq!(view.connection_target(), first);
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
    let video = view.video().capture_ref(Duration::ZERO).unwrap().unwrap();
    let audio = view.audio().capture_ref(Duration::ZERO).unwrap().unwrap();
    let metadata = view
        .metadata()
        .capture_ref(Duration::ZERO)
        .unwrap()
        .unwrap();
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
    receiver.connect_to(&next).unwrap();
    let mut session = receiver.kvm().unwrap();
    assert_eq!(session.receiver().connection_target(), next);
    session.key_down(0xff0d).unwrap();
    // Drop releases only the second session's key, before the disconnect.
    drop(session);
    receiver.disconnect();
    receiver.reconnect();
    drop(receiver);

    let log = fs::read_to_string(&log_path).unwrap();
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

#[test]
fn controls_keep_the_same_receiver_and_synchronizer() {
    if let Some(library) = std::env::var_os("GRAFTON_CONTROLS_CHILD") {
        exercise(std::path::PathBuf::from(library));
        return;
    }
    let directory = fixture::FixtureDir::create("framesync-controls");
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
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "controls_keep_the_same_receiver_and_synchronizer",
                "--nocapture",
            ])
            .env("GRAFTON_CONTROLS_CHILD", &library)
            .env(
                "GRAFTON_FAKE_NDI_LOG",
                directory.path().join(format!("controls-{advanced}.log")),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn exercise(library: std::path::PathBuf) {
    let ndi = Ndi::with_options(&RuntimeOptions::new().library_path(library)).unwrap();
    let source = Source::new("STUDIO (Clocked)");
    let receiver = Receiver::new(&ndi, &ReceiverOptions::new(source.clone())).unwrap();
    let mut sync = FrameSync::new(receiver).unwrap();
    assert_eq!(sync.receiver().connection_target(), source);
    let before = sync
        .capture_video_ref(ScanType::Progressive)
        .unwrap()
        .unwrap();
    assert_eq!(before.picture_aspect_ratio(), 1.25);
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
        assert_eq!(capture.audio_queue_depth(), 42);
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
    assert_eq!(receiver.connection_target(), source);
    drop(receiver);

    let log = fs::read_to_string(std::env::var_os("GRAFTON_FAKE_NDI_LOG").unwrap()).unwrap();
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
