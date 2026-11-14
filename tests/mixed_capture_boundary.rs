//! Standard mixed-media capture, exercised against a scripted native boundary.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

#[path = "support/fixture.rs"]
mod fixture;
#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;

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
    runtime_fixture::FakeRuntime {
        cfgs: vec!["fake_objects", "fake_capture"],
        ..Default::default()
    }
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

#[cfg(any(feature = "tokio", feature = "smol"))]
fn native_payload(kind: &str) -> usize {
    let prefix = format!("{kind}-data:");
    log()
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix(&prefix))
        .unwrap()
        .parse()
        .unwrap()
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
    async fn check<R: grafton_ndi::asynchronous::AsyncRuntime + 'static>(
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

        scoped_checks(&receiver, serial + 200).await;

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
        cancel_scoped(receiver, serial + 500, true, true).await;
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
    for (index, typed, native_entry) in [(0, false, true), (1, true, false), (2, false, false)] {
        #[cfg(feature = "tokio")]
        ::tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(cancel_scoped(
                grafton_ndi::asynchronous::tokio::AsyncReceiver::new(make()),
                1000 + index,
                typed,
                native_entry,
            ));
        #[cfg(feature = "smol")]
        ::smol::block_on(cancel_scoped(
            grafton_ndi::asynchronous::smol::AsyncReceiver::new(make()),
            2000 + index,
            typed,
            native_entry,
        ));
    }
}

#[cfg(any(feature = "tokio", feature = "smol"))]
async fn catch_future<T>(future: impl std::future::Future<Output = T>) -> std::thread::Result<T> {
    use std::{
        panic::{catch_unwind, AssertUnwindSafe},
        task::Poll,
    };
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(|context| {
        match catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(context))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(value)) => Poll::Ready(Ok(value)),
            Err(payload) => Poll::Ready(Err(payload)),
        }
    })
    .await
}

