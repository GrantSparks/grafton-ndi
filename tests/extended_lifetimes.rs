//! Test-owned native fixtures exercise ownership order without SDK discovery.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(policy, prerequisites, child, fixture);
use common::TestChild;
#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;

use grafton_ndi::{
    capabilities::{RuntimeFeature, SdkEdition},
    discovery_server::{
        AdvertiserOptions, RecvAdvertisement, RecvAdvertiser, SendAdvertisement, SendAdvertiser,
    },
    runtime::RuntimeOptions,
    Error, Ndi, Receiver, ReceiverOptions, Sender, SenderOptions, Source, SourceAddress,
    VideoFrame,
};
use runtime_fixture::{read_fake_log, FakeRuntime};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

/// Build a fake runtime whose senders, receivers and advertisers log their
/// lifecycle. It reports the first version whose advertisers are usable.
fn fixture(directory: &Path, advanced: bool) -> PathBuf {
    let mut runtime = FakeRuntime {
        cfgs: vec!["fake_objects"],
        version: Some("6.4.0.0"),
        ..Default::default()
    };
    if advanced {
        runtime.cfgs.push("fake_advanced");
        runtime.build(&directory.join("libndi_advanced.so"))
    } else {
        runtime.build(&directory.join("libndi.so"))
    }
}
/// Bytes as the fake runtime logs the strings it is given.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
/// Where the fake runtime logged `event`.
fn position(events: &str, event: &str) -> usize {
    events
        .find(&format!("{event}\n"))
        .unwrap_or_else(|| panic!("`{event}` was not logged: {events}"))
}
/// The Discovery Server every advertiser here names. The SDK creates an
/// advertiser only for an address, and does not need to reach it.
fn server() -> AdvertiserOptions {
    AdvertiserOptions::default().servers(["192.0.2.1"])
}
fn frame() -> VideoFrame {
    VideoFrame::builder().resolution(2, 2).build().unwrap()
}
fn sender(ndi: &Ndi) -> Sender {
    Sender::new(ndi, &SenderOptions::new("fixture")).unwrap()
}

fn retained_source(name: &str, address: &str) -> Source {
    Source::new(name)
        .unwrap()
        .with_address(SourceAddress::new(address).unwrap())
}

fn assert_retained_reads(kind: &str, expected: &[(&str, &str)]) {
    let prefix = format!("retained-source:{kind}:");
    let observed = read_fake_log()
        .lines()
        .filter_map(|line| line.strip_prefix(&prefix).map(str::to_owned))
        .collect::<Vec<_>>();
    let expected = expected
        .iter()
        .map(|(name, address)| format!("{}:{}", hex(name.as_bytes()), hex(address.as_bytes())))
        .collect::<Vec<_>>();
    assert_eq!(
        observed, expected,
        "later reads must retain the exact source"
    );
}

