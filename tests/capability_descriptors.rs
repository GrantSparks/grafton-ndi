//! Version-gated enum values reach native descriptors only after authorization.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

#[cfg(feature = "advanced")]
#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
#[cfg(feature = "advanced")]
common_helpers!(policy, prerequisites);
#[path = "support/fixture.rs"]
mod fixture;
#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;

use grafton_ndi::{
    capabilities::{CapabilityUnavailable, RuntimeFeature},
    Error, Ndi, PixelFormat, Receiver, ReceiverBandwidth, ReceiverColorFormat, ReceiverOptions,
    RuntimeOptions, Sender, SenderOptions, Source, VideoFrame,
};
#[cfg(feature = "advanced")]
use std::time::Duration;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const PIXELS: [PixelFormat; 15] = [
    PixelFormat::Uyvy,
    PixelFormat::Uyva,
    PixelFormat::P216,
    PixelFormat::Pa16,
    PixelFormat::Yv12,
    PixelFormat::I420,
    PixelFormat::Nv12,
    PixelFormat::Bgra,
    PixelFormat::Bgrx,
    PixelFormat::Rgba,
    PixelFormat::Rgbx,
    PixelFormat::Na48,
    PixelFormat::Nb48,
    PixelFormat::Na4g,
    PixelFormat::Nb4g,
];

fn gated(format: PixelFormat) -> bool {
    matches!(
        format,
        PixelFormat::Na48 | PixelFormat::Nb48 | PixelFormat::Na4g | PixelFormat::Nb4g
    )
}

fn frame(format: PixelFormat) -> VideoFrame {
    VideoFrame::builder()
        .resolution(4, 2)
        .pixel_format(format)
        .metadata("<gate/>")
        .build()
        .unwrap()
}

fn events() -> String {
    fs::read_to_string(std::env::var_os("GRAFTON_FAKE_NDI_LOG").unwrap()).unwrap_or_default()
}

fn calls(prefix: &str) -> usize {
    events()
        .lines()
        .filter(|event| *event == prefix || (prefix.ends_with(':') && event.starts_with(prefix)))
        .count()
}

fn refused(error: &Error, expected: RuntimeFeature) {
    assert!(
        matches!(error, Error::UnsupportedCapability {
        feature, reason: CapabilityUnavailable::RuntimeTooOld, ..
    } if *feature == expected),
        "wrong gate refusal: {error}"
    );
}

fn accepted<T>(result: grafton_ndi::Result<T>, supported: bool, feature: RuntimeFeature) {
    match result {
        Ok(_) => assert!(supported, "{feature:?} reached the SDK before its gate"),
        Err(error) => {
            assert!(!supported, "supported {feature:?} was refused: {error}");
            refused(&error, feature);
        }
    }
}

#[test]
fn caller_descriptors_apply_version_gates_before_native_operations() {
    if let Some(library) = std::env::var_os("GRAFTON_DESCRIPTOR_LIBRARY") {
        let ndi =
            Ndi::with_options(&RuntimeOptions::new().library_path(PathBuf::from(library))).unwrap();
        let modern = std::env::var("GRAFTON_DESCRIPTOR_MODERN").unwrap() == "1";
        sending(&ndi, modern);
        receiving(&ndi, modern);
        #[cfg(feature = "advanced")]
        advanced(&ndi, modern);
        return;
    }
    let directory = fixture::FixtureDir::create("capability-descriptors");
    for (version, modern) in [("6.3.2.0", false), ("6.4.0.2", true), ("7.0.0.0", true)] {
        let path = directory.path().join(version);
        fs::create_dir(&path).unwrap();
        let library = path.join("libndi_advanced.so");
        runtime_fixture::FakeRuntime {
            cfgs: vec![
                "fake_objects",
                "fake_advanced",
                "fake_capture",
                "fake_capability_descriptors",
            ],
            version: Some(version),
            ..Default::default()
        }
        .build(&library);
        run_child(&library, modern, &path);
    }
}

fn run_child(library: &Path, modern: bool, directory: &Path) {
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "caller_descriptors_apply_version_gates_before_native_operations",
            "--nocapture",
        ])
        .env("GRAFTON_DESCRIPTOR_LIBRARY", library)
        .env("GRAFTON_DESCRIPTOR_MODERN", if modern { "1" } else { "0" })
        .env("GRAFTON_FAKE_NDI_LOG", directory.join("descriptors.log"))
        .env("GRAFTON_FAKE_CALLBACK_MODE", "immediate")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}: {}\n{}",
        directory.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.stderr.is_empty() {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
    }
}

