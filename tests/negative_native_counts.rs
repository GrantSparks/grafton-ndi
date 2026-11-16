//! Native count breaches retain evidence and pending status invalidations.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

#[path = "support/fixture.rs"]
mod fixture;
#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;

use grafton_ndi::{
    Error, FrameSync, Ndi, Receiver, ReceiverEvent, ReceiverItem, ReceiverOptions, RuntimeOptions,
    SdkResultDefect, Source,
};
use std::{process::Command, time::Duration};

fn refused<T: std::fmt::Debug>(result: Result<T, Error>, function: &str, expected: i64) {
    assert!(
        matches!(result, Err(Error::UnexpectedSdkResult(SdkResultDefect::NegativeCount { operation, value }))
        if operation.name().to_str().unwrap() == function && value.get() == expected),
        "{result:?}"
    );
}

#[test]
fn negative_counts_are_errors_and_status_survives() {
    if let Some(path) = std::env::var_os("GRAFTON_NEGATIVE_COUNTS_CHILD") {
        let ndi = Ndi::with_options(&RuntimeOptions::new().library_path(path)).unwrap();
        let receiver =
            Receiver::new(&ndi, &ReceiverOptions::new(Source::new("").unwrap())).unwrap();
        for selected in [
            "connections",
            "received-video",
            "received-audio",
            "received-metadata",
            "dropped-video",
            "dropped-audio",
            "dropped-metadata",
            "queued-video",
            "queued-audio",
            "queued-metadata",
        ] {
            std::env::set_var("GRAFTON_FAKE_NEGATIVE_COUNT", selected);
            let function = if selected == "connections" {
                "NDIlib_recv_get_no_connections"
            } else if selected.starts_with("queued-") {
                "NDIlib_recv_get_queue"
            } else {
                "NDIlib_recv_get_performance"
            };
            refused(
                receiver.connection_stats(),
                function,
                if selected.starts_with("received-") || selected.starts_with("dropped-") {
                    i64::MIN
                } else {
                    -1
                },
            );
        }
        std::env::set_var("GRAFTON_FAKE_NEGATIVE_COUNT", "connections");
        assert!(!receiver.is_connected());
        std::env::set_var("GRAFTON_FAKE_CAPTURE_SCRIPT", "negative-count:status");
        assert!(receiver.capture_video(Duration::ZERO).unwrap().is_none());
        refused(
            receiver.next_event(Duration::ZERO),
            "NDIlib_recv_get_no_connections",
            -1,
        );
        std::env::remove_var("GRAFTON_FAKE_NEGATIVE_COUNT");
        assert!(
            matches!(receiver.next_event(Duration::ZERO).unwrap(), Some(ReceiverEvent::StatusChanged(status)) if status.connections() == 0)
        );
        assert!(receiver.next_event(Duration::ZERO).unwrap().is_none());
        assert_eq!(receiver.connection_stats().unwrap().connections(), 0);

        std::env::set_var("GRAFTON_FAKE_NEGATIVE_COUNT", "connections");
        std::env::set_var("GRAFTON_FAKE_CAPTURE_SCRIPT", "mixed-failure:status,video");
        refused(
            receiver.capture_any(Duration::ZERO),
            "NDIlib_recv_get_no_connections",
            -1,
        );
        let captures = || {
            std::fs::read_to_string(std::env::var_os("GRAFTON_FAKE_NDI_LOG").unwrap())
                .unwrap()
                .lines()
                .filter(|line| line.starts_with("capture:"))
                .count()
        };
        let before = captures();
        assert!(matches!(
            receiver.capture_any(Duration::ZERO).unwrap(),
            Some(ReceiverItem::Video(_))
        ));
        assert_eq!(
            captures(),
            before + 1,
            "a failed status must permit native media capture"
        );
        refused(
            receiver.connection_stats(),
            "NDIlib_recv_get_no_connections",
            -1,
        );
        std::env::set_var(
            "GRAFTON_FAKE_CAPTURE_SCRIPT",
            "other-events:source,unknown,status",
        );
        for _ in 0..3 {
            assert!(receiver.capture_video(Duration::ZERO).unwrap().is_none());
        }
        assert!(matches!(
            receiver.next_event(Duration::ZERO).unwrap(),
            Some(ReceiverEvent::SourceChanged)
        ));
        assert!(matches!(
            receiver.next_event(Duration::ZERO).unwrap(),
            Some(ReceiverEvent::Unknown(987))
        ));
        refused(
            receiver.next_event(Duration::ZERO),
            "NDIlib_recv_get_no_connections",
            -1,
        );
        assert!(receiver.next_event(Duration::ZERO).unwrap().is_none());
        std::env::remove_var("GRAFTON_FAKE_NEGATIVE_COUNT");
        assert!(matches!(
            receiver.next_event(Duration::ZERO).unwrap(),
            Some(ReceiverEvent::StatusChanged(_))
        ));
        assert!(receiver.next_event(Duration::ZERO).unwrap().is_none());
        let framesync = FrameSync::new(receiver).unwrap();
        std::env::set_var("GRAFTON_FAKE_NEGATIVE_COUNT", "queue-depth");
        refused(
            framesync.audio_queue_depth(),
            "NDIlib_framesync_audio_queue_depth",
            -1,
        );
        std::env::remove_var("GRAFTON_FAKE_NEGATIVE_COUNT");
        assert_eq!(framesync.audio_queue_depth().unwrap(), 42);
        return;
    }
    let directory = fixture::FixtureDir::create("negative-native-counts");
    let library = runtime_fixture::FakeRuntime {
        cfgs: vec!["fake_objects", "fake_capture", "fake_receiver_controls"],
        ..Default::default()
    }
    .build(&directory.path().join("libndi.so"));
    let result = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "negative_counts_are_errors_and_status_survives",
            "--nocapture",
        ])
        .env("GRAFTON_NEGATIVE_COUNTS_CHILD", library)
        .env("GRAFTON_FAKE_NDI_LOG", directory.path().join("counts.log"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