#[cfg(any(feature = "tokio", feature = "smol"))]
async fn scoped_checks<R: grafton_ndi::asynchronous::AsyncRuntime + 'static>(
    receiver: &grafton_ndi::asynchronous::AsyncReceiver<R>,
    serial: usize,
) {
    // Both typed and combined capture see active regions only, with the same
    // headers and metadata as owned capture. The callback is FnOnce, with an
    // owned capture, and runs on a different thread from this executor.
    let caller = std::thread::current().id();
    for (index, token, bytes) in [(0, "video", 32), (1, "planar", 24), (2, "alpha", 24)] {
        script(serial + index, &format!("{token},{token},{token},{token}"));
        let before = frees("video");
        let once = Box::new(bytes);
        let summary = receiver
            .video()
            .capture_with(Duration::ZERO, move |frame| {
                assert_ne!(std::thread::current().id(), caller);
                assert!(frame.data().is_none());
                assert_eq!(
                    frame
                        .planes()
                        .next()
                        .unwrap()
                        .rows()
                        .next()
                        .unwrap()
                        .as_ptr() as usize,
                    native_payload("video")
                );
                let pixels: usize = frame
                    .planes()
                    .flat_map(|p| p.rows())
                    .flatten()
                    .map(|&b| usize::from(b))
                    .sum();
                let expected = *once;
                drop(once);
                assert_eq!(pixels, expected * 37);
                (
                    frame.width(),
                    frame.height(),
                    frame.timecode(),
                    frame.timestamp(),
                    frame.metadata().unwrap().len(),
                    pixels,
                )
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(frees("video"), before + 1);
        let owned = receiver
            .video()
            .capture(Duration::ZERO)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            summary,
            (
                owned.width(),
                owned.height(),
                owned.timecode(),
                owned.timestamp(),
                owned.metadata().unwrap().len(),
                owned.data().iter().map(|&b| usize::from(b)).sum::<usize>()
            )
        );
        let mixed = receiver
            .capture_any_with(Duration::ZERO, |item| {
                let ReceiverItemRef::Video(frame) = item else {
                    panic!("{item:?}")
                };
                assert!(frame.data().is_none());
                frame.to_owned()
            })
            .await
            .unwrap()
            .unwrap();
        let ReceiverItem::Video(owned_mixed) =
            receiver.capture_any(Duration::ZERO).await.unwrap().unwrap()
        else {
            panic!("video")
        };
        assert_eq!(mixed.data(), owned_mixed.data());
        assert_eq!(mixed.frame_rate(), owned_mixed.frame_rate());
        assert_eq!(mixed.scan_type(), owned_mixed.scan_type());
        assert_eq!(
            mixed.picture_aspect_ratio(),
            owned_mixed.picture_aspect_ratio()
        );
        assert_eq!(mixed.metadata(), owned_mixed.metadata());
        assert_eq!(frees("video"), before + 4);
    }
    script(
        serial + 10,
        "audio,audio,metadata,metadata,status,source,unknown",
    );
    let before = frees("audio");
    let audio = receiver
        .audio()
        .capture_with(Duration::ZERO, |frame| {
            assert!(frame.data().is_none());
            assert_eq!(
                frame.channel_data(0).unwrap().as_ptr() as usize,
                native_payload("audio")
            );
            assert_eq!(frame.channel_data(0).unwrap(), [0.25, 0.5]);
            assert_eq!(frame.channel_data(1).unwrap(), [-0.25, -0.5]);
            frame.to_owned()
        })
        .await
        .unwrap()
        .unwrap();
    let ReceiverItem::Audio(other) = receiver
        .capture_any_with(Duration::ZERO, |item| item.to_owned())
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("audio")
    };
    assert_eq!(audio.data(), other.data());
    assert_eq!(audio.timecode(), other.timecode());
    assert_eq!(audio.timestamp(), other.timestamp());
    assert_eq!(audio.metadata(), other.metadata());
    assert_eq!(frees("audio"), before + 2);
    let before = frees("metadata");
    assert_eq!(
        receiver
            .metadata()
            .capture_with(Duration::ZERO, |frame| {
                assert_eq!(frame.data().as_ptr() as usize, native_payload("metadata"));
                (frame.data().len(), frame.timecode())
            })
            .await
            .unwrap(),
        Some((11, Timecode::from_ticks(3456)))
    );
    let ReceiverItem::Metadata(metadata) = receiver
        .capture_any_with(Duration::ZERO, |item| item.to_owned())
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("metadata")
    };
    assert_eq!(metadata.data(), "<metadata/>");
    assert_eq!(frees("metadata"), before + 2);
    assert!(receiver
        .video()
        .capture_with(Duration::ZERO, |_| panic!("notification"))
        .await
        .unwrap()
        .is_none());
    let calls = captures();
    assert!(matches!(
        receiver
            .capture_any_with(Duration::ZERO, |item| match item {
                ReceiverItemRef::Event(event) => *event,
                _ => panic!("{item:?}"),
            })
            .await
            .unwrap(),
        Some(ReceiverEvent::StatusChanged(_))
    ));
    assert_eq!(
        captures(),
        calls,
        "consume retained event without native entry"
    );
    for expected in [ReceiverEvent::SourceChanged, ReceiverEvent::Unknown(987)] {
        assert_eq!(
            receiver
                .capture_any_with(Duration::ZERO, |item| match item {
                    ReceiverItemRef::Event(event) => *event,
                    _ => panic!("{item:?}"),
                })
                .await
                .unwrap(),
            Some(expected)
        );
    }
    assert!(receiver.poll_event(Duration::ZERO).await.unwrap().is_none());

    for (index, token, family) in [
        (20, "bad-video", "video"),
        (21, "bad-audio", "audio"),
        (22, "bad-metadata", "metadata"),
    ] {
        script(serial + index, &format!("{token},{token}"));
        let before = frees(family);
        let typed: grafton_ndi::Result<Option<()>> = match family {
            "video" => {
                receiver
                    .video()
                    .capture_with(Duration::ZERO, |_| panic!("invalid video"))
                    .await
            }
            "audio" => {
                receiver
                    .audio()
                    .capture_with(Duration::ZERO, |_| panic!("invalid audio"))
                    .await
            }
            _ => {
                receiver
                    .metadata()
                    .capture_with(Duration::ZERO, |_| panic!("invalid text"))
                    .await
            }
        };
        assert!(matches!(typed, Err(Error::InvalidFrame(_))));
        assert!(matches!(
            receiver
                .capture_any_with(Duration::ZERO, |_| panic!("invalid item"))
                .await,
            Err(Error::InvalidFrame(_))
        ));
        assert_eq!(frees(family), before + 2);
    }
    script(serial + 30, "none,none,error,error");
    assert!(receiver
        .video()
        .capture_with(Duration::ZERO, |_| panic!("miss"))
        .await
        .unwrap()
        .is_none());
    assert!(receiver
        .capture_any_with(Duration::ZERO, |_| panic!("miss"))
        .await
        .unwrap()
        .is_none());
    assert!(matches!(
        receiver.video().capture_with(Duration::ZERO, |_| ()).await,
        Err(Error::CaptureFailed(_))
    ));
    assert!(matches!(
        receiver.capture_any_with(Duration::ZERO, |_| ()).await,
        Err(Error::CaptureFailed(_))
    ));
    let calls = captures();
    assert!(matches!(
        receiver.video().capture_with(Duration::MAX, |_| ()).await,
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        receiver.capture_any_with(Duration::MAX, |_| ()).await,
        Err(Error::InvalidInput(_))
    ));
    assert_eq!(captures(), calls);

    // Unwinding releases all media families, and the worker remains usable.
    for (index, token, family) in [
        (40, "video", "video"),
        (41, "audio", "audio"),
        (42, "metadata", "metadata"),
    ] {
        script(serial + index, &format!("{token},{token}"));
        let before = frees(family);
        let result = catch_future(async {
            match family {
                "video" => {
                    receiver
                        .video()
                        .capture_with(Duration::ZERO, |_| panic!("callback panic"))
                        .await
                }
                "audio" => {
                    receiver
                        .audio()
                        .capture_with(Duration::ZERO, |_| panic!("callback panic"))
                        .await
                }
                _ => {
                    receiver
                        .metadata()
                        .capture_with(Duration::ZERO, |_| panic!("callback panic"))
                        .await
                }
            }
        })
        .await
        .expect_err("callback panic must unwind the awaiting task");
        assert_eq!(result.downcast_ref::<&str>(), Some(&"callback panic"));
        let panic = catch_future(
            receiver.capture_any_with(Duration::ZERO, move |_| std::panic::panic_any(42_u32)),
        )
        .await
        .expect_err("mixed callback panic must unwind the awaiting task");
        assert_eq!(*panic.downcast::<u32>().unwrap(), 42);
        assert_eq!(frees(family), before + 2);
    }

    {
        use std::{
            future::Future,
            sync::mpsc,
            task::{Context, Poll, Waker},
        };
        script(serial + 45, "video,video");
        let before = frees("video");
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let video = receiver.video();
        let mut abandoned = Box::pin(video.capture_with(Duration::ZERO, move |_| {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            std::panic::panic_any(77_u32);
        }));
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(
            abandoned.as_mut().poll(&mut context),
            Poll::Pending
        ));
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(abandoned);
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while frees("video") == before {
            assert!(
                Instant::now() < deadline,
                "abandoned panicking callback leaked its frame"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(receiver
            .video()
            .capture(Duration::ZERO)
            .await
            .unwrap()
            .is_some());
        assert_eq!(frees("video"), before + 2);
    }

    // Reentry would deadlock if application code still held the capture lock.
    script(serial + 50, "video,audio");
    let reenter = receiver.clone();
    receiver
        .video()
        .capture_with(Duration::ZERO, move |_| {
            reenter.receiver().reconnect();
            assert!(reenter
                .receiver()
                .audio()
                .capture_ref(Duration::ZERO)
                .unwrap()
                .is_some());
        })
        .await
        .unwrap()
        .unwrap();
    script(serial + 51, "video");
    let reenter = receiver.clone();
    receiver
        .capture_any_with(Duration::ZERO, move |_| {
            reenter.receiver().disconnect();
        })
        .await
        .unwrap()
        .unwrap();
}

