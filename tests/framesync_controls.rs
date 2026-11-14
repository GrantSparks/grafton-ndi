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
use std::{fs, process::Command};

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
