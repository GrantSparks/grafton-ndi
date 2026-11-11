//! Test-owned native fixtures exercise ownership order without SDK discovery.
#![cfg(all(target_os = "linux", ndi_has_discovery_header, not(ndi_link_static)))]

#[path = "support/fixture_cc.rs"]
mod fixture_cc;
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

fn fixture(directory: &Path, advanced: bool) -> PathBuf {
    let sdk = std::env::var_os("NDI_SDK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/share/NDI SDK for Linux"));
    let library = directory.join(if advanced {
        "libndi_advanced.so"
    } else {
        "libndi.so"
    });
    let include = if sdk.join("include").is_dir() {
        sdk.join("include")
    } else {
        sdk.join("Include")
    };
    let mut command = fixture_cc::command();
    command
        .args(["-std=c11", "-shared", "-fPIC", "-DFAKE_OBJECTS"])
        .arg("-I")
        .arg(include)
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/fake_sdk.c"
        ))
        .arg("-o")
        .arg(&library);
    if advanced {
        command.arg("-DFAKE_ADVANCED");
    }
    let result = command.output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    library
}
fn log() -> String {
    fs::read_to_string(std::env::var_os("GRAFTON_FAKE_NDI_LOG").unwrap()).unwrap()
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
        "forgotten_sender",
        "receiver_registration",
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
            let advertiser = SendAdvertiser::new(&ndi, &AdvertiserOptions::default()).unwrap();
            let registration = advertiser
                .register(&sender, SendAdvertisement::default())
                .unwrap();
            assert!(advertiser
                .register(&sender, SendAdvertisement::default())
                .is_err());
            sender.send_video_async(frame());
            drop(sender);
            assert!(log().contains("sender-flush\n"));
            assert!(!log().contains("sender-destroy\n"));
            drop(advertiser);
            assert!(!log().contains("send-advertiser-destroy\n"));
            drop(registration);
            let events = log();
            assert!(
                events.find("send-advertiser-remove\n").unwrap()
                    < events.find("sender-destroy\n").unwrap()
            );
            assert_eq!(events.matches("send-advertiser-add\n").count(), 1);
            assert_eq!(events.matches("sender-destroy\n").count(), 1);
            assert_eq!(events.matches("send-advertiser-destroy\n").count(), 1);
        }
        "forgotten_sender" => {
            let sender = sender(&ndi);
            let advertiser = SendAdvertiser::new(&ndi, &AdvertiserOptions::default()).unwrap();
            let registration = advertiser
                .register(&sender, SendAdvertisement::default())
                .unwrap();
            std::mem::forget(registration);
            drop(sender);
            drop(advertiser);
            drop(ndi);
            let events = log();
            assert!(!events.contains("sender-destroy\n"));
            assert!(!events.contains("send-advertiser-destroy\n"));
            assert!(NDI::is_running());
            return;
        }
        "receiver_registration" => {
            let receiver =
                Receiver::new(&ndi, &ReceiverOptions::builder(Source::default()).build()).unwrap();
            let advertiser = RecvAdvertiser::new(&ndi, &AdvertiserOptions::default()).unwrap();
            let registration = advertiser
                .register(&receiver, RecvAdvertisement::default())
                .unwrap();
            assert!(advertiser
                .register(&receiver, RecvAdvertisement::default())
                .is_err());
            drop(receiver);
            assert!(!log().contains("receiver-destroy\n"));
            assert!(registration.unregister().unwrap());
            assert_eq!(log().matches("receiver-destroy\n").count(), 1);
            drop(advertiser);
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
            let advertiser = SendAdvertiser::new(&ndi, &AdvertiserOptions::default()).unwrap();
            let registration = advertiser
                .register(&sender, SendAdvertisement::default())
                .unwrap();
            assert!(QueuedSender::from_sender(sender, QueueOptions::default()).is_err());
            assert!(!log().contains("sender-destroy\n"));
            drop(registration);
            drop(advertiser);
            assert_eq!(log().matches("sender-destroy\n").count(), 1);
        }
        _ => panic!("unknown fixture scenario"),
    }
    drop(ndi);
    assert!(!NDI::is_running());
}