#[test]
fn native_resource_ownership() {
    if let Ok(case) = std::env::var("GRAFTON_OWNERSHIP_CHILD") {
        child(&case);
        return;
    }
    let directory = common::FixtureDir::create("ownership");
    let standard = fixture(directory.path(), false);
    let advanced = fixture(directory.path(), true);
    let missing_scatter = directory.join("missing-scatter");
    fs::create_dir_all(&missing_scatter).unwrap();
    let missing_scatter = FakeRuntime {
        cfgs: vec!["fake_objects", "fake_advanced", "fake_omit_scatter"],
        version: Some("6.4.0.0"),
        ..Default::default()
    }
    .build(&missing_scatter.join("libndi_advanced.so"));
    let mut scenarios = vec![
        "retained_failover",
        "retained_registered_failover",
        "retained_concurrent_failover",
        "retained_routes",
        "retained_connection_targets",
        "retained_creation_target",
        "retained_receiver_name",
        "retained_receiver_metadata",
        "retained_registered_receiver_name",
        "sender_unregistered",
        "sender_copy_reuse",
        "sender_split_copy_reuse",
        "sender_split_overlap",
        "sender_split_drop",
        "sender_registration",
        "dropped_sender_advertiser",
        "receiver_registration",
        "dropped_receiver_advertiser",
        "foreign_registration",
        "refused_unregistration",
        "malformed_metadata",
        "metadata_count",
    ];
    if cfg!(feature = "advanced") {
        scenarios.extend([
            "compressed_destroy",
            "retained_router_metadata",
            "compressed_close",
            "compressed_finish",
            "queue_immediate",
            "queue_backlog",
            "queue_split",
            "queue_scatter_immediate",
            "queue_scatter_delayed",
            "queue_scatter_protocol",
            "queue_scatter_unavailable",
            "queue_destroy",
            "queue_close",
            "queue_registered_sender",
            "queue_sender_conversion",
        ]);
    }
    for case in scenarios {
        TestChild::new("native_resource_ownership")
            .env("GRAFTON_OWNERSHIP_CHILD", case)
            .env(
                "GRAFTON_OWNERSHIP_LIBRARY",
                if case == "queue_scatter_unavailable" {
                    &missing_scatter
                } else if case.starts_with("queue_")
                    || case.starts_with("compressed_")
                    || case == "retained_router_metadata"
                {
                    &advanced
                } else {
                    &standard
                },
            )
            .env(
                "GRAFTON_FAKE_CALLBACK_MODE",
                if matches!(
                    case,
                    "queue_immediate"
                        | "queue_backlog"
                        | "queue_scatter_immediate"
                        | "queue_scatter_protocol"
                ) {
                    "immediate"
                } else {
                    "delayed"
                },
            )
            .env(
                "GRAFTON_FAKE_NDI_LOG",
                directory.join(format!("{case}.log")),
            )
            .run();
    }
}
fn child(case: &str) {
    let advanced = case.starts_with("queue_")
        || case.starts_with("compressed_")
        || case == "retained_router_metadata";
    let path = PathBuf::from(std::env::var_os("GRAFTON_OWNERSHIP_LIBRARY").unwrap());
    let ndi = Ndi::with_options(&RuntimeOptions::default().library_path(path).edition(
        if advanced {
            SdkEdition::Advanced
        } else {
            SdkEdition::Standard
        },
    ))
    .unwrap();
    // The queue scenarios call Advanced entry points, which need a build with
    // the Advanced SDK declarations as well as a runtime that exports them.
    if advanced {
        if let Err(error) = ndi.require_feature(RuntimeFeature::AdvancedSenderConfiguration) {
            common::skip(
                common::Requirement::Advanced,
                &format!("the Advanced SDK cannot be used: {error}"),
            );
            return;
        }
    }
    if case.starts_with("retained_") {
        std::env::set_var("GRAFTON_FAKE_RETAIN_SOURCES", "1");
    }
    match case {
        "retained_failover" => {
            let sender = sender(&ndi);
            sender.set_failover(&retained_source("first failover", "192.0.2.1:5960"));
            sender.set_failover(&retained_source(
                "second failover",
                "http://backup.example/ndi",
            ));
            sender.clear_failover();
            sender.set_failover(&retained_source("last failover", "192.0.2.3:5962"));
            drop(sender);
            assert_retained_reads(
                "failover",
                &[
                    ("first failover", "192.0.2.1:5960"),
                    ("second failover", "http://backup.example/ndi"),
                    ("last failover", "192.0.2.3:5962"),
                ],
            );
        }
        "retained_registered_failover" => {
            let sender = sender(&ndi);
            sender.set_failover(&retained_source("registered failover", "192.0.2.5:5960"));
            let mut advertiser = SendAdvertiser::new(&ndi, &server()).unwrap();
            let _registration = advertiser
                .register(&sender, &SendAdvertisement::default())
                .unwrap();
            drop(sender);
            assert!(!read_fake_log().contains("sender-destroy\n"));
            assert_retained_reads("failover", &[]);
            drop(advertiser);
            assert_retained_reads("failover", &[("registered failover", "192.0.2.5:5960")]);
            let events = read_fake_log();
            assert!(
                position(&events, "send-advertiser-destroy")
                    < position(&events, "sender-destroy-begin")
            );
        }
        "retained_concurrent_failover" => {
            let sender = sender(&ndi);
            std::thread::scope(|scope| {
                for worker in 0..4 {
                    let sender = &sender;
                    scope.spawn(move || {
                        for change in 0..20 {
                            let name = format!("concurrent-{worker}-{change}");
                            let address = format!("http://example.test/{name}");
                            sender.set_failover(&retained_source(&name, &address));
                        }
                    });
                }
            });
            drop(sender);
            let events = read_fake_log();
            let reads = events
                .lines()
                .filter_map(|line| line.strip_prefix("retained-source:failover:"))
                .collect::<Vec<_>>();
            assert_eq!(reads.len(), 80);
            for read in reads {
                assert!(
                    (0..4).any(|worker| (0..20).any(|change| {
                        let name = format!("concurrent-{worker}-{change}");
                        read == format!(
                            "{}:{}",
                            hex(name.as_bytes()),
                            hex(format!("http://example.test/{name}").as_bytes())
                        )
                    })),
                    "a concurrent request must retain its matching name and address: {read}"
                );
            }
        }
        "retained_routes" => {
            use grafton_ndi::routing::{Router, RouterOptions};
            let mut router = Router::new(&ndi, &RouterOptions::new("retained router")).unwrap();
            for name in ["route A", "route B"] {
                router
                    .set_route(&retained_source(name, "192.0.2.10:5960"))
                    .unwrap();
            }
            std::env::set_var("GRAFTON_FAKE_ROUTING_CHANGE", "refuse");
            for name in ["refused C", "refused D"] {
                assert!(router
                    .set_route(&retained_source(name, "http://refused.example/ndi"))
                    .is_err());
            }
            std::env::remove_var("GRAFTON_FAKE_ROUTING_CHANGE");
            router
                .set_route(&retained_source("route E", "192.0.2.11:5960"))
                .unwrap();
            router.clear_route().unwrap();
            router
                .set_route(&retained_source("route F", "192.0.2.12:5960"))
                .unwrap();
            drop(router);
            assert_retained_reads(
                "route",
                &[
                    ("route A", "192.0.2.10:5960"),
                    ("route B", "192.0.2.10:5960"),
                    ("route B", "192.0.2.10:5960"),
                    ("route B", "192.0.2.10:5960"),
                    ("route E", "192.0.2.11:5960"),
                    ("route F", "192.0.2.12:5960"),
                ],
            );
            assert_retained_reads(
                "refused-route",
                &[
                    ("refused C", "http://refused.example/ndi"),
                    ("refused D", "http://refused.example/ndi"),
                ],
            );
        }
        "retained_creation_target" => {
            let receiver = Receiver::new(
                &ndi,
                &ReceiverOptions::new(retained_source("creation target", "192.0.2.19:5960")),
            )
            .unwrap();
            // Every caller-side source/options value is gone before destroy
            // asks the fake SDK to read the exact creation strings again.
            drop(receiver);
            assert_retained_reads("receiver", &[("creation target", "192.0.2.19:5960")]);
        }
        "retained_connection_targets" => {
            let receiver =
                Receiver::new(&ndi, &ReceiverOptions::new(Source::new("").unwrap())).unwrap();
            receiver.connect_to(&retained_source("target A", "192.0.2.20:5960"));
            receiver.reconnect();
            receiver.connect_to(&retained_source("target B", "http://target.example/ndi"));
            receiver.disconnect();
            receiver.reconnect();
            drop(receiver);
            assert_retained_reads(
                "receiver",
                &[
                    ("target A", "192.0.2.20:5960"),
                    ("target A", "192.0.2.20:5960"),
                    ("target B", "http://target.example/ndi"),
                    ("target B", "http://target.example/ndi"),
                ],
            );
        }
        "retained_receiver_metadata" => {
            let receiver =
                Receiver::new(&ndi, &ReceiverOptions::new(Source::new("").unwrap())).unwrap();
            for index in 0..16 {
                let frame = grafton_ndi::MetadataFrame::new(format!("<entry index='{index}'/>"))
                    .unwrap()
                    .with_timecode(grafton_ndi::Timecode::from_ticks(index));
                receiver.add_connection_metadata(&frame).unwrap();
            }
            receiver.connect_to(&Source::new("metadata target").unwrap());
            receiver.reconnect();
            receiver.clear_connection_metadata().unwrap();
            receiver
                .add_connection_metadata(&grafton_ndi::MetadataFrame::new("<last/>").unwrap())
                .unwrap();
            drop(receiver);
            let events = read_fake_log();
            for index in 0..16 {
                let expected = format!(
                    "retained-metadata:receiver:{}:{index}",
                    hex(format!("<entry index='{index}'/>").as_bytes())
                );
                assert_eq!(events.lines().filter(|line| *line == expected).count(), 3);
            }
            assert!(events.contains(&format!("retained-metadata:receiver:{}:", hex(b"<last/>"))));
        }
        #[cfg(feature = "advanced")]
        "retained_router_metadata" => {
            let mut router = grafton_ndi::routing::Router::new(
                &ndi,
                &grafton_ndi::routing::RouterOptions::new("metadata router"),
            )
            .unwrap();
            for index in 0..16 {
                router
                    .add_connection_metadata(
                        &grafton_ndi::MetadataFrame::new(format!("<entry index='{index}'/>"))
                            .unwrap()
                            .with_timecode(grafton_ndi::Timecode::from_ticks(index)),
                    )
                    .unwrap();
            }
            router
                .set_route(&Source::new("metadata target").unwrap())
                .unwrap();
            router.clear_connection_metadata().unwrap();
            router
                .add_connection_metadata(&grafton_ndi::MetadataFrame::new("<last/>").unwrap())
                .unwrap();
            drop(router);
            let events = read_fake_log();
            for index in 0..16 {
                let expected = format!(
                    "retained-metadata:router:{}:{index}",
                    hex(format!("<entry index='{index}'/>").as_bytes())
                );
                assert_eq!(events.lines().filter(|line| *line == expected).count(), 2);
            }
            assert!(events.contains(&format!("retained-metadata:router:{}:", hex(b"<last/>"))));
        }
        "retained_receiver_name" | "retained_registered_receiver_name" => {
            let name = "retained receiver creation name";
            let options = ReceiverOptions::new(Source::new("").unwrap()).name(name);
            let receiver = Receiver::new(&ndi, &options).unwrap();
            drop(options);
            receiver.connect_to(&Source::new("named target").unwrap());
            receiver.reconnect();
            if case == "retained_registered_receiver_name" {
                let mut advertiser = RecvAdvertiser::new(&ndi, &server()).unwrap();
                let _registration = advertiser
                    .register(&receiver, &RecvAdvertisement::default())
                    .unwrap();
                drop(receiver);
                assert!(!read_fake_log().contains("receiver-destroy\n"));
                drop(advertiser);
            } else {
                drop(receiver);
            }
            let expected = format!("retained-receiver-name:{}", hex(name.as_bytes()));
            let events = read_fake_log();
            let reads: Vec<_> = events
                .lines()
                .filter(|line| line.starts_with("retained-receiver-name:"))
                .collect();
            assert_eq!(
                reads,
                [expected.as_str(), expected.as_str(), expected.as_str()]
            );
        }
        #[cfg(feature = "advanced")]
        "compressed_finish" => {
            use grafton_ndi::{
                advanced::{
                    CompressedSender, CompressedVideoFrame, SubmittedVideo, VideoCodec,
                    VideoPacketFormat,
                },
                StreamTier,
            };
            std::env::set_var("GRAFTON_FAKE_VALIDATE_COMPRESSED_LOANS", "1");
            let mut sender =
                CompressedSender::new(&ndi, &SenderOptions::new("compressed-finish")).unwrap();
            for (tier, value) in [(StreamTier::Program, 11), (StreamTier::Preview, 22)] {
                let format = VideoPacketFormat::new(VideoCodec::SpeedHq422, 16, 16)
                    .unwrap()
                    .with_tier(tier);
                let frame = CompressedVideoFrame::new(format, vec![value; 16]).unwrap();
                assert!(sender.submit_video(frame).is_none());
            }
            let reclaimed = sender.reclaim_video();
            let events = read_fake_log();
            assert_eq!(events.matches("sender-flush\n").count(), 1);
            assert!(!events.contains("sender-destroy-begin\n"));
            for value in [11, 22] {
                assert!(
                    position(&events, "sender-flush")
                        < position(&events, &format!("sender-loan-released={value}"))
                );
            }
            let empty = sender.split().0.reclaim_video();
            assert!(empty.program.is_none() && empty.preview.is_none());
            assert_eq!(
                read_fake_log(),
                events,
                "an empty reclaim makes no native call"
            );
            for (frame, value) in [
                (reclaimed.program.unwrap(), 11),
                (reclaimed.preview.unwrap(), 22),
            ] {
                let SubmittedVideo::Contiguous(frame) = frame else {
                    panic!("contiguous frame");
                };
                assert_eq!(frame.data()[0], value);
                assert!(sender.submit_video(frame).is_none());
            }
            drop(sender);
            assert!(
                position(&read_fake_log(), "sender-flush")
                    < position(&read_fake_log(), "sender-destroy-begin")
            );
        }
        #[cfg(feature = "advanced")]
        "compressed_destroy" | "compressed_close" => {
            use grafton_ndi::{
                advanced::{CompressedSender, CompressedVideoFrame, VideoCodec, VideoPacketFormat},
                StreamTier,
            };
            std::env::set_var("GRAFTON_FAKE_VALIDATE_COMPRESSED_LOANS", "1");
            let mut sender =
                CompressedSender::new(&ndi, &SenderOptions::new("compressed-owner")).unwrap();
            for (tier, value) in [(StreamTier::Program, 11), (StreamTier::Preview, 22)] {
                let format = VideoPacketFormat::new(VideoCodec::SpeedHq422, 16, 16)
                    .unwrap()
                    .with_tier(tier);
                let frame = CompressedVideoFrame::new(format, vec![value; 16]).unwrap();
                assert!(sender.submit_video(frame).is_none());
            }
            if case == "compressed_close" {
                let reclaimed = sender.close();
                assert_eq!(
                    match reclaimed.program.unwrap() {
                        grafton_ndi::advanced::SubmittedVideo::Contiguous(frame) => frame.data()[0],
                        _ => panic!("contiguous program"),
                    },
                    11
                );
                assert_eq!(
                    match reclaimed.preview.unwrap() {
                        grafton_ndi::advanced::SubmittedVideo::Contiguous(frame) => frame.data()[0],
                        _ => panic!("contiguous preview"),
                    },
                    22
                );
            } else {
                drop(sender);
            }
            let events = read_fake_log();
            for value in [11, 22] {
                let release = position(&events, &format!("sender-loan-released={value}"));
                assert!(position(&events, "sender-destroy-begin") < release);
                assert!(release < position(&events, "sender-destroy"));
            }
            assert_eq!(events.matches("sender-destroy\n").count(), 1);
        }
        "sender_copy_reuse" | "sender_split_copy_reuse" => {
            use grafton_ndi::{BorrowedVideoFrame, PixelFormat};
            std::env::set_var("GRAFTON_FAKE_VALIDATE_VIDEO_COPY", "1");
            let mut sender = sender(&ndi);
            let mut pixels = vec![0; 16 * 16 * 4];
            // The fake SDK rereads each previous submission during the next
            // native call, before returning its release proof. Refilling the
            // frame still in flight would change the byte it expects.
            for value in 1..=6 {
                pixels.fill(value);
                let source = BorrowedVideoFrame::builder(&pixels, 16, 16, PixelFormat::Bgra)
                    .build()
                    .unwrap();
                if case == "sender_split_copy_reuse" {
                    sender.split().0.submit_video_copy(&source).unwrap();
                } else {
                    sender.submit_video_copy(&source).unwrap();
                }
            }
            pixels.fill(9);
            drop(pixels);
            drop(sender);
            let events = read_fake_log();
            for value in 1..=6 {
                assert_eq!(
                    events
                        .matches(&format!("sender-copy-released={value}\n"))
                        .count(),
                    1
                );
            }
            assert_eq!(events.matches("sender-submit-return\n").count(), 6);
            assert!(
                position(&events, "sender-flush") < position(&events, "sender-copy-released=6")
            );
            assert!(
                position(&events, "sender-copy-released=6")
                    < position(&events, "sender-destroy-begin")
            );
        }
        "sender_split_overlap" => {
            use std::time::Instant;
            let gate = common::FixtureDir::create("raw-video-gate");
            std::env::set_var("GRAFTON_FAKE_VIDEO_GATE", gate.path());
            struct Release<'a>(&'a Path);
            impl Drop for Release<'_> {
                fn drop(&mut self) {
                    fs::write(self.0.join("release"), b"").unwrap();
                }
            }
            let mut sender = sender(&ndi);
            let (mut video, shared) = sender.split();
            std::thread::scope(|scope| {
                let submission = scope.spawn(move || video.submit_video(frame()).unwrap());
                let _release = Release(gate.path());
                let deadline = Instant::now() + Duration::from_secs(10);
                while !gate.join("entered").exists() {
                    assert!(Instant::now() < deadline, "video never entered the SDK");
                    std::thread::sleep(Duration::from_millis(1));
                }
                let audio = grafton_ndi::AudioFrame::builder()
                    .num_samples(480)
                    .build()
                    .unwrap();
                let (audio_done, completed) = std::sync::mpsc::sync_channel(1);
                scope.spawn(move || {
                    shared.send_audio(&audio);
                    let _ = audio_done.send(());
                });
                // On failure this thread unwinds through Release before the
                // scope joins, so a video-lock regression releases both workers.
                assert!(
                    completed.recv_timeout(Duration::from_secs(2)).is_ok(),
                    "audio waited for the blocked video submission"
                );
                assert!(
                    !submission.is_finished(),
                    "video must still be blocked in the SDK"
                );
                let events = read_fake_log();
                assert!(events.contains("sender-audio\n"));
                assert!(!events.contains("sender-video-released\n"));
            });
            std::env::remove_var("GRAFTON_FAKE_VIDEO_GATE");
            drop(sender);
            let events = read_fake_log();
            assert!(position(&events, "sender-audio") < position(&events, "sender-video-released"));
            assert_eq!(events.matches("sender-flush\n").count(), 1);
            assert_eq!(events.matches("sender-destroy\n").count(), 1);
        }
        "sender_split_drop" => {
            let mut sender = sender(&ndi);
            {
                let (mut video, shared) = sender.split();
                video.submit_video(frame()).unwrap();
                shared.flush_video();
                assert!(video.reclaim_video().is_some());
                assert!(video.reclaim_video().is_none());
                video.submit_video(frame()).unwrap();
            }
            // Dropping the borrowed view leaves this final loan with its owner.
            assert_eq!(read_fake_log().matches("sender-flush\n").count(), 1);
            drop(sender);
            let events = read_fake_log();
            assert_eq!(events.matches("sender-flush\n").count(), 2);
            assert_eq!(events.matches("sender-destroy\n").count(), 1);
        }
        "sender_unregistered" => {
            let mut sender = sender(&ndi);
            sender.submit_video(frame()).unwrap();
            drop(sender);
            let events = read_fake_log();
            assert_eq!(events.matches("sender-flush\n").count(), 1);
            assert!(position(&events, "sender-flush") < position(&events, "sender-destroy-begin"));
            assert_eq!(events.matches("sender-destroy\n").count(), 1);
        }
        "sender_registration" => {
            let mut sender = sender(&ndi);
            let mut advertiser = SendAdvertiser::new(&ndi, &server()).unwrap();
            let registration = advertiser
                .register(&sender, &SendAdvertisement::default())
                .unwrap();
            assert!(advertiser
                .register(&sender, &SendAdvertisement::default())
                .is_err());
            sender.submit_video(frame()).unwrap();
            drop(sender);
            assert!(read_fake_log().contains("sender-flush\n"));
            // The registration, not the dropped handle, keeps the sender alive.
            assert!(!read_fake_log().contains("sender-destroy\n"));
            advertiser.unregister(registration).unwrap();
            let events = read_fake_log();
            assert!(
                position(&events, "send-advertiser-remove") < position(&events, "sender-destroy")
            );
            assert!(!events.contains("send-advertiser-destroy\n"));
            drop(advertiser);
            let events = read_fake_log();
            assert_eq!(
                events
                    .matches("send-advertiser-add monitoring=false\n")
                    .count(),
                1
            );
            assert_eq!(events.matches("send-advertiser-remove\n").count(), 1);
            assert_eq!(events.matches("sender-destroy\n").count(), 1);
            assert_eq!(events.matches("send-advertiser-destroy\n").count(), 1);
        }
        "dropped_sender_advertiser" => {
            let sender = sender(&ndi);
            let mut advertiser = SendAdvertiser::new(&ndi, &server()).unwrap();
            // Discarding the registration leaves the sender advertised.
            let _ = advertiser
                .register(
                    &sender,
                    &SendAdvertisement::default().allow_monitoring(true),
                )
                .unwrap();
            drop(sender);
            let events = read_fake_log();
            assert!(events.contains("send-advertiser-add monitoring=true\n"));
            assert!(!events.contains("send-advertiser-remove\n"));
            assert!(!events.contains("sender-destroy\n"));
            drop(advertiser);
            // The SDK's teardown order: stop advertising the sender, destroy
            // the advertiser, and only then destroy the sender.
            let events = read_fake_log();
            assert!(
                position(&events, "send-advertiser-remove")
                    < position(&events, "send-advertiser-destroy")
            );
            assert!(
                position(&events, "send-advertiser-destroy") < position(&events, "sender-destroy")
            );
            assert_eq!(events.matches("sender-destroy\n").count(), 1);
        }
        "receiver_registration" => {
            let receiver =
                Receiver::new(&ndi, &ReceiverOptions::new(Source::new("").unwrap())).unwrap();
            let mut advertiser = RecvAdvertiser::new(&ndi, &server()).unwrap();
            let registration = advertiser
                .register(&receiver, &RecvAdvertisement::default())
                .unwrap();
            assert!(advertiser
                .register(&receiver, &RecvAdvertisement::default())
                .is_err());
            drop(receiver);
            assert!(!read_fake_log().contains("receiver-destroy\n"));
            advertiser.unregister(registration).unwrap();
            let events = read_fake_log();
            assert!(
                position(&events, "recv-advertiser-remove group=null")
                    < position(&events, "receiver-destroy")
            );
            assert_eq!(events.matches("receiver-destroy\n").count(), 1);
            drop(advertiser);
        }
        "dropped_receiver_advertiser" => {
            let receiver =
                Receiver::new(&ndi, &ReceiverOptions::new(Source::new("").unwrap())).unwrap();
            let mut advertiser = RecvAdvertiser::new(&ndi, &server()).unwrap();
            let _ = advertiser
                .register(
                    &receiver,
                    &RecvAdvertisement::default().input_group("Program"),
                )
                .unwrap();
            drop(receiver);
            assert!(!read_fake_log().contains("receiver-destroy\n"));
            drop(advertiser);
            // The input-group name is still intact when the registration
            // ends, which is when this runtime reads it.
            let events = read_fake_log();
            let removed = format!("recv-advertiser-remove group={}", hex(b"Program"));
            assert!(position(&events, &removed) < position(&events, "recv-advertiser-destroy"));
            assert!(
                position(&events, "recv-advertiser-destroy")
                    < position(&events, "receiver-destroy")
            );
            assert_eq!(events.matches("receiver-destroy\n").count(), 1);
        }
        "foreign_registration" => {
            let sender = sender(&ndi);
            let mut advertiser = SendAdvertiser::new(&ndi, &server()).unwrap();
            let mut other = SendAdvertiser::new(&ndi, &server()).unwrap();
            let registration = advertiser
                .register(&sender, &SendAdvertisement::default())
                .unwrap();
            // Another advertiser neither ends the registration nor calls the
            // SDK for it: it hands the registration back with the reason.
            let rejected = other.unregister(registration).unwrap_err();
            assert!(matches!(rejected.error(), Error::InvalidInput(_)));
            assert!(!read_fake_log().contains("send-advertiser-remove\n"));
            let registration = rejected.into_inner();
            // It may advertise the same sender itself.
            let second = other
                .register(&sender, &SendAdvertisement::default())
                .unwrap();
            other.unregister(second).unwrap();
            drop(other);
            drop(sender);
            assert!(!read_fake_log().contains("sender-destroy\n"));
            // The registration that came back still ends where it was issued.
            advertiser.unregister(registration).unwrap();
            assert_eq!(read_fake_log().matches("sender-destroy\n").count(), 1);
            drop(advertiser);

            let receiver =
                Receiver::new(&ndi, &ReceiverOptions::new(Source::new("").unwrap())).unwrap();
            let mut advertiser = RecvAdvertiser::new(&ndi, &server()).unwrap();
            let mut other = RecvAdvertiser::new(&ndi, &server()).unwrap();
            let registration = advertiser
                .register(&receiver, &RecvAdvertisement::default())
                .unwrap();
            let rejected = other.unregister(registration).unwrap_err();
            assert!(!read_fake_log().contains("recv-advertiser-remove"));
            drop(receiver);
            advertiser.unregister(rejected.into_inner()).unwrap();
            assert_eq!(read_fake_log().matches("receiver-destroy\n").count(), 1);
        }
        "refused_unregistration" => {
            let sender = sender(&ndi);
            let mut advertiser = SendAdvertiser::new(&ndi, &server()).unwrap();
            let registration = advertiser
                .register(&sender, &SendAdvertisement::default())
                .unwrap();
            drop(sender);
            // An SDK that removes nothing fails the call, which changes
            // nothing: the registration comes back and keeps the sender alive.
            std::env::set_var("GRAFTON_FAKE_ADVERTISER_REMOVE", "refuse");
            let rejected = advertiser.unregister(registration).unwrap_err();
            assert!(matches!(
                rejected.error(),
                Error::NativeOperationFailed {
                    operation} if operation.name() == c"NDIlib_send_advertiser_del_sender"
            ));
            assert!(!read_fake_log().contains("sender-destroy\n"));
            std::env::remove_var("GRAFTON_FAKE_ADVERTISER_REMOVE");
            // The registration that came back can still be ended.
            advertiser.unregister(rejected.into_inner()).unwrap();
            assert_eq!(read_fake_log().matches("sender-destroy\n").count(), 1);
            drop(advertiser);

            let receiver =
                Receiver::new(&ndi, &ReceiverOptions::new(Source::new("").unwrap())).unwrap();
            let mut advertiser = RecvAdvertiser::new(&ndi, &server()).unwrap();
            let registration = advertiser
                .register(&receiver, &RecvAdvertisement::default())
                .unwrap();
            drop(receiver);
            std::env::set_var("GRAFTON_FAKE_ADVERTISER_REMOVE", "refuse");
            let rejected = advertiser.unregister(registration).unwrap_err();
            assert!(matches!(
                rejected.error(),
                Error::NativeOperationFailed {
                    operation} if operation.name() == c"NDIlib_recv_advertiser_del_receiver"
            ));
            assert!(!read_fake_log().contains("receiver-destroy\n"));
            std::env::remove_var("GRAFTON_FAKE_ADVERTISER_REMOVE");
            // Dropping the advertiser ends the registration that remained.
            drop(advertiser);
            let events = read_fake_log();
            assert!(
                position(&events, "recv-advertiser-remove group=null")
                    < position(&events, "receiver-destroy")
            );
            assert_eq!(events.matches("receiver-destroy\n").count(), 1);
        }
        "malformed_metadata" => {
            let sender = sender(&ndi);
            assert!(sender.capture_metadata(Duration::ZERO).is_err());
            assert_eq!(read_fake_log().matches("sender-metadata-free\n").count(), 1);
            assert!(read_fake_log().contains("sender-metadata-free-count=2\n"));
            drop(sender);
        }
        "metadata_count" => {
            std::env::set_var("GRAFTON_FAKE_SENDER_METADATA_COUNT", "exclude-nul");
            let sender = sender(&ndi);
            let frame = sender.capture_metadata(Duration::ZERO).unwrap().unwrap();
            assert_eq!(frame.data(), "x");
            assert_eq!(frame.timecode().ticks(), 123);
            let events = read_fake_log();
            assert_eq!(events.matches("sender-metadata-free\n").count(), 1);
            assert!(events.contains("sender-metadata-free-count=1\n"));
            drop(sender);
            assert_eq!(frame.data(), "x", "the copy outlives the SDK allocation");
        }
        #[cfg(feature = "advanced")]
        "queue_immediate" => {
            use grafton_ndi::{
                advanced::{QueueLimits, QueuedSender},
                QueueLimit,
            };
            let mut queue = QueuedSender::new(
                &ndi,
                &SenderOptions::new("queue"),
                &QueueLimits::default().max_frames(1),
            )
            .unwrap();
            let id = queue.try_submit(frame()).unwrap();
            assert_eq!(queue.retained_frames(), 1);
            let rejected = queue.try_submit(frame()).unwrap_err();
            let (error, rejected_frame) = rejected.into_parts();
            assert!(matches!(error, Error::QueueFull(QueueLimit::Frames)));
            let completed = queue.next_completed(Duration::ZERO).unwrap().unwrap();
            assert_eq!(completed.id(), id);
            assert_eq!(queue.retained_frames(), 0);
            let next = queue.try_submit(rejected_frame).unwrap();
            assert_ne!(next, id);
            assert!(!queue.completion_protocol_failed());
            drop(queue);
            assert_eq!(read_fake_log().matches("sender-completion\n").count(), 2);
            assert_eq!(read_fake_log().matches("sender-submit-return\n").count(), 2);
            assert_eq!(read_fake_log().matches("sender-destroy\n").count(), 1);
            // A frame larger than the byte limit is handed back unsent.
            let mut small = QueuedSender::new(
                &ndi,
                &SenderOptions::new("queue"),
                &QueueLimits::default().max_bytes(1),
            )
            .unwrap();
            let rejected = small.try_submit(frame()).unwrap_err();
            assert!(matches!(rejected.error(), Error::InvalidInput(_)));
            assert_eq!(small.retained_frames(), 0);
            assert_eq!(read_fake_log().matches("sender-submit-return\n").count(), 2);

            // Compressed metadata retains its caller's string allocation;
            // unused capacity must count towards the queue limit too.
            use grafton_ndi::advanced::{CompressedVideoFrame, VideoCodec, VideoPacketFormat};
            let mut metadata = String::with_capacity(1024);
            metadata.push_str("<pooled/>");
            let metadata_pointer = metadata.as_ptr();
            let mut packet = CompressedVideoFrame::new(
                VideoPacketFormat::new(VideoCodec::SpeedHq422, 16, 16).unwrap(),
                vec![0; 4],
            )
            .unwrap();
            packet.set_metadata(metadata).unwrap();
            let mut bounded = QueuedSender::new(
                &ndi,
                &SenderOptions::new("queue metadata capacity"),
                &QueueLimits::default().max_bytes(512),
            )
            .unwrap();
            let rejected = bounded.try_submit(packet).unwrap_err();
            assert!(matches!(rejected.error(), Error::InvalidInput(_)));
            let grafton_ndi::advanced::QueuedVideoFrame::Compressed(packet) = rejected.into_inner()
            else {
                panic!("the rejected frame changed type");
            };
            assert_eq!(packet.metadata().unwrap().as_ptr(), metadata_pointer);
            assert_eq!(bounded.retained_frames(), 0);
            assert_eq!(read_fake_log().matches("sender-submit-return\n").count(), 2);

            let packet = |capacity| {
                let mut bytes = Vec::with_capacity(capacity);
                bytes.extend_from_slice(&[0; 4]);
                CompressedVideoFrame::new(
                    VideoPacketFormat::new(VideoCodec::SpeedHq422, 16, 16).unwrap(),
                    bytes,
                )
                .unwrap()
            };
            let mut exact = QueuedSender::new(
                &ndi,
                &SenderOptions::new("queue exact capacity"),
                &QueueLimits::default().max_bytes(4),
            )
            .unwrap();
            let first = exact.try_submit(packet(4)).unwrap();
            let rejected = exact.try_submit(packet(4)).unwrap_err();
            assert!(matches!(
                rejected.error(),
                Error::QueueFull(QueueLimit::Bytes)
            ));
            let pooled = packet(1024);
            let original = pooled.data().as_ptr();
            let refused = exact.try_submit(pooled).unwrap_err();
            assert!(matches!(refused.error(), Error::InvalidInput(_)));
            let grafton_ndi::advanced::QueuedVideoFrame::Compressed(pooled) = refused.into_inner()
            else {
                panic!("the rejected frame changed type");
            };
            assert_eq!(pooled.data().as_ptr(), original);
            assert_eq!(exact.retained_frames(), 1);
            assert_eq!(read_fake_log().matches("sender-submit-return\n").count(), 3);
            assert_eq!(
                exact.next_completed(Duration::ZERO).unwrap().unwrap().id(),
                first
            );
            exact.try_submit(rejected.into_inner()).unwrap();
            assert_eq!(read_fake_log().matches("sender-submit-return\n").count(), 4);
        }
        #[cfg(feature = "advanced")]
        "queue_scatter_immediate"
        | "queue_scatter_delayed"
        | "queue_scatter_protocol"
        | "queue_scatter_unavailable" => {
            use grafton_ndi::advanced::{
                QueueLimits, QueuedSender, QueuedVideoFrame, ScatterVideoFrame, VideoCodec,
                VideoPacketFormat,
            };
            let packet = || {
                ScatterVideoFrame::new(
                    VideoPacketFormat::new(VideoCodec::SpeedHq422, 16, 16).unwrap(),
                    vec![vec![0, 1, 2, 3], vec![4, 5, 6, 7]],
                )
                .unwrap()
            };
            let mut queue = QueuedSender::new(
                &ndi,
                &SenderOptions::new("scatter queue"),
                &QueueLimits::default(),
            )
            .unwrap();
            if case == "queue_scatter_unavailable" {
                let frame = packet();
                let original = frame
                    .blocks()
                    .map(|block| block.as_ptr())
                    .collect::<Vec<_>>();
                let rejected = queue.try_submit(frame).unwrap_err();
                assert!(matches!(
                    rejected.error(),
                    Error::UnsupportedCapability {
                        capability: grafton_ndi::Capability::Runtime(RuntimeFeature::ScatterSend),
                        ..
                    }
                ));
                let QueuedVideoFrame::Scatter(frame) = rejected.into_inner() else {
                    panic!("the frame changed type")
                };
                assert_eq!(
                    frame
                        .blocks()
                        .map(|block| block.as_ptr())
                        .collect::<Vec<_>>(),
                    original
                );
                assert!(!read_fake_log().contains("sender-scatter-submit"));
            } else if case == "queue_scatter_protocol" {
                std::env::set_var("GRAFTON_FAKE_SCATTER_KEY", "altered");
                let frame = packet();
                let original = frame
                    .blocks()
                    .map(|block| block.as_ptr())
                    .collect::<Vec<_>>();
                let id = queue.try_submit(frame).unwrap();
                assert!(queue.completion_protocol_failed());
                assert_eq!(queue.retained_frames(), 1);
                assert!(queue.try_next_completed().is_none());
                assert!(matches!(
                    queue.try_submit(packet()).unwrap_err().error(),
                    Error::UnexpectedSdkResult(_)
                ));
                assert!(matches!(
                    queue.next_completed(Duration::ZERO),
                    Err(Error::UnexpectedSdkResult(_))
                ));
                let mut reclaimed = queue.close();
                assert_eq!(reclaimed.len(), 1);
                let completed = reclaimed.pop().unwrap();
                assert_eq!(completed.id(), id);
                let QueuedVideoFrame::Scatter(frame) = completed.into_frame() else {
                    panic!("the frame changed type")
                };
                assert_eq!(
                    frame
                        .blocks()
                        .map(|block| block.as_ptr())
                        .collect::<Vec<_>>(),
                    original
                );
                assert_eq!(read_fake_log().matches("sender-destroy\n").count(), 1);
            } else {
                let mut submitted = Vec::new();
                for _ in 0..3 {
                    let frame = packet();
                    let original = frame
                        .blocks()
                        .map(|block| block.as_ptr() as usize)
                        .collect::<Vec<_>>();
                    let id = queue.try_submit(frame).unwrap();
                    submitted.push((id, original));
                }
                if case == "queue_scatter_delayed" {
                    assert!(queue.try_next_completed().is_none());
                    std::env::set_var("GRAFTON_FAKE_CALLBACK_MODE", "drain");
                    let frame = packet();
                    let original = frame
                        .blocks()
                        .map(|block| block.as_ptr() as usize)
                        .collect::<Vec<_>>();
                    let id = queue.try_submit(frame).unwrap();
                    submitted.push((id, original));
                }
                for _ in 0..submitted.len() {
                    let done = queue.next_completed(Duration::ZERO).unwrap().unwrap();
                    let original = &submitted.iter().find(|(id, _)| *id == done.id()).unwrap().1;
                    let QueuedVideoFrame::Scatter(frame) = done.into_frame() else {
                        panic!("the frame changed type")
                    };
                    let blocks = frame.into_blocks();
                    assert_eq!(
                        blocks
                            .iter()
                            .map(|block| block.as_ptr() as usize)
                            .collect::<Vec<_>>(),
                        *original
                    );
                    assert_eq!(blocks, [vec![0, 1, 2, 3], vec![4, 5, 6, 7]]);
                }
                assert_eq!(queue.retained_frames(), 0);
                assert!(!queue.completion_protocol_failed());
            }
        }
        #[cfg(feature = "advanced")]
        "queue_split" => {
            use grafton_ndi::{
                advanced::{QueueLimits, QueuedSender},
                AudioFrame,
            };
            use std::sync::{
                atomic::{AtomicBool, Ordering},
                Barrier,
            };
            let mut queue = QueuedSender::new(
                &ndi,
                &SenderOptions::new("split queue"),
                &QueueLimits::default(),
            )
            .unwrap();
            queue.try_submit(frame()).unwrap();
            let audio = AudioFrame::builder()
                .num_samples(480)
                .num_channels(2)
                .build()
                .unwrap();
            let started = Barrier::new(2);
            let finished = AtomicBool::new(false);
            let (mut video, sender) = queue.split();
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    started.wait();
                    assert!(video
                        .next_completed(Duration::from_millis(250))
                        .unwrap()
                        .is_none());
                    finished.store(true, Ordering::Release);
                });
                scope.spawn(|| {
                    started.wait();
                    sender.send_audio(&audio);
                    assert!(!finished.load(Ordering::Acquire));
                    assert_eq!(read_fake_log().matches("sender-audio\n").count(), 1);
                });
            });
            drop(queue);
            let events = read_fake_log();
            assert!(position(&events, "sender-audio") < position(&events, "sender-destroy-begin"));
            assert_eq!(events.matches("sender-completion\n").count(), 1);
        }
        #[cfg(feature = "advanced")]
        "queue_backlog" => {
            use grafton_ndi::advanced::{
                CompressedVideoFrame, QueueLimits, QueuedSender, QueuedVideoFrame, VideoCodec,
                VideoPacketFormat,
            };
            let pixels = QueuedVideoFrame::from(frame());
            let packet = QueuedVideoFrame::from(
                CompressedVideoFrame::new(
                    VideoPacketFormat::new(VideoCodec::SpeedHq422, 16, 16).unwrap(),
                    vec![0, 1, 2, 3],
                )
                .unwrap(),
            );
            let address = |frame: &QueuedVideoFrame| match frame {
                QueuedVideoFrame::Pixels(frame) => frame.data().as_ptr(),
                QueuedVideoFrame::Compressed(frame) => frame.data().as_ptr(),
                _ => unreachable!("the test constructs only pixels and packets"),
            };
            let mut queue = QueuedSender::new(
                &ndi,
                &SenderOptions::new("queue backlog"),
                &QueueLimits::new().max_frames(64),
            )
            .unwrap();
            let mut submitted = Vec::new();
            let mut addresses = std::collections::HashSet::new();
            for _ in 0..32 {
                for original in [&pixels, &packet] {
                    let frame = original.clone();
                    let pointer = address(&frame);
                    assert!(!pointer.is_null());
                    assert_ne!(pointer, address(original));
                    assert!(addresses.insert(pointer));
                    let id = queue.try_submit(frame).unwrap();
                    submitted.push((id, pointer));
                }
            }
            // Immediate callbacks have moved all 64 entries into ready; none
            // has been collected, so their allocations must still be distinct.
            assert_eq!(queue.retained_frames(), 64);
            let mut resubmitted = Vec::new();
            for (id, pointer) in submitted {
                let completed = queue.try_next_completed().unwrap();
                assert_eq!(completed.id(), id);
                assert_eq!(address(completed.frame()), pointer);
                let next = queue.try_submit(completed.into_frame()).unwrap();
                assert!(next.get() > id.get());
                resubmitted.push((next, pointer));
            }
            for (id, pointer) in resubmitted {
                let completed = queue.try_next_completed().unwrap();
                assert_eq!(completed.id(), id);
                assert_eq!(address(completed.frame()), pointer);
            }
            assert_eq!(queue.retained_frames(), 0);
            assert!(!queue.completion_protocol_failed());
            drop(queue);
            assert_eq!(read_fake_log().matches("sender-completion\n").count(), 128);
            assert_eq!(
                read_fake_log().matches("sender-submit-return\n").count(),
                128
            );
        }
        #[cfg(feature = "advanced")]
        "queue_destroy" | "queue_close" => {
            use grafton_ndi::advanced::{QueueLimits, QueuedSender, QueuedVideoFrame};
            let mut queue =
                QueuedSender::new(&ndi, &SenderOptions::new("queue"), &QueueLimits::default())
                    .unwrap();
            let original = [frame(), frame()].map(|frame| {
                let pointer = frame.data().as_ptr();
                (queue.try_submit(frame).unwrap(), pointer)
            });
            assert!(queue
                .next_completed(Duration::from_millis(1))
                .unwrap()
                .is_none());
            assert_eq!(queue.retained_frames(), 2);
            if case == "queue_close" {
                let reclaimed = queue.close();
                assert_eq!(reclaimed.len(), original.len());
                // This fixture completes pending frames most recent first.
                for (completed, (id, pointer)) in
                    reclaimed.into_iter().zip(original.into_iter().rev())
                {
                    assert_eq!(completed.id(), id);
                    let QueuedVideoFrame::Pixels(frame) = completed.into_frame() else {
                        panic!("the frame changed type")
                    };
                    assert_eq!(frame.data().as_ptr(), pointer);
                }
            } else {
                drop(queue);
            }
            let events = read_fake_log();
            assert!(
                events.find("sender-destroy-begin\n").unwrap()
                    < events.find("sender-completion\n").unwrap()
            );
            assert!(
                events.rfind("sender-completion\n").unwrap()
                    < events.find("sender-destroy\n").unwrap()
            );
            assert_eq!(events.matches("sender-completion\n").count(), 2);
            assert_eq!(events.matches("sender-destroy\n").count(), 1);
        }
        #[cfg(feature = "advanced")]
        "queue_registered_sender" => {
            use grafton_ndi::advanced::{QueueLimits, QueuedSender};
            let mut sender = sender(&ndi);
            let original = frame();
            let pixels = original.data().as_ptr();
            assert!(sender.submit_video(original).unwrap().is_none());
            let mut advertiser = SendAdvertiser::new(&ndi, &server()).unwrap();
            let registration = advertiser
                .register(&sender, &SendAdvertisement::default())
                .unwrap();
            // A registered sender cannot become a queue: it comes back as it was.
            let mut sender = QueuedSender::from_sender(sender, &QueueLimits::default())
                .expect_err("a registered sender is refused")
                .into_inner();
            assert!(!read_fake_log().contains("sender-flush\n"));
            assert_eq!(read_fake_log().matches("sender-submit\n").count(), 1);
            // The refusal did not reclaim or replace the original native loan.
            assert_eq!(sender.reclaim_video().unwrap().data().as_ptr(), pixels);
            assert!(sender.reclaim_video().is_none());
            let retained = frame();
            let retained_pixels = retained.data().as_ptr();
            assert!(sender.submit_video(retained).unwrap().is_none());
            advertiser.unregister(registration).unwrap();
            let (queue, earlier) =
                QueuedSender::from_sender(sender, &QueueLimits::default()).unwrap();
            assert_eq!(earlier.unwrap().data().as_ptr(), retained_pixels);
            assert_eq!(read_fake_log().matches("sender-flush\n").count(), 2);
            drop(queue);
            drop(advertiser);
            assert_eq!(read_fake_log().matches("sender-destroy\n").count(), 1);
        }
        #[cfg(feature = "advanced")]
        "queue_sender_conversion" => {
            use grafton_ndi::advanced::{QueueLimits, QueuedSender, QueuedVideoFrame};
            let mut sender = sender(&ndi);
            let original = frame();
            let pixels = original.data().as_ptr();
            assert!(sender.submit_video(original).unwrap().is_none());
            let (mut queue, earlier) =
                QueuedSender::from_sender(sender, &QueueLimits::default()).unwrap();
            let earlier = earlier.unwrap();
            assert_eq!(earlier.data().as_ptr(), pixels);
            assert_eq!(read_fake_log().matches("sender-flush\n").count(), 1);
            assert!(!read_fake_log().contains("sender-destroy\n"));
            queue.try_submit(earlier).unwrap();
            let events = read_fake_log();
            assert!(position(&events, "sender-flush") < events.rfind("sender-submit\n").unwrap());
            let completed = queue.close();
            assert_eq!(completed.len(), 1);
            let QueuedVideoFrame::Pixels(recovered) = completed[0].frame() else {
                panic!("the recovered frame changed type")
            };
            assert_eq!(recovered.data().as_ptr(), pixels);
            let events = read_fake_log();
            assert_eq!(events.matches("sender-flush\n").count(), 1);
            assert_eq!(events.matches("sender-destroy\n").count(), 1);
        }
        _ => panic!("unknown fixture scenario"),
    }
    drop(ndi);
    assert!(!Ndi::is_running());
}
