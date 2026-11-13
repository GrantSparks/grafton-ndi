//! Standard mixed-media capture, exercised against a scripted native boundary.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

#[path = "support/fixture.rs"]
mod fixture;

use grafton_ndi::{
    Error, FrameRate, Ndi, Receiver, ReceiverEvent, ReceiverItem, ReceiverItemRef, ReceiverOptions,
    RuntimeOptions, ScanType, Source, Timecode,
};
use std::{
    fs,
    process::Command,
    time::{Duration, Instant},
};

#[test]
fn mixed_capture_dispatch_and_lifecycle() {
    if let Some(library) = std::env::var_os("GRAFTON_MIXED_CAPTURE_CHILD") {
        exercise(std::path::PathBuf::from(library));
        return;
    }
    let directory = fixture::FixtureDir::create("mixed-capture");
    let library = directory.path().join("libndi_capture.so");
    fixture::FakeRuntime::new()
        .with("fake_objects")
        .with("fake_capture")
        .build(&library);
    let result = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "mixed_capture_dispatch_and_lifecycle",
            "--nocapture",
        ])
        .env("GRAFTON_MIXED_CAPTURE_CHILD", library)
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

fn script(serial: usize, items: &str) {
    std::env::set_var("GRAFTON_FAKE_CAPTURE_SCRIPT", format!("{serial}:{items}"));
}

fn log() -> String {
    fs::read_to_string(std::env::var_os("GRAFTON_FAKE_NDI_LOG").unwrap()).unwrap()
}

fn captures() -> usize {
    log()
        .lines()
        .filter(|line| line.starts_with("capture:"))
        .count()
}
fn frees(kind: &str) -> usize {
    log()
        .lines()
        .filter(|line| *line == format!("free-{kind}"))
        .count()
}

fn exercise(library: std::path::PathBuf) {
    let ndi = Ndi::with_options(&RuntimeOptions::new().library_path(library)).unwrap();
    let receiver = Receiver::new(&ndi, &ReceiverOptions::new(Source::new("fixture"))).unwrap();
    script(1, "video,planar,alpha,audio,metadata,status,source,unknown");
    for (kind, payload) in [("video", 32), ("planar", 24), ("alpha", 24)] {
        let before = frees("video");
        let item = receiver.capture_any_ref(Duration::ZERO).unwrap().unwrap();
        let ReceiverItemRef::Video(frame) = &item else {
            panic!("{item:?}")
        };
        assert!(frame.data().is_none(), "{kind} has native padding");
        assert_eq!(
            frame
                .planes()
                .flat_map(|plane| plane.rows())
                .flatten()
                .count(),
            payload
        );
        assert!(frame
            .planes()
            .flat_map(|plane| plane.rows())
            .flatten()
            .all(|&pixel| pixel == 37));
        assert_eq!(frame.frame_rate(), FrameRate::new(30_000, 1001).unwrap());
        assert_eq!(frame.picture_aspect_ratio(), 1.25);
        assert_eq!(frame.scan_type(), ScanType::Field1);
        assert_eq!(frame.timecode(), Timecode::from_ticks(1234));
        assert_eq!(frame.timestamp().unwrap().ticks(), 5678);
        assert_eq!(frame.metadata(), Some("<video/>"));
        let ReceiverItem::Video(owned) = item.to_owned() else {
            unreachable!()
        };
        assert_eq!(owned.data(), vec![37; payload]);
        assert_eq!(owned.picture_aspect_ratio(), 1.25);
        assert_eq!(owned.metadata(), Some("<video/>"));
        assert_eq!(owned.timecode(), frame.timecode());
        assert_eq!(frees("video"), before);
        drop(item);
        assert_eq!(frees("video"), before + 1);
    }
    let item = receiver.capture_any_ref(Duration::ZERO).unwrap().unwrap();
    let ReceiverItemRef::Audio(frame) = &item else {
        panic!("{item:?}")
    };
    assert!(frame.data().is_none());
    assert_eq!(frame.channel_data(0), Some([0.25, 0.5].as_slice()));
    assert_eq!(frame.channel_data(1), Some([-0.25, -0.5].as_slice()));
    let ReceiverItem::Audio(owned) = item.to_owned() else {
        unreachable!()
    };
    assert_eq!(owned.data(), [0.25, 0.5, -0.25, -0.5]);
    assert_eq!(owned.timecode(), Timecode::from_ticks(2345));
    assert_eq!(owned.timestamp().unwrap().ticks(), 6789);
    assert_eq!(owned.metadata(), Some("<audio/>"));
    drop(item);
    assert_eq!(frees("audio"), 1);
    let ReceiverItem::Metadata(metadata) = receiver.capture_any(Duration::ZERO).unwrap().unwrap()
    else {
        panic!("metadata variant")
    };
    assert_eq!(metadata.data(), "<metadata/>");
    assert_eq!(metadata.timecode(), Timecode::from_ticks(3456));
    assert_eq!(frees("metadata"), 1);
    for expected in [
        ReceiverEvent::StatusChanged(Default::default()),
        ReceiverEvent::SourceChanged,
        ReceiverEvent::Unknown(987),
    ] {
        let ReceiverItem::Event(event) = receiver.capture_any(Duration::ZERO).unwrap().unwrap()
        else {
            panic!("event variant")
        };
        assert_eq!(event, expected);
    }
    assert!(
        receiver.poll_event(Duration::ZERO).unwrap().is_none(),
        "returned events must not be retained again"
    );

    // Typed capture retains an invalidation and combined capture consumes it
    // without making another native call, including for a zero timeout.
    script(2, "status");
    assert!(receiver
        .audio()
        .capture_ref(Duration::ZERO)
        .unwrap()
        .is_none());
    let calls = captures();
    assert!(matches!(
        receiver.capture_any(Duration::ZERO).unwrap(),
        Some(ReceiverItem::Event(ReceiverEvent::StatusChanged(_)))
    ));
    assert_eq!(captures(), calls);
    assert!(receiver.poll_event(Duration::ZERO).unwrap().is_none());

    for (serial, token, family) in [
        (3, "bad-video", "video"),
        (4, "bad-audio", "audio"),
        (5, "bad-metadata", "metadata"),
    ] {
        script(serial, token);
        let before = frees(family);
        assert!(matches!(
            receiver.capture_any_ref(Duration::ZERO),
            Err(Error::InvalidFrame(_))
        ));
        assert_eq!(
            frees(family),
            before + 1,
            "rejected {token} must be freed once"
        );
    }
    script(6, "error");
    assert!(matches!(
        receiver.capture_any(Duration::ZERO),
        Err(Error::CaptureFailed(_))
    ));

    script(7, "none");
    let calls = captures();
    assert!(matches!(
        receiver.capture_any(Duration::MAX),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        receiver.capture_any_ref(Duration::MAX),
        Err(Error::InvalidInput(_))
    ));
    assert_eq!(captures(), calls);
    assert!(receiver.capture_any(Duration::ZERO).unwrap().is_none());
    assert_eq!(captures(), calls + 1);
    assert!(log().lines().last().unwrap().ends_with(":111:0"));

    // Immediate native misses spend the whole budget with bounded retry
    // pauses, and media arriving later does not wait for another media kind.
    script(8, "none");
    let calls = captures();
    let start = Instant::now();
    assert!(receiver
        .capture_any(Duration::from_millis(35))
        .unwrap()
        .is_none());
    assert!(start.elapsed() >= Duration::from_millis(35));
    assert!(
        captures() - calls <= 8,
        "early native returns must not spin"
    );
    script(9, "none,none,audio");
    let start = Instant::now();
    assert!(matches!(
        receiver.capture_any(Duration::from_secs(2)).unwrap(),
        Some(ReceiverItem::Audio(_))
    ));
    assert!(start.elapsed() < Duration::from_secs(1));

    #[cfg(any(feature = "tokio", feature = "smol"))]
    exercise_async(&ndi);
}

