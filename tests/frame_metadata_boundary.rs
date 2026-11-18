//! Malformed side text must not discard otherwise usable native media.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(child);
use common::TestChild;

#[path = "support/fixture.rs"]
mod fixture;
#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;

use grafton_ndi::{
    AudioSource, FrameMetadataStatus, Ndi, Receiver, ReceiverItem, ReceiverItemRef,
    ReceiverOptions, RuntimeOptions, Source, VideoSource,
};
use std::time::Duration;

#[test]
fn malformed_per_frame_metadata_preserves_typed_mixed_and_async_media() {
    if let Some(library) = std::env::var_os("GRAFTON_METADATA_BOUNDARY_CHILD") {
        let ndi = Ndi::with_options(&RuntimeOptions::new().library_path(library)).unwrap();
        let receiver =
            Receiver::new(&ndi, &ReceiverOptions::new(Source::new("fixture").unwrap())).unwrap();
        synchronous(&receiver);
        #[cfg(feature = "tokio")]
        ::tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(asynchronous(
                grafton_ndi::asynchronous::tokio::AsyncReceiver::new(
                    Receiver::new(
                        &ndi,
                        &ReceiverOptions::new(Source::new("tokio fixture").unwrap()),
                    )
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
    TestChild::new("malformed_per_frame_metadata_preserves_typed_mixed_and_async_media")
        .env("GRAFTON_METADATA_BOUNDARY_CHILD", library)
        .env("GRAFTON_FAKE_NDI_LOG", directory.path().join("capture.log"))
        .run();
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
    assert!(matches!(
        frame.color_info(),
        Err(grafton_ndi::Error::InvalidFrame(_))
    ));
    let mut owned = VideoSource::to_owned(frame);
    assert_eq!(owned.metadata_status(), status);
    assert_eq!(owned.clone().metadata_status(), status);
    for _ in 0..3 {
        frame.copy_into(&mut owned);
        assert_eq!(owned.metadata_status(), status);
    }
    #[cfg(feature = "image-encoding")]
    {
        use grafton_ndi::{
            image_encoding::{ColorPolicy, ImageFormat, ImageOptions, ImageWorkspace, YuvRange},
            YuvMatrix,
        };
        let defaults = ImageOptions::new(ImageFormat::Png);
        let mut workspace = ImageWorkspace::new();
        let mut output = Vec::new();
        assert!(matches!(
            workspace.encode(frame, &defaults),
            Err(grafton_ndi::Error::InvalidFrame(_))
        ));
        assert!(matches!(
            workspace.encode_to(frame, &defaults, &mut output),
            Err(grafton_ndi::Error::InvalidFrame(_))
        ));
        assert!(output.is_empty());
        assert!(matches!(
            workspace.encode_data_url(frame, &defaults),
            Err(grafton_ndi::Error::InvalidFrame(_))
        ));
        assert!(matches!(
            owned.encode_image(&defaults),
            Err(grafton_ndi::Error::InvalidFrame(_))
        ));
        assert!(matches!(
            owned.encode_image_data_url(&defaults),
            Err(grafton_ndi::Error::InvalidFrame(_))
        ));
        let options = defaults.color(ColorPolicy::Override {
            matrix: YuvMatrix::Bt709,
            range: YuvRange::Limited,
        });
        assert_eq!(
            workspace.encode(frame, &options).unwrap(),
            owned.encode_image(&options).unwrap()
        );
    }
}

fn audio(frame: &impl AudioSource, status: FrameMetadataStatus) {
    assert_eq!(frame.metadata(), None);
    assert_eq!(frame.metadata_status(), status);
    assert_eq!(frame.sample_rate(), 48_000);
    assert_eq!(frame.channel_data(0), Some([0.25, 0.5].as_slice()));
    assert_eq!(frame.channel_data(1), Some([-0.25, -0.5].as_slice()));
    let mut owned = AudioSource::to_owned(frame);
    assert_eq!(owned.metadata_status(), status);
    assert_eq!(owned.clone().metadata_status(), status);
    for _ in 0..3 {
        frame.copy_into(&mut owned);
        assert_eq!(owned.metadata_status(), status);
    }
}

fn synchronous(receiver: &Receiver) {
    for (index, (kind, status)) in cases().into_iter().enumerate() {
        script(index * 4, kind);
        if kind.starts_with("video") {
            let frame = receiver.capture_video_ref(Duration::ZERO).unwrap().unwrap();
            video(&frame, status);
            video(&frame.to_owned(), status);
        } else {
            let frame = receiver.capture_audio_ref(Duration::ZERO).unwrap().unwrap();
            audio(&frame, status);
            audio(&frame.to_owned(), status);
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
                &receiver.capture_video(Duration::ZERO).unwrap().unwrap(),
                status,
            );
        } else {
            audio(
                &receiver.capture_audio(Duration::ZERO).unwrap().unwrap(),
                status,
            );
        }
        script(index * 4 + 3, kind);
        match receiver.capture_any(Duration::ZERO).unwrap().unwrap() {
            ReceiverItem::Video(frame) => video(&frame, status),
            ReceiverItem::Audio(frame) => audio(&frame, status),
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
                    .capture_video_with(Duration::ZERO, move |frame| video(frame, status))
                    .await
                    .unwrap(),
                Some(())
            );
        } else {
            assert_eq!(
                receiver
                    .capture_audio_with(Duration::ZERO, move |frame| audio(frame, status))
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
                    .capture_video(Duration::ZERO)
                    .await
                    .unwrap()
                    .unwrap(),
                status,
            );
        } else {
            audio(
                &receiver
                    .capture_audio(Duration::ZERO)
                    .await
                    .unwrap()
                    .unwrap(),
                status,
            );
        }
        script(serial + index * 4 + 3, kind);
        match receiver.capture_any(Duration::ZERO).await.unwrap().unwrap() {
            ReceiverItem::Video(frame) => video(&frame, status),
            ReceiverItem::Audio(frame) => audio(&frame, status),
            other => panic!("unexpected {other:?}"),
        }
    }
}
