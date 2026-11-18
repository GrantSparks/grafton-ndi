//! Connection targets and native requests stay in agreement, without an SDK.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(child, fixture);
use common::TestChild;

#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;
use runtime_fixture::{read_fake_log, NativeGate};

use grafton_ndi::{
    Error, Ndi, Receiver, ReceiverEvent, ReceiverItemRef, ReceiverOptions, RuntimeOptions, Source,
};
use std::{
    path::PathBuf,
    sync::{mpsc, Barrier},
    time::{Duration, Instant},
};

#[test]
fn connection_target_tracks_native_requests() {
    if std::env::var_os("GRAFTON_TARGET_CHILD").is_some() {
        exercise_target();
        return;
    }
    let directory = common::FixtureDir::create("connection-target");
    let library = directory.path().join("libndi_target.so");
    runtime_fixture::FakeRuntime {
        cfgs: vec!["fake_objects"],
        ..Default::default()
    }
    .build(&library);
    TestChild::new("connection_target_tracks_native_requests")
        .env("GRAFTON_TARGET_CHILD", &library)
        .env(
            "GRAFTON_FAKE_NDI_LOG",
            directory.path().join("requests.log"),
        )
        .run();
}

#[test]
fn connection_target_snapshot_does_not_wait_for_a_native_connection_request() {
    if let Some(library) = std::env::var_os("GRAFTON_TARGET_BLOCKED_CHILD") {
        let ndi = Ndi::with_options(
            &RuntimeOptions::new().library_path(std::path::PathBuf::from(library)),
        )
        .unwrap();
        let initial = Source::new("Initial").unwrap();
        let receiver = Receiver::new(&ndi, &ReceiverOptions::new(initial.clone())).unwrap();
        let gate = PathBuf::from(std::env::var_os("GRAFTON_FAKE_CONNECT_GATE").unwrap());
        let blocked = Source::new("Blocked").unwrap();
        std::env::set_var("GRAFTON_FAKE_CAPTURE_SCRIPT", "native-gate:source");
        assert!(receiver
            .capture_audio_ref(Duration::ZERO)
            .unwrap()
            .is_none());
        std::thread::scope(|scope| {
            let receiver = &receiver;
            let release = NativeGate(&gate);
            scope.spawn(|| receiver.connect_to(&blocked));
            release.wait_for_entry();
            let (send, receive) = mpsc::channel();
            let (finished, capture_result) = mpsc::channel();
            scope.spawn(move || {
                send.send(receiver.connection_target()).unwrap();
                capture_during_native_connect(receiver);
                finished.send(()).unwrap();
            });
            let snapshot = receive.recv_timeout(Duration::from_secs(2));
            // The capture must complete while connect is still blocked.
            let captured = capture_result.recv_timeout(Duration::from_secs(3));
            drop(release);
            assert_eq!(
                snapshot.expect("snapshot waited for native connect"),
                initial
            );
            captured.expect("capture did not finish while native connect was gated");
        });
        assert_eq!(receiver.connection_target(), blocked);

        let capture_gate = PathBuf::from(std::env::var_os("GRAFTON_FAKE_CAPTURE_GATE").unwrap());
        std::env::set_var("GRAFTON_FAKE_CAPTURE_SCRIPT", "wait-gate:hold,status");
        std::thread::scope(|scope| {
            let release = NativeGate(&capture_gate);
            let waiting = scope.spawn(|| receiver.next_event(Duration::from_secs(8)).unwrap());
            release.wait_for_entry();
            let (send, completed) = mpsc::channel();
            let receiver = &receiver;
            scope.spawn(move || {
                receiver.reconnect();
                assert!(receiver
                    .capture_audio_ref(Duration::ZERO)
                    .unwrap()
                    .is_none());
                send.send(()).unwrap();
            });
            let changed = completed.recv_timeout(Duration::from_secs(2));
            drop(release);
            changed.expect("reconnect and typed capture must run during an event wait");
            assert!(matches!(
                waiting.join().unwrap(),
                Some(ReceiverEvent::StatusChanged(_))
            ));
        });
        return;
    }
    let directory = common::FixtureDir::create("connection-target-blocked");
    let library = directory.path().join("libndi_blocked.so");
    runtime_fixture::FakeRuntime {
        cfgs: vec!["fake_objects", "fake_blocking_connect", "fake_capture"],
        ..Default::default()
    }
    .build(&library);
    let capture_gate = common::FixtureDir::create("capture-blocked");
    TestChild::new("connection_target_snapshot_does_not_wait_for_a_native_connection_request")
        .env("GRAFTON_TARGET_BLOCKED_CHILD", &library)
        .env("GRAFTON_FAKE_CONNECT_GATE", directory.path())
        .env("GRAFTON_FAKE_CAPTURE_GATE", capture_gate.path())
        .env("GRAFTON_FAKE_NDI_LOG", directory.join("captures.log"))
        .run();
}