fn sending(ndi: &Ndi, modern: bool) {
    let mut sender = Sender::new(ndi, &SenderOptions::new("descriptor gates")).unwrap();
    let earlier = frame(PixelFormat::Bgra);
    assert!(sender.send_video_async(earlier).unwrap().is_none());
    for format in PIXELS {
        let supported = modern || !gated(format);
        let input = frame(format);
        let before = calls("video-send:");
        accepted(
            sender.send_video(&input),
            supported,
            RuntimeFeature::Planar444Video,
        );
        assert_eq!(calls("video-send:"), before + usize::from(supported));
        let pixels = input.data().as_ptr();
        let before = calls("sender-submit");
        match sender.send_video_async(input) {
            Ok(_) => assert!(supported),
            Err(rejected) => {
                assert!(!supported);
                refused(rejected.error(), RuntimeFeature::Planar444Video);
                let recovered = rejected.into_inner();
                assert_eq!(recovered.data().as_ptr(), pixels);
                assert_eq!(recovered.metadata(), Some("<gate/>"));
            }
        }
        assert_eq!(calls("sender-submit"), before + usize::from(supported));
        let input = frame(format);
        let before = calls("sender-submit");
        accepted(
            sender.send_video_async_copy(&input),
            supported,
            RuntimeFeature::Planar444Video,
        );
        assert_eq!(calls("sender-submit"), before + usize::from(supported));
    }
    assert!(sender.finish_video_async().is_some());
    // A refusal must also leave a prior owned submission untouched.
    if !modern {
        let mut sender = Sender::new(ndi, &SenderOptions::new("pending descriptor gate")).unwrap();
        let earlier = frame(PixelFormat::Bgra);
        let pointer = earlier.data().as_ptr();
        sender.send_video_async(earlier).unwrap();
        for format in [
            PixelFormat::Na48,
            PixelFormat::Nb48,
            PixelFormat::Na4g,
            PixelFormat::Nb4g,
        ] {
            let input = frame(format);
            assert!(sender.send_video(&input).is_err());
            assert!(sender.send_video_async_copy(&input).is_err());
            assert!(sender.send_video_async(input).is_err());
        }
        assert_eq!(
            sender.finish_video_async().unwrap().data().as_ptr(),
            pointer
        );
    }
}

fn receiving(ndi: &Ndi, modern: bool) {
    for color in [
        ReceiverColorFormat::BgrxBgra,
        ReceiverColorFormat::UyvyBgra,
        ReceiverColorFormat::RgbxRgba,
        ReceiverColorFormat::UyvyRgba,
        ReceiverColorFormat::Fastest,
        ReceiverColorFormat::Best,
        ReceiverColorFormat::Best444,
    ] {
        for bandwidth in [
            ReceiverBandwidth::MetadataOnly,
            ReceiverBandwidth::AudioOnly,
            ReceiverBandwidth::Lowest,
            ReceiverBandwidth::Highest,
            ReceiverBandwidth::VideoOnlyLowest,
            ReceiverBandwidth::VideoOnlyHighest,
        ] {
            let feature = if color == ReceiverColorFormat::Best444 {
                Some(RuntimeFeature::ReceiveBest444)
            } else if matches!(
                bandwidth,
                ReceiverBandwidth::VideoOnlyLowest | ReceiverBandwidth::VideoOnlyHighest
            ) {
                Some(RuntimeFeature::ReceiveVideoOnly)
            } else {
                None
            };
            let options = ReceiverOptions::new(Source::new("fixture"))
                .color_format(color)
                .bandwidth(bandwidth);
            let before = calls("receiver-create");
            match Receiver::new(ndi, &options) {
                Ok(_) => assert!(modern || feature.is_none()),
                Err(error) => {
                    assert!(!modern);
                    refused(&error, feature.unwrap());
                }
            }
            assert_eq!(
                calls("receiver-create"),
                before + usize::from(modern || feature.is_none())
            );
        }
    }
    let malformed = ReceiverOptions::new(Source::new("fixture\0name"))
        .color_format(ReceiverColorFormat::Best444);
    let before = calls("receiver-create");
    let error = Receiver::new(ndi, &malformed)
        .err()
        .expect("invalid source name");
    if modern {
        assert!(matches!(error, Error::InvalidCString(_)));
    } else {
        refused(&error, RuntimeFeature::ReceiveBest444);
    }
    assert_eq!(calls("receiver-create"), before);
}

