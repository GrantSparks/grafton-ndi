//! Sending and receiver options apply format gates; AV sync uses validated timing.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

#[cfg(feature = "advanced")]
#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
#[cfg(feature = "advanced")]
common_helpers!(policy, prerequisites, child, fixture);
use common::TestChild;
#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;
use runtime_fixture::read_fake_log;

use grafton_ndi::{
    capabilities::RuntimeFeature, Error, Ndi, PixelFormat, Receiver, ReceiverBandwidth,
    ReceiverColorFormat, ReceiverOptions, RuntimeOptions, Sender, SenderOptions, Source,
    VideoFrame,
};
#[cfg(feature = "advanced")]
use std::time::Duration;
use std::{
    fs,
    path::{Path, PathBuf},
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

fn calls(prefix: &str) -> usize {
    read_fake_log()
        .lines()
        .filter(|event| *event == prefix || (prefix.ends_with(':') && event.starts_with(prefix)))
        .count()
}

fn refused(error: &Error, expected: RuntimeFeature) {
    assert!(
        matches!(error, Error::UnsupportedCapability {
        capability: grafton_ndi::Capability::Runtime(feature), refusal: grafton_ndi::CapabilityRefusal::RuntimeTooOld { .. }, ..
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
    let directory = common::FixtureDir::create("capability-descriptors");
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
    TestChild::new("caller_descriptors_apply_version_gates_before_native_operations")
        .env("GRAFTON_DESCRIPTOR_LIBRARY", library)
        .env("GRAFTON_DESCRIPTOR_MODERN", if modern { "1" } else { "0" })
        .env("GRAFTON_FAKE_NDI_LOG", directory.join("descriptors.log"))
        .env("GRAFTON_FAKE_CALLBACK_MODE", "immediate")
        .run();
}

fn sending(ndi: &Ndi, modern: bool) {
    let mut sender = Sender::new(ndi, &SenderOptions::new("descriptor gates")).unwrap();
    let earlier = frame(PixelFormat::Bgra);
    assert!(sender.submit_video(earlier).unwrap().is_none());
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
        match sender.submit_video(input) {
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
            sender.submit_video_copy(&input),
            supported,
            RuntimeFeature::Planar444Video,
        );
        assert_eq!(calls("sender-submit"), before + usize::from(supported));
    }
    assert!(sender.reclaim_video().is_some());
    // A refusal must also leave a prior owned submission untouched.
    if !modern {
        let mut sender = Sender::new(ndi, &SenderOptions::new("pending descriptor gate")).unwrap();
        let earlier = frame(PixelFormat::Bgra);
        let pointer = earlier.data().as_ptr();
        sender.submit_video(earlier).unwrap();
        for format in [
            PixelFormat::Na48,
            PixelFormat::Nb48,
            PixelFormat::Na4g,
            PixelFormat::Nb4g,
        ] {
            let input = frame(format);
            assert!(sender.send_video(&input).is_err());
            assert!(sender.submit_video_copy(&input).is_err());
            assert!(sender.submit_video(input).is_err());
        }
        assert_eq!(sender.reclaim_video().unwrap().data().as_ptr(), pointer);
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
            let options = ReceiverOptions::new(Source::new("fixture").unwrap())
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
    let malformed = ReceiverOptions::new(Source::new("fixture").unwrap())
        .name("fixture\0name")
        .color_format(ReceiverColorFormat::Best444);
    let before = calls("receiver-create");
    let error = Receiver::new(ndi, &malformed).expect_err("invalid receiver name");
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
        AdvancedConfig, AvSync, AvSyncOutcome, AvSyncRequest, AvSyncVideo, QueueLimits,
        QueuedSender, QueuedVideoFrame,
    };
    if let Err(error) = ndi.require_feature(RuntimeFeature::SendCompletion) {
        common::skip(
            common::Requirement::Advanced,
            &format!("descriptor fixtures need Advanced declarations: {error}"),
        );
        return;
    }
    synchronizer_lifetimes(ndi);
    allocator_lifetimes(ndi);
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
        let options = ReceiverOptions::new(Source::new("configured gate fixture").unwrap())
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
        &QueueLimits::default(),
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
                    .try_next_completed()
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
    let receiver = Receiver::new(
        ndi,
        &ReceiverOptions::new(Source::new("AV sync fixture").unwrap()),
    )
    .unwrap();
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
            .capture_video_ref(Duration::ZERO)
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
            assert!(read_fake_log()
                .contains(&format!("avsync-synchronize:{}:5678:true", format as u32)));
        }
        let owned = captured.to_owned();
        drop(captured);
        // Capture through the split receiver too, retaining a native-call
        // count assertion for the borrowed split-view path on every runtime.
        std::env::set_var(
            "GRAFTON_FAKE_CAPTURE_SCRIPT",
            format!("split-{serial}:{token}"),
        );
        {
            let (capture, controls) = sync.split();
            let borrowed = controls
                .receiver()
                .capture_video_ref(Duration::ZERO)
                .unwrap()
                .unwrap();
            let before = calls("avsync-synchronize:");
            assert!(matches!(
                capture.synchronize(
                    AvSyncVideo::new(&borrowed).unwrap(),
                    AvSyncRequest::automatic()
                ),
                Ok(AvSyncOutcome::NoAudioStream)
            ));
            assert_eq!(calls("avsync-synchronize:"), before + 1);
        }
        for request in [
            AvSyncRequest::automatic(),
            AvSyncRequest::new(Some(48_000), Some(1600)).unwrap(),
        ] {
            let before = calls("avsync-synchronize:");
            // AV sync matches this frame's validated timing independently of
            // the runtime's ability to send its planar pixel format.
            assert!(matches!(
                sync.synchronize(AvSyncVideo::new(&owned).unwrap(), request),
                Ok(AvSyncOutcome::NoAudioStream)
            ));
            assert_eq!(calls("avsync-synchronize:"), before + 1);
            let (capture, _) = sync.split();
            let before = calls("avsync-synchronize:");
            assert!(matches!(
                capture.synchronize(AvSyncVideo::new(&owned).unwrap(), request),
                Ok(AvSyncOutcome::NoAudioStream)
            ));
            assert_eq!(calls("avsync-synchronize:"), before + 1);
        }
    }
}

#[cfg(feature = "advanced")]
fn synchronizer_lifetimes(ndi: &Ndi) {
    use grafton_ndi::advanced::{AdvancedConfig, AvSync, Genlock, GenlockOptions};
    for recover in [false, true] {
        let start = read_fake_log().len();
        let receiver = Receiver::new(
            ndi,
            &ReceiverOptions::new(Source::new("lifetime fixture").unwrap()),
        )
        .unwrap();
        let sync = AvSync::new(receiver).unwrap();
        if recover {
            drop(sync.into_receiver());
        } else {
            drop(sync);
        }
        let trace = read_fake_log();
        let trace = &trace[start..];
        assert_eq!(trace.matches("avsync-destroy").count(), 1);
        assert_eq!(trace.matches("receiver-destroy").count(), 1);
        assert!(trace.find("avsync-destroy").unwrap() < trace.find("receiver-destroy").unwrap());
    }
    if let Err(error) = ndi.require_feature(RuntimeFeature::Genlock) {
        common::skip(
            common::Requirement::Advanced,
            &format!("genlock fixture needs declarations: {error}"),
        );
        return;
    }
    let source = Source::new("genlock-source").unwrap();
    let config = AdvancedConfig::parse(r#"{"marker":"alive"}"#).unwrap();
    let mut genlock = Genlock::new(
        ndi,
        &GenlockOptions::new()
            .source(source.clone())
            .config(config.clone()),
    )
    .unwrap();
    drop(source);
    drop(config);
    genlock.connect_to(&Source::new("genlock-reconnected").unwrap());
    drop(genlock);
    assert!(read_fake_log().contains(r#"genlock-destroy:genlock-reconnected:{"marker":"alive"}"#));
}

#[cfg(feature = "advanced")]
fn allocator_lifetimes(ndi: &Ndi) {
    use grafton_ndi::advanced::{
        AllocationBuffer, AllocatorLimits, AllocatorService, PacketReceiver, PassthroughPreset,
    };
    if let Err(error) = ndi.require_feature(RuntimeFeature::ReceiveAllocators) {
        common::skip(
            common::Requirement::Advanced,
            &format!("allocator fixture needs declarations: {error}"),
        );
        return;
    }
    let service =
        AllocatorService::new(ndi.process_lifetime(), &AllocatorLimits::default()).unwrap();
    for packets in [false, true] {
        for recover in [false, true] {
            let start = read_fake_log().len();
            let options = ReceiverOptions::new(Source::new("allocator lifetime fixture").unwrap());
            if packets {
                let receiver = PacketReceiver::new(ndi, &options, PassthroughPreset::Hevc).unwrap();
                let mut receiver = service.attach_packets(receiver).unwrap();
                receiver
                    .set_video_allocator(AllocationBuffer::zeroed)
                    .unwrap();
                receiver
                    .set_audio_allocator(AllocationBuffer::zeroed)
                    .unwrap();
                if recover {
                    drop(receiver.into_packet_receiver());
                } else {
                    drop(receiver);
                }
            } else {
                let receiver = Receiver::new(ndi, &options).unwrap();
                let mut receiver = service.attach(receiver).unwrap();
                receiver
                    .set_video_allocator(AllocationBuffer::zeroed)
                    .unwrap();
                receiver
                    .set_audio_allocator(AllocationBuffer::zeroed)
                    .unwrap();
                if recover {
                    drop(receiver.into_receiver());
                } else {
                    drop(receiver);
                }
            }
            let trace = read_fake_log();
            let trace = &trace[start..];
            assert_eq!(trace.matches("receiver-destroy").count(), 1, "{trace}");
            for stream in ["video", "audio"] {
                let install = format!("{stream}-allocator-install");
                let reset = format!("{stream}-allocator-reset");
                assert_eq!(trace.matches(&install).count(), 1, "{trace}");
                assert_eq!(trace.matches(&reset).count(), 1, "{trace}");
                assert!(trace.find(&install).unwrap() < trace.find(&reset).unwrap());
                assert!(trace.find(&reset).unwrap() < trace.find("receiver-destroy").unwrap());
            }
        }
    }
}