#[cfg(any(feature = "tokio", feature = "smol"))]
fn exercise_async(ndi: &Ndi) {
    async fn check<R: grafton_ndi::asynchronous::AsyncRuntime>(
        receiver: grafton_ndi::asynchronous::AsyncReceiver<R>,
        serial: usize,
    ) {
        script(serial, "video,audio,metadata,source,error");
        assert!(matches!(
            receiver.capture_any(Duration::ZERO).await.unwrap(),
            Some(ReceiverItem::Video(_))
        ));
        assert!(matches!(
            receiver.capture_any(Duration::ZERO).await.unwrap(),
            Some(ReceiverItem::Audio(_))
        ));
        assert!(matches!(
            receiver.capture_any(Duration::ZERO).await.unwrap(),
            Some(ReceiverItem::Metadata(_))
        ));
        assert!(matches!(
            receiver.capture_any(Duration::ZERO).await.unwrap(),
            Some(ReceiverItem::Event(ReceiverEvent::SourceChanged))
        ));
        assert!(matches!(
            receiver.capture_any(Duration::ZERO).await,
            Err(Error::CaptureFailed(_))
        ));
        assert!(matches!(
            receiver.capture_any(Duration::MAX).await,
            Err(Error::InvalidInput(_))
        ));
        assert!(receiver
            .capture_any(Duration::ZERO)
            .await
            .unwrap()
            .is_none());

        // Native entry is the observable proof that the cancellation gate
        // was passed. Dropping the pending future must still free the frame
        // that its worker consumes, rather than leave a borrowed guard there.
        use std::{
            future::Future,
            task::{Context, Poll, Waker},
        };
        script(serial + 100, "video");
        std::env::set_var("GRAFTON_FAKE_CAPTURE_DELAY_MS", "60");
        let before_calls = captures();
        let before_frees = frees("video");
        let mut capture = Box::pin(receiver.capture_any(Duration::from_millis(200)));
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(capture.as_mut().poll(&mut context), Poll::Pending));
        let deadline = Instant::now() + Duration::from_secs(5);
        while captures() == before_calls {
            assert!(
                Instant::now() < deadline,
                "blocking worker did not enter native capture"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        drop(capture);
        while frees("video") == before_frees {
            assert!(
                Instant::now() < deadline,
                "discarded worker item was not freed"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(frees("video"), before_frees + 1);
        std::env::remove_var("GRAFTON_FAKE_CAPTURE_DELAY_MS");
        assert!(receiver
            .capture_any(Duration::ZERO)
            .await
            .unwrap()
            .is_none());
    }
    let make = || Receiver::new(ndi, &ReceiverOptions::new(Source::new("async fixture"))).unwrap();
    #[cfg(feature = "tokio")]
    ::tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(check(
            grafton_ndi::asynchronous::tokio::AsyncReceiver::new(make()),
            20,
        ));
    #[cfg(feature = "smol")]
    ::smol::block_on(check(
        grafton_ndi::asynchronous::smol::AsyncReceiver::new(make()),
        21,
    ));
}
