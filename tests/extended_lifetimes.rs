//! Test-owned native fixtures exercise ownership order without SDK discovery.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

#[path = "support/fixture.rs"]
mod fixture;

use fixture::FakeRuntime;
use grafton_ndi::{
    capabilities::SdkEdition,
    discovery::{
        AdvertiserOptions, RecvAdvertisement, RecvAdvertiser, SendAdvertisement, SendAdvertiser,
    },
    runtime::RuntimeOptions,
    Receiver, ReceiverOptions, Sender, SenderOptions, Source, VideoFrame, NDI,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

/// Build a fake runtime whose senders, receivers and advertisers log their
/// lifecycle. It reports the first version whose advertisers are usable.
fn fixture(directory: &Path, advanced: bool) -> PathBuf {
    let runtime = FakeRuntime::new().with("fake_objects").version("6.4.0.0");
    if advanced {
        runtime
            .with("fake_advanced")
            .build(&directory.join("libndi_advanced.so"))
    } else {
        runtime.build(&directory.join("libndi.so"))
    }
}
fn log() -> String {
    fs::read_to_string(std::env::var_os("GRAFTON_FAKE_NDI_LOG").unwrap()).unwrap()
}
/// Where the fake runtime logged `event`.
fn position(events: &str, event: &str) -> usize {
    events
        .find(&format!("{event}\n"))
        .unwrap_or_else(|| panic!("`{event}` was not logged: {events}"))
}
fn frame() -> VideoFrame {
    VideoFrame::builder().resolution(2, 2).build().unwrap()
}
fn sender(ndi: &NDI) -> Sender {
    Sender::new(ndi, &SenderOptions::builder("fixture").build()).unwrap()
}

#[test]
fn native_resource_ownership() {
    if let Ok(case) = std::env::var("GRAFTON_OWNERSHIP_CHILD") {
        child(&case);
        return;
    }
    let directory =
        std::env::temp_dir().join(format!("grafton-ndi-ownership-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let standard = fixture(&directory, false);
    let advanced = fixture(&directory, true);
    let mut scenarios = vec![
        "sender_registration",
        "dropped_sender_advertiser",
        "receiver_registration",
        "dropped_receiver_advertiser",
        "foreign_registration",
        "malformed_metadata",
    ];
    if cfg!(all(feature = "advanced", ndi_has_advanced_header)) {
        scenarios.extend([
            "queue_immediate",
            "queue_destroy",
            "queue_registered_sender",
        ]);
    }
    for case in scenarios {
        let result = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "native_resource_ownership", "--nocapture"])
            .env("GRAFTON_OWNERSHIP_CHILD", case)
            .env(
                "GRAFTON_OWNERSHIP_LIBRARY",
                if case.starts_with("queue_") {
                    &advanced
                } else {
                    &standard
                },
            )
            .env(
                "GRAFTON_FAKE_CALLBACK_MODE",
                if case == "queue_immediate" {
                    "immediate"
                } else {
                    "delayed"
                },
            )
            .env(
                "GRAFTON_FAKE_NDI_LOG",
                directory.join(format!("{case}.log")),
            )
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{case}: {}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fs::remove_dir_all(directory).unwrap();
}
fn child(case: &str) {
    let advanced = case.starts_with("queue_");
    let path = PathBuf::from(std::env::var_os("GRAFTON_OWNERSHIP_LIBRARY").unwrap());
    let ndi = NDI::with_options(&RuntimeOptions::default().library_path(path).edition(
        if advanced {
            SdkEdition::Advanced
        } else {
            SdkEdition::Standard
        },
    ))
    .unwrap();
    match case {
        "sender_registration" => {
            let mut sender = sender(&ndi);
            let mut advertiser = SendAdvertiser::new(&ndi, &AdvertiserOptions::default()).unwrap();
            let registration = advertiser
                .register(&sender, SendAdvertisement::default())
                .unwrap();
            assert!(advertiser
                .register(&sender, SendAdvertisement::default())
                .is_err());
            sender.send_video_async(frame());
            drop(sender);
            assert!(log().contains("sender-flush\n"));
            // The registration, not the dropped handle, keeps the sender alive.
            assert!(!log().contains("sender-destroy\n"));
            assert!(advertiser.unregister(registration));
            let events = log();
            assert!(
                position(&events, "send-advertiser-remove") < position(&events, "sender-destroy")
            );
            assert!(!events.contains("send-advertiser-destroy\n"));
            drop(advertiser);
            let events = log();
            assert_eq!(events.matches("send-advertiser-add\n").count(), 1);
            assert_eq!(events.matches("send-advertiser-remove\n").count(), 1);
            assert_eq!(events.matches("sender-destroy\n").count(), 1);
            assert_eq!(events.matches("send-advertiser-destroy\n").count(), 1);
        }
        "dropped_sender_advertiser" => {
            let sender = sender(&ndi);
            let mut advertiser = SendAdvertiser::new(&ndi, &AdvertiserOptions::default()).unwrap();
            // Discarding the registration leaves the sender advertised.
            let _ = advertiser
                .register(&sender, SendAdvertisement::default().allow_monitoring(true))
                .unwrap();
            drop(sender);
            let events = log();
            assert!(events.contains("send-advertiser-add-monitoring\n"));
            assert!(!events.contains("send-advertiser-remove\n"));
            assert!(!events.contains("sender-destroy\n"));
            drop(advertiser);
            // The SDK's teardown order: stop advertising the sender, destroy
            // the advertiser, and only then destroy the sender.
            let events = log();
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
                Receiver::new(&ndi, &ReceiverOptions::builder(Source::default()).build()).unwrap();
            let mut advertiser = RecvAdvertiser::new(&ndi, &AdvertiserOptions::default()).unwrap();
            let registration = advertiser
                .register(&receiver, RecvAdvertisement::default())
                .unwrap();
            assert!(advertiser
                .register(&receiver, RecvAdvertisement::default())
                .is_err());
            drop(receiver);
            assert!(!log().contains("receiver-destroy\n"));
            assert!(advertiser.unregister(registration));
            let events = log();
            assert!(
                position(&events, "recv-advertiser-remove") < position(&events, "receiver-destroy")
            );
            assert_eq!(events.matches("receiver-destroy\n").count(), 1);
            drop(advertiser);
        }
        "dropped_receiver_advertiser" => {
            let receiver =
                Receiver::new(&ndi, &ReceiverOptions::builder(Source::default()).build()).unwrap();
            let mut advertiser = RecvAdvertiser::new(&ndi, &AdvertiserOptions::default()).unwrap();
            let _ = advertiser
                .register(
                    &receiver,
                    RecvAdvertisement::default().input_group("Program"),
                )
                .unwrap();
            drop(receiver);
            assert!(!log().contains("receiver-destroy\n"));
            drop(advertiser);
            let events = log();
            assert!(
                position(&events, "recv-advertiser-remove")
                    < position(&events, "recv-advertiser-destroy")
            );
            assert!(
                position(&events, "recv-advertiser-destroy")
                    < position(&events, "receiver-destroy")
            );
            assert_eq!(events.matches("receiver-destroy\n").count(), 1);
        }
        "foreign_registration" => {
            let sender = sender(&ndi);
            let mut advertiser = SendAdvertiser::new(&ndi, &AdvertiserOptions::default()).unwrap();
            let mut other = SendAdvertiser::new(&ndi, &AdvertiserOptions::default()).unwrap();
            let registration = advertiser
                .register(&sender, SendAdvertisement::default())
                .unwrap();
            // Another advertiser neither removes the registration nor calls
            // the SDK for it, and may advertise the same sender itself.
            assert!(!other.unregister(registration));
            assert!(!log().contains("send-advertiser-remove\n"));
            let second = other
                .register(&sender, SendAdvertisement::default())
                .unwrap();
            assert!(other.unregister(second));
            drop(other);
            drop(sender);
            assert!(!log().contains("sender-destroy\n"));
            drop(advertiser);
            assert_eq!(log().matches("sender-destroy\n").count(), 1);
        }
        "malformed_metadata" => {
            let sender = sender(&ndi);
            assert!(sender.capture_metadata(Duration::ZERO).is_err());
            assert_eq!(log().matches("sender-metadata-free\n").count(), 1);
            drop(sender);
        }
        #[cfg(all(feature = "advanced", ndi_has_advanced_header))]
        "queue_immediate" => {
            use grafton_ndi::advanced::{QueueOptions, QueuedSender};
            let mut queue = QueuedSender::new(
                &ndi,
                &SenderOptions::builder("queue").build(),
                QueueOptions::default().max_frames(1),
            )
            .unwrap();
            let id = queue.try_submit(frame()).unwrap();
            assert_eq!(queue.retained_frames(), 1);
            let rejected = queue.try_submit(frame()).unwrap_err();
            let (_, rejected_frame) = rejected.into_parts();
            let completed = queue.wait_completed(Duration::ZERO).unwrap().unwrap();
            assert_eq!(completed.id(), id);
            assert_eq!(queue.retained_frames(), 0);
            let next = queue.try_submit(rejected_frame).unwrap();
            assert_ne!(next, id);
            assert!(!queue.completion_protocol_failed());
            drop(queue);
            assert_eq!(log().matches("sender-completion\n").count(), 2);
            assert_eq!(log().matches("sender-submit-return\n").count(), 2);
            assert_eq!(log().matches("sender-destroy\n").count(), 1);
        }
        #[cfg(all(feature = "advanced", ndi_has_advanced_header))]
        "queue_destroy" => {
            use grafton_ndi::advanced::{QueueOptions, QueuedSender};
            let mut queue = QueuedSender::new(
                &ndi,
                &SenderOptions::builder("queue").build(),
                QueueOptions::default(),
            )
            .unwrap();
            queue.try_submit(frame()).unwrap();
            queue.try_submit(frame()).unwrap();
            assert!(queue
                .wait_completed(Duration::from_millis(1))
                .unwrap()
                .is_none());
            assert_eq!(queue.retained_frames(), 2);
            drop(queue);
            let events = log();
            assert!(
                events.find("sender-destroy-begin\n").unwrap()
                    < events.find("sender-completion\n").unwrap()
            );
            assert!(
                events.rfind("sender-completion\n").unwrap()
                    < events.find("sender-destroy\n").unwrap()
            );
            assert_eq!(events.matches("sender-completion\n").count(), 2);
        }
        #[cfg(all(feature = "advanced", ndi_has_advanced_header))]
        "queue_registered_sender" => {
            use grafton_ndi::advanced::{QueueOptions, QueuedSender};
            let sender = sender(&ndi);
            let mut advertiser = SendAdvertiser::new(&ndi, &AdvertiserOptions::default()).unwrap();
            let registration = advertiser
                .register(&sender, SendAdvertisement::default())
                .unwrap();
            assert!(QueuedSender::from_sender(sender, QueueOptions::default()).is_err());
            assert!(!log().contains("sender-destroy\n"));
            assert!(advertiser.unregister(registration));
            drop(advertiser);
            assert_eq!(log().matches("sender-destroy\n").count(), 1);
        }
        _ => panic!("unknown fixture scenario"),
    }
    drop(ndi);
    assert!(!NDI::is_running());
}
