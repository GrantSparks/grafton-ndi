//! Test-owned native fixtures exercise ownership order without SDK discovery.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

mod common;
#[path = "support/fixture.rs"]
mod fixture;

use fixture::FakeRuntime;
use grafton_ndi::{
    capabilities::{RuntimeFeature, SdkEdition},
    discovery::{
        AdvertiserOptions, RecvAdvertisement, RecvAdvertiser, SendAdvertisement, SendAdvertiser,
    },
    runtime::RuntimeOptions,
    Error, Ndi, Receiver, ReceiverOptions, Sender, SenderOptions, Source, VideoFrame,
};
use std::{
    fs,
    io::Write,
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
    AdvertiserOptions::default().servers("192.0.2.1")
}
fn frame() -> VideoFrame {
    VideoFrame::builder().resolution(2, 2).build().unwrap()
}
fn sender(ndi: &Ndi) -> Sender {
    Sender::new(ndi, &SenderOptions::new("fixture")).unwrap()
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
        "refused_unregistration",
        "malformed_metadata",
    ];
    if cfg!(feature = "advanced") {
        scenarios.extend([
            "queue_immediate",
            "queue_destroy",
            "queue_registered_sender",
        ]);
    }
    let mut explained = false;
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
        let errors = String::from_utf8_lossy(&result.stderr);
        assert!(
            result.status.success(),
            "{case}: {}\n{errors}",
            String::from_utf8_lossy(&result.stdout),
        );
        // A scenario that could not run says so on its own standard error,
        // which the harness shows only for a failure: pass the notice on, and
        // the reason before the first one.
        let (notices, reasons): (Vec<_>, Vec<_>) = errors
            .lines()
            .filter(|line| !line.is_empty())
            .partition(|line| line.starts_with("skipped:"));
        let mut stderr = std::io::stderr().lock();
        for notice in notices {
            if !std::mem::replace(&mut explained, true) {
                for reason in &reasons {
                    let _ = writeln!(stderr, "{reason}");
                }
            }
            let _ = writeln!(stderr, "{notice} (scenario {case})");
        }
    }
    fs::remove_dir_all(directory).unwrap();
}
fn child(case: &str) {
    let advanced = case.starts_with("queue_");
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
    match case {
        "sender_registration" => {
            let mut sender = sender(&ndi);
            let mut advertiser = SendAdvertiser::new(&ndi, &server()).unwrap();
            let registration = advertiser
                .register(&sender, SendAdvertisement::default())
                .unwrap();
            assert!(advertiser
                .register(&sender, SendAdvertisement::default())
                .is_err());
            sender.send_video_async(frame()).unwrap();
            drop(sender);
            assert!(log().contains("sender-flush\n"));
            // The registration, not the dropped handle, keeps the sender alive.
            assert!(!log().contains("sender-destroy\n"));
            advertiser.unregister(registration).unwrap();
            let events = log();
            assert!(
                position(&events, "send-advertiser-remove") < position(&events, "sender-destroy")
            );
            assert!(!events.contains("send-advertiser-destroy\n"));
            drop(advertiser);
            let events = log();
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
                .register(&sender, SendAdvertisement::default().allow_monitoring(true))
                .unwrap();
            drop(sender);
            let events = log();
            assert!(events.contains("send-advertiser-add monitoring=true\n"));
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
            let receiver = Receiver::new(&ndi, &ReceiverOptions::new(Source::new(""))).unwrap();
            let mut advertiser = RecvAdvertiser::new(&ndi, &server()).unwrap();
            let registration = advertiser
                .register(&receiver, RecvAdvertisement::default())
                .unwrap();
            assert!(advertiser
                .register(&receiver, RecvAdvertisement::default())
                .is_err());
            drop(receiver);
            assert!(!log().contains("receiver-destroy\n"));
            advertiser.unregister(registration).unwrap();
            let events = log();
            assert!(
                position(&events, "recv-advertiser-remove group=null")
                    < position(&events, "receiver-destroy")
            );
            assert_eq!(events.matches("receiver-destroy\n").count(), 1);
            drop(advertiser);
        }
        "dropped_receiver_advertiser" => {
            let receiver = Receiver::new(&ndi, &ReceiverOptions::new(Source::new(""))).unwrap();
            let mut advertiser = RecvAdvertiser::new(&ndi, &server()).unwrap();
            let _ = advertiser
                .register(
                    &receiver,
                    RecvAdvertisement::default().input_group("Program"),
                )
                .unwrap();
            drop(receiver);
            assert!(!log().contains("receiver-destroy\n"));
            drop(advertiser);
            // The input-group name is still intact when the registration
            // ends, which is when this runtime reads it.
            let events = log();
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
                .register(&sender, SendAdvertisement::default())
                .unwrap();
            // Another advertiser neither ends the registration nor calls the
            // SDK for it: it hands the registration back with the reason.
            let rejected = other.unregister(registration).unwrap_err();
            assert!(matches!(rejected.error(), Error::InvalidInput(_)));
            assert!(!log().contains("send-advertiser-remove\n"));
            let registration = rejected.into_inner();
            // It may advertise the same sender itself.
            let second = other
                .register(&sender, SendAdvertisement::default())
                .unwrap();
            other.unregister(second).unwrap();
            drop(other);
            drop(sender);
            assert!(!log().contains("sender-destroy\n"));
            // The registration that came back still ends where it was issued.
            advertiser.unregister(registration).unwrap();
            assert_eq!(log().matches("sender-destroy\n").count(), 1);
            drop(advertiser);

            let receiver = Receiver::new(&ndi, &ReceiverOptions::new(Source::new(""))).unwrap();
            let mut advertiser = RecvAdvertiser::new(&ndi, &server()).unwrap();
            let mut other = RecvAdvertiser::new(&ndi, &server()).unwrap();
            let registration = advertiser
                .register(&receiver, RecvAdvertisement::default())
                .unwrap();
            let rejected = other.unregister(registration).unwrap_err();
            assert!(!log().contains("recv-advertiser-remove"));
            drop(receiver);
            advertiser.unregister(rejected.into_inner()).unwrap();
            assert_eq!(log().matches("receiver-destroy\n").count(), 1);
        }
        "refused_unregistration" => {
            let sender = sender(&ndi);
            let mut advertiser = SendAdvertiser::new(&ndi, &server()).unwrap();
            let registration = advertiser
                .register(&sender, SendAdvertisement::default())
                .unwrap();
            drop(sender);
            // An SDK that removes nothing fails the call, which changes
            // nothing: the registration comes back and keeps the sender alive.
            std::env::set_var("GRAFTON_FAKE_ADVERTISER_REMOVE", "refuse");
            let rejected = advertiser.unregister(registration).unwrap_err();
            assert!(matches!(
                rejected.error(),
                Error::NativeOperationFailed {
                    operation: "unregister sender"
                }
            ));
            assert!(!log().contains("sender-destroy\n"));
            std::env::remove_var("GRAFTON_FAKE_ADVERTISER_REMOVE");
            // The registration that came back can still be ended.
            advertiser.unregister(rejected.into_inner()).unwrap();
            assert_eq!(log().matches("sender-destroy\n").count(), 1);
            drop(advertiser);

            let receiver = Receiver::new(&ndi, &ReceiverOptions::new(Source::new(""))).unwrap();
            let mut advertiser = RecvAdvertiser::new(&ndi, &server()).unwrap();
            let registration = advertiser
                .register(&receiver, RecvAdvertisement::default())
                .unwrap();
            drop(receiver);
            std::env::set_var("GRAFTON_FAKE_ADVERTISER_REMOVE", "refuse");
            let rejected = advertiser.unregister(registration).unwrap_err();
            assert!(matches!(
                rejected.error(),
                Error::NativeOperationFailed {
                    operation: "unregister receiver"
                }
            ));
            assert!(!log().contains("receiver-destroy\n"));
            std::env::remove_var("GRAFTON_FAKE_ADVERTISER_REMOVE");
            // Dropping the advertiser ends the registration that remained.
            drop(advertiser);
            let events = log();
            assert!(
                position(&events, "recv-advertiser-remove group=null")
                    < position(&events, "receiver-destroy")
            );
            assert_eq!(events.matches("receiver-destroy\n").count(), 1);
        }
        "malformed_metadata" => {
            let sender = sender(&ndi);
            assert!(sender.capture_metadata(Duration::ZERO).is_err());
            assert_eq!(log().matches("sender-metadata-free\n").count(), 1);
            drop(sender);
        }
        #[cfg(feature = "advanced")]
        "queue_immediate" => {
            use grafton_ndi::advanced::{QueueOptions, QueuedSender};
            let mut queue = QueuedSender::new(
                &ndi,
                &SenderOptions::new("queue"),
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
        #[cfg(feature = "advanced")]
        "queue_destroy" => {
            use grafton_ndi::advanced::{QueueOptions, QueuedSender};
            let mut queue =
                QueuedSender::new(&ndi, &SenderOptions::new("queue"), QueueOptions::default())
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
        #[cfg(feature = "advanced")]
        "queue_registered_sender" => {
            use grafton_ndi::advanced::{QueueOptions, QueuedSender};
            let sender = sender(&ndi);
            let mut advertiser = SendAdvertiser::new(&ndi, &server()).unwrap();
            let registration = advertiser
                .register(&sender, SendAdvertisement::default())
                .unwrap();
            // A registered sender cannot become a queue: it comes back as it was.
            let sender = QueuedSender::from_sender(sender, QueueOptions::default())
                .err()
                .expect("a registered sender is refused")
                .into_inner();
            sender.send_video(&frame()).unwrap();
            drop(sender);
            assert!(!log().contains("sender-destroy\n"));
            advertiser.unregister(registration).unwrap();
            drop(advertiser);
            assert_eq!(log().matches("sender-destroy\n").count(), 1);
        }
        _ => panic!("unknown fixture scenario"),
    }
    drop(ndi);
    assert!(!Ndi::is_running());
}