#[cfg(any(feature = "tokio", feature = "smol"))]
async fn cancel_scoped<R: grafton_ndi::asynchronous::AsyncRuntime>(
    receiver: grafton_ndi::asynchronous::AsyncReceiver<R>,
    serial: usize,
    typed: bool,
    native_entry: bool,
) {
    use std::{
        future::Future,
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Arc,
        },
        task::{Context, Poll, Waker},
    };
    struct ResultDrop(Arc<AtomicBool>);
    impl Drop for ResultDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    script(serial, "video");
    if native_entry {
        std::env::set_var("GRAFTON_FAKE_CAPTURE_DELAY_MS", "80");
    }
    let calls = captures();
    let before = frees("video");
    let destroyed = log()
        .lines()
        .filter(|&line| line == "receiver-destroy")
        .count();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let dropped = Arc::new(AtomicBool::new(false));
    let worker_dropped = Arc::clone(&dropped);
    let process = move |width| {
        assert_eq!(width, 4);
        entered_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        ResultDrop(worker_dropped)
    };
    let mut future = Box::pin(async {
        if typed {
            receiver
                .video()
                .capture_with(Duration::from_millis(200), move |frame| {
                    process(frame.width())
                })
                .await
        } else {
            receiver
                .capture_any_with(Duration::from_millis(200), move |item| {
                    let ReceiverItemRef::Video(frame) = item else {
                        panic!("{item:?}")
                    };
                    process(frame.width())
                })
                .await
        }
    });
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
    let deadline = Instant::now() + Duration::from_secs(5);
    if native_entry {
        while captures() == calls {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        drop(future);
        drop(receiver);
        // Native work passed the gate; the callback still executes after both
        // its future and the async owner are gone.
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    } else {
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(future);
        drop(receiver);
    }
    assert_eq!(frees("video"), before, "callback retains the native buffer");
    assert_eq!(
        log()
            .lines()
            .filter(|&line| line == "receiver-destroy")
            .count(),
        destroyed
    );
    // The wait timeout does not bound callback processing or cancel its effects.
    std::thread::sleep(Duration::from_millis(220));
    assert_eq!(frees("video"), before);
    release_tx.send(()).unwrap();
    while !dropped.load(Ordering::Acquire)
        || log()
            .lines()
            .filter(|&line| line == "receiver-destroy")
            .count()
            == destroyed
    {
        assert!(
            Instant::now() < deadline,
            "abandoned result/receiver did not drop"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(frees("video"), before + 1);
    let records = log();
    assert!(records.rfind("free-video").unwrap() < records.rfind("receiver-destroy").unwrap());
    std::env::remove_var("GRAFTON_FAKE_CAPTURE_DELAY_MS");
}