fn capture_during_native_connect(receiver: &Receiver) {
    let before = read_fake_log();
    assert!(matches!(
        receiver.capture_any_ref(Duration::ZERO).unwrap(),
        Some(ReceiverItemRef::Event(ReceiverEvent::SourceChanged))
    ));
    assert_eq!(
        read_fake_log(),
        before,
        "consuming a retained event called the SDK"
    );
    assert!(receiver.next_event(Duration::ZERO).unwrap().is_none());
    assert!(receiver.capture_video(Duration::ZERO).unwrap().is_none());
    assert!(receiver.capture_audio(Duration::ZERO).unwrap().is_none());
    assert!(receiver
        .capture_metadata_ref(Duration::ZERO)
        .unwrap()
        .is_none());
    assert!(receiver.capture_any(Duration::ZERO).unwrap().is_none());
    let log = read_fake_log();
    let attempts: Vec<_> = log[before.len()..]
        .lines()
        .filter_map(|line| line.strip_prefix("capture:none:"))
        .collect();
    assert_eq!(attempts, ["000:0", "100:0", "010:0", "001:0", "111:0"]);

    let budget = Duration::from_millis(230);
    let began = Instant::now();
    assert!(receiver.next_event(budget).unwrap().is_none());
    assert!(began.elapsed() >= budget);
    let bounded = read_fake_log();
    let waits: Vec<_> = bounded[log.len()..]
        .lines()
        .filter_map(|line| line.strip_prefix("capture:none:000:"))
        .map(|wait| wait.parse::<u32>().unwrap())
        .collect();
    assert!(!waits.is_empty());
    assert!(waits.into_iter().all(|wait| wait <= 100));
}

fn exercise_target() {
    let path = std::path::PathBuf::from(std::env::var_os("GRAFTON_TARGET_CHILD").unwrap());
    let ndi = Ndi::with_options(&RuntimeOptions::new().library_path(path)).unwrap();
    let a = Source::new("A").unwrap();
    let b = Source::new("B").unwrap();
    let receiver = Receiver::new(&ndi, &ReceiverOptions::new(a.clone())).unwrap();
    let initial = receiver.connection_target();
    assert_eq!(initial, a);
    let _: () = receiver.reconnect();
    assert_eq!(receiver.connection_target(), a);
    let _: () = receiver.connect_to(&b);
    assert_eq!(receiver.connection_target(), b);
    assert_eq!(initial, a, "a snapshot does not change with the receiver");
    receiver.disconnect();
    assert_eq!(receiver.connection_target(), b);
    receiver.reconnect();
    assert_eq!(receiver.connection_target(), b);
    assert!(matches!(
        Source::new("bad\0name"),
        Err(Error::InvalidCString(_))
    ));
    assert_eq!(receiver.connection_target(), b);
    let requests = || {
        read_fake_log()
            .lines()
            .filter_map(|line| line.strip_prefix("receiver-connect:").map(str::to_owned))
            .collect::<Vec<_>>()
    };
    assert_eq!(requests(), ["A", "B", "<disconnected>", "B"]);

    let start = Barrier::new(8);
    std::thread::scope(|scope| {
        for worker in 0..8 {
            let receiver = &receiver;
            let start = &start;
            scope.spawn(move || {
                start.wait();
                for request in 0..25 {
                    receiver.connect_to(&Source::new(format!("{worker}/{request}")).unwrap());
                    receiver.reconnect();
                }
            });
        }
    });
    let concurrent = requests();
    assert_eq!(concurrent.len(), 404);
    // Each explicit target is unique. A reconnect can repeat only the current
    // target; a target reappearing after another would expose a stale retry.
    let mut transitions = concurrent[4..].to_vec();
    transitions.dedup();
    assert_eq!(
        transitions
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        transitions.len()
    );
    assert_eq!(
        requests().last().unwrap(),
        receiver.connection_target().name()
    );
    #[cfg(feature = "tokio")]
    {
        let before = requests();
        rejected_async_requests(
            Receiver::new(&ndi, &ReceiverOptions::new(Source::new("").unwrap())).unwrap(),
        );
        assert_eq!(
            requests(),
            before,
            "refused async inputs issued native requests"
        );
    }
    // Retrying after the contention must send exactly the retained target.
    let target = receiver.connection_target();
    receiver.disconnect();
    receiver.reconnect();
    assert_eq!(requests().last().unwrap(), target.name());
}

#[cfg(feature = "tokio")]
fn rejected_async_requests(receiver: Receiver) {
    use grafton_ndi::asynchronous::{AsyncReceiver, Tokio};
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
        time::Duration,
    };
    let receiver = AsyncReceiver::<Tokio>::new(receiver);
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(
        Source::new("bad\0async source"),
        Err(Error::InvalidCString(_))
    ));
    let source = Source::new("valid async source").unwrap();
    let mut connect = std::pin::pin!(receiver.connect_to(&source));
    assert!(matches!(
        connect.as_mut().poll(&mut context),
        Poll::Ready(Err(Error::SpawnFailed(
            grafton_ndi::SpawnFailure::NoRuntime
        )))
    ));
    let mut change = std::pin::pin!(receiver.wait_for_source_name_change(Duration::ZERO));
    assert!(matches!(
        change.as_mut().poll(&mut context),
        Poll::Ready(Err(Error::UnsupportedCapability { .. }))
    ));
}