#[cfg(feature = "advanced")]
fn advanced(ndi: &Ndi, modern: bool) {
    use grafton_ndi::advanced::{
        AdvancedConfig, AvSync, AvSyncOutcome, AvSyncRequest, AvSyncVideo, QueueOptions,
        QueuedSender, QueuedVideoFrame,
    };
    if let Err(error) = ndi.require_feature(RuntimeFeature::SendCompletion) {
        common::skip(
            common::Requirement::Advanced,
            &format!("descriptor fixtures need Advanced declarations: {error}"),
        );
        return;
    }
    for (color, bandwidth, feature) in [
        (
            ReceiverColorFormat::Best444,
            ReceiverBandwidth::Highest,
            RuntimeFeature::ReceiveBest444,
        ),
        (
            ReceiverColorFormat::BgrxBgra,
            ReceiverBandwidth::VideoOnlyLowest,
            RuntimeFeature::ReceiveVideoOnly,
        ),
        (
            ReceiverColorFormat::BgrxBgra,
            ReceiverBandwidth::VideoOnlyHighest,
            RuntimeFeature::ReceiveVideoOnly,
        ),
    ] {
        let options = ReceiverOptions::new(Source::new("configured gate fixture"))
            .color_format(color)
            .bandwidth(bandwidth)
            .config(AdvancedConfig::parse("{}").unwrap());
        let before = calls("receiver-create");
        accepted(Receiver::new(ndi, &options), modern, feature);
        assert_eq!(calls("receiver-create"), before + usize::from(modern));
    }
    let mut queue = QueuedSender::new(
        ndi,
        &SenderOptions::new("queued descriptor gates"),
        QueueOptions::default(),
    )
    .unwrap();
    for format in PIXELS {
        let supported = modern || !gated(format);
        let input = frame(format);
        let pixels = input.data().as_ptr();
        let before = calls("sender-submit");
        match queue.try_submit(input) {
            Ok(_) => {
                assert!(supported);
                let completed = queue
                    .try_take_completed()
                    .expect("immediate native completion");
                let QueuedVideoFrame::Pixels(recovered) = completed.into_frame() else {
                    panic!("wrong queued variant")
                };
                assert_eq!(recovered.data().as_ptr(), pixels);
            }
            Err(rejected) => {
                assert!(!supported);
                refused(rejected.error(), RuntimeFeature::Planar444Video);
                let QueuedVideoFrame::Pixels(recovered) = rejected.into_inner() else {
                    panic!("wrong queued variant")
                };
                assert_eq!(recovered.data().as_ptr(), pixels);
            }
        }
        assert_eq!(calls("sender-submit"), before + usize::from(supported));
        assert_eq!(queue.retained_frames(), 0);
    }
    let receiver =
        Receiver::new(ndi, &ReceiverOptions::new(Source::new("AV sync fixture"))).unwrap();
    let mut sync = AvSync::new(receiver).unwrap();
    for (serial, (token, format)) in [
        ("gate-na48", PixelFormat::Na48),
        ("gate-nb48", PixelFormat::Nb48),
        ("gate-na4g", PixelFormat::Na4g),
        ("gate-nb4g", PixelFormat::Nb4g),
        ("gate-nb4g-invalid-metadata", PixelFormat::Nb4g),
    ]
    .into_iter()
    .enumerate()
    {
        std::env::set_var("GRAFTON_FAKE_CAPTURE_SCRIPT", format!("{serial}:{token}"));
        let captured = sync
            .receiver()
            .video()
            .capture_ref(Duration::ZERO)
            .unwrap()
            .unwrap();
        assert_eq!(captured.pixel_format(), format);
        assert_eq!(captured.timestamp().unwrap().ticks(), 5678);
        let before = calls("avsync-synchronize:");
        assert!(matches!(
            sync.synchronize(
                AvSyncVideo::new(&captured).unwrap(),
                AvSyncRequest::automatic()
            ),
            Ok(AvSyncOutcome::NoAudioStream)
        ));
        assert_eq!(calls("avsync-synchronize:"), before + 1);
        if token.ends_with("invalid-metadata") {
            assert!(captured.metadata().is_none());
            assert!(events().contains(&format!("avsync-synchronize:{}:5678:true", format as u32)));
        }
        let owned = captured.to_owned();
        drop(captured);
        for request in [
            AvSyncRequest::automatic(),
            AvSyncRequest::new(Some(48_000), Some(1600)).unwrap(),
        ] {
            let before = calls("avsync-synchronize:");
            accepted(
                sync.synchronize(AvSyncVideo::new(&owned).unwrap(), request),
                modern,
                RuntimeFeature::Planar444Video,
            );
            assert_eq!(calls("avsync-synchronize:"), before + usize::from(modern));
            let (capture, _) = sync.split();
            let before = calls("avsync-synchronize:");
            accepted(
                capture.synchronize(AvSyncVideo::new(&owned).unwrap(), request),
                modern,
                RuntimeFeature::Planar444Video,
            );
            assert_eq!(calls("avsync-synchronize:"), before + usize::from(modern));
        }
    }
}
