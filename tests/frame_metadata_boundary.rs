//! Malformed side text must not discard otherwise usable native media.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

#[path = "support/fixture.rs"]
mod fixture;
#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;

use grafton_ndi::{
    AudioSource, FrameMetadataStatus, Ndi, Receiver, ReceiverItem, ReceiverItemRef,
    ReceiverOptions, RuntimeOptions, Source, VideoSource,
};
use std::{process::Command, time::Duration};

#[test]
fn malformed_per_frame_metadata_preserves_typed_mixed_and_async_media() {
    if let Some(library) = std::env::var_os("GRAFTON_METADATA_BOUNDARY_CHILD") {
        let ndi = Ndi::with_options(&RuntimeOptions::new().library_path(library)).unwrap();
        let receiver = Receiver::new(&ndi, &ReceiverOptions::new(Source::new("fixture"))).unwrap();
        synchronous(&receiver);
        #[cfg(feature = "tokio")]
        ::tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(asynchronous(
                grafton_ndi::asynchronous::tokio::AsyncReceiver::new(
                    Receiver::new(&ndi, &ReceiverOptions::new(Source::new("tokio fixture")))
                        .unwrap(),
                ),
                100,
            ));
        #[cfg(feature = "smol")]
        ::smol::block_on(asynchronous(
            grafton_ndi::asynchronous::smol::AsyncReceiver::new(receiver),
            200,
        ));
        return;
    }
    let directory = fixture::FixtureDir::create("frame-metadata");
    let library = directory.path().join("libndi_capture.so");
    runtime_fixture::FakeRuntime {
        cfgs: vec!["fake_objects", "fake_capture"],
        ..Default::default()
    }
    .build(&library);
    let result = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "malformed_per_frame_metadata_preserves_typed_mixed_and_async_media",
            "--nocapture",
        ])
        .env("GRAFTON_METADATA_BOUNDARY_CHILD", library)
        .env("GRAFTON_FAKE_NDI_LOG", directory.path().join("capture.log"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

fn script(serial: usize, kind: &str) {
    std::env::set_var("GRAFTON_FAKE_CAPTURE_SCRIPT", format!("{serial}:{kind}"));
}

fn cases() -> [(&'static str, FrameMetadataStatus); 4] {
    [
        ("video-invalid-metadata", FrameMetadataStatus::NotUtf8),
        ("audio-invalid-metadata", FrameMetadataStatus::NotUtf8),
        (
            "video-unterminated-metadata",
            FrameMetadataStatus::Unterminated,
        ),
        (
            "audio-unterminated-metadata",
            FrameMetadataStatus::Unterminated,
        ),
    ]
}

fn video(frame: &impl VideoSource, status: FrameMetadataStatus) {
    assert_eq!(frame.metadata(), None);
    assert_eq!(frame.metadata_status(), status);
    assert_eq!(frame.width(), 4);
    assert_eq!(frame.height(), 2);
    assert!(frame
        .planes()
        .flat_map(|plane| plane.rows())
        .flatten()
        .all(|&byte| byte == 37));
}

fn audio(frame: &impl AudioSource, status: FrameMetadataStatus) {
    assert_eq!(frame.metadata(), None);
    assert_eq!(frame.metadata_status(), status);
    assert_eq!(frame.sample_rate(), 48_000);
    assert_eq!(frame.channel_data(0), Some([0.25, 0.5].as_slice()));
    assert_eq!(frame.channel_data(1), Some([-0.25, -0.5].as_slice()));
}

fn synchronous(receiver: &Receiver) {
    for (index, (kind, status)) in cases().into_iter().enumerate() {
        script(index * 4, kind);
        if kind.starts_with("video") {
            let frame = receiver
                .video()
                .capture_ref(Duration::ZERO)
                .unwrap()
                .unwrap();
            video(&frame, status);
            video(&frame.to_owned(), FrameMetadataStatus::Absent);
        } else {
            let frame = receiver
                .audio()
                .capture_ref(Duration::ZERO)
                .unwrap()
                .unwrap();
            audio(&frame, status);
            audio(&frame.to_owned(), FrameMetadataStatus::Absent);
        }
        script(index * 4 + 1, kind);
        match receiver.capture_any_ref(Duration::ZERO).unwrap().unwrap() {
            ReceiverItemRef::Video(frame) => video(&frame, status),
            ReceiverItemRef::Audio(frame) => audio(&frame, status),
            other => panic!("unexpected {other:?}"),
        }
        script(index * 4 + 2, kind);
        if kind.starts_with("video") {
            video(
                &receiver.video().capture(Duration::ZERO).unwrap().unwrap(),
                FrameMetadataStatus::Absent,
            );
        } else {
            audio(
                &receiver.audio().capture(Duration::ZERO).unwrap().unwrap(),
                FrameMetadataStatus::Absent,
            );
        }
        script(index * 4 + 3, kind);
        match receiver.capture_any(Duration::ZERO).unwrap().unwrap() {
            ReceiverItem::Video(frame) => video(&frame, FrameMetadataStatus::Absent),
            ReceiverItem::Audio(frame) => audio(&frame, FrameMetadataStatus::Absent),
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[cfg(any(feature = "tokio", feature = "smol"))]
async fn asynchronous<R: grafton_ndi::asynchronous::AsyncRuntime + 'static>(
    receiver: grafton_ndi::asynchronous::AsyncReceiver<R>,
    serial: usize,
) {
    for (index, (kind, status)) in cases().into_iter().enumerate() {
        script(serial + index * 4, kind);
        if kind.starts_with("video") {
            assert_eq!(
                receiver
                    .video()
                    .capture_with(Duration::ZERO, move |frame| video(frame, status))
                    .await
                    .unwrap(),
                Some(())
            );
        } else {
            assert_eq!(
                receiver
                    .audio()
                    .capture_with(Duration::ZERO, move |frame| audio(frame, status))
                    .await
                    .unwrap(),
                Some(())
            );
        }
        script(serial + index * 4 + 1, kind);
        assert_eq!(
            receiver
                .capture_any_with(Duration::ZERO, move |item| match item {
                    ReceiverItemRef::Video(frame) => video(frame, status),
                    ReceiverItemRef::Audio(frame) => audio(frame, status),
                    other => panic!("unexpected {other:?}"),
                })
                .await
                .unwrap(),
            Some(())
        );
        script(serial + index * 4 + 2, kind);
        if kind.starts_with("video") {
            video(
                &receiver
                    .video()
                    .capture(Duration::ZERO)
                    .await
                    .unwrap()
                    .unwrap(),
                FrameMetadataStatus::Absent,
            );
        } else {
            audio(
                &receiver
                    .audio()
                    .capture(Duration::ZERO)
                    .await
                    .unwrap()
                    .unwrap(),
                FrameMetadataStatus::Absent,
            );
        }
        script(serial + index * 4 + 3, kind);
        match receiver.capture_any(Duration::ZERO).await.unwrap().unwrap() {
            ReceiverItem::Video(frame) => video(&frame, FrameMetadataStatus::Absent),
            ReceiverItem::Audio(frame) => audio(&frame, FrameMetadataStatus::Absent),
            other => panic!("unexpected {other:?}"),
        }
    }
}
