//! Dropped waits stop after their in-flight native attempt, on both runtimes.
#![cfg(all(
    target_os = "linux",
    not(ndi_link_static),
    any(feature = "tokio", feature = "smol")
))]

#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(child, fixture);
use common::TestChild;

#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;
use runtime_fixture::read_fake_log;

use grafton_ndi::{
    asynchronous::{AsyncReceiver, AsyncRuntime},
    Ndi, Receiver, ReceiverEvent, ReceiverItem, ReceiverOptions, RuntimeOptions, Source,
    SourceNameChange,
};
use std::{
    fs,
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{Context, Poll, Wake, Waker},
    time::{Duration, Instant},
};

#[derive(Debug)]
struct Completion(AtomicBool);

impl Wake for Completion {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::Release);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.store(true, Ordering::Release);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Notification {
    Event(ReceiverEvent),
    Name(SourceNameChange),
}

fn event(item: ReceiverItem) -> ReceiverEvent {
    let ReceiverItem::Event(event) = item else {
        panic!("script delivered media instead of an event")
    };
    event
}

fn await_condition(mut condition: impl FnMut() -> bool, reason: &str) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !condition() {
        assert!(Instant::now() < deadline, "{reason}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

async fn replayed_notifications<R: AsyncRuntime>(ndi: &Ndi) {
    let mut serial = 0;
    let runtime = std::any::type_name::<R>().rsplit("::").next().unwrap();
    for kind in 0..5 {
        for combined in [false, true] {
            if kind >= 3 && combined {
                continue;
            }
            for completed in [false, true] {
                serial += 1;
                let receiver = AsyncReceiver::<R>::new(
                    Receiver::new(ndi, &ReceiverOptions::new(Source::new("").unwrap())).unwrap(),
                );
                let (script, expected) = match kind {
                    0 => (
                        "status",
                        Notification::Event(ReceiverEvent::StatusChanged(Default::default())),
                    ),
                    1 => ("source", Notification::Event(ReceiverEvent::SourceChanged)),
                    2 => ("unknown", Notification::Event(ReceiverEvent::Unknown(987))),
                    3 => (
                        "renamed camera",
                        Notification::Name(SourceNameChange::Connected("renamed camera".into())),
                    ),
                    _ => (
                        "disconnected",
                        Notification::Name(SourceNameChange::Disconnected),
                    ),
                };
                let script_name = if kind >= 3 {
                    "GRAFTON_FAKE_SOURCE_NAME_SCRIPT"
                } else {
                    "GRAFTON_FAKE_CAPTURE_SCRIPT"
                };
                // The runtime name prevents the second runtime reusing the
                // previous one's final one-shot script.
                std::env::set_var(script_name, format!("{runtime}-{serial}:{script}"));
                let before = calls(kind >= 3);
                let mut future = Box::pin(async {
                    let budget = Duration::from_secs(30);
                    if kind >= 3 {
                        receiver
                            .wait_for_source_name_change(budget)
                            .await
                            .unwrap()
                            .map(Notification::Name)
                    } else if combined {
                        receiver
                            .capture_any(budget)
                            .await
                            .unwrap()
                            .map(event)
                            .map(Notification::Event)
                    } else {
                        receiver
                            .next_event(budget)
                            .await
                            .unwrap()
                            .map(Notification::Event)
                    }
                });
                let completion = Arc::new(Completion(AtomicBool::new(false)));
                let waker = Waker::from(Arc::clone(&completion));
                let mut context = Context::from_waker(&waker);
                assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
                await_condition(
                    || calls(kind >= 3) > before,
                    "worker never entered native wait",
                );
                if completed {
                    // A completion wake establishes that the worker's result
                    // reached its runtime, without polling our future again.
                    await_condition(
                        || completion.0.load(Ordering::Acquire),
                        "worker did not complete",
                    );
                }
                drop(future);
                let mut replayed = None;
                await_condition(
                    || {
                        replayed = if kind >= 3 {
                            receiver
                                .receiver()
                                .wait_for_source_name_change(Duration::ZERO)
                                .unwrap()
                                .map(Notification::Name)
                        } else {
                            receiver
                                .receiver()
                                .next_event(Duration::ZERO)
                                .unwrap()
                                .map(Notification::Event)
                        };
                        replayed.is_some()
                    },
                    &format!("an abandoned notification was lost: {} kind={kind} combined={combined} completed={completed}", std::any::type_name::<R>()),
                );
                assert_eq!(replayed, Some(expected));
                if kind >= 3 {
                    assert!(receiver
                        .receiver()
                        .wait_for_source_name_change(Duration::ZERO)
                        .unwrap()
                        .is_none());
                } else {
                    assert!(receiver
                        .receiver()
                        .next_event(Duration::ZERO)
                        .unwrap()
                        .is_none());
                }

                // Normal completion disarms the hand-off and must never replay.
                serial += 1;
                std::env::set_var(script_name, format!("{runtime}-{serial}:{script}"));
                if kind >= 3 {
                    assert!(receiver
                        .wait_for_source_name_change(Duration::ZERO)
                        .await
                        .unwrap()
                        .is_some());
                    assert!(receiver
                        .receiver()
                        .wait_for_source_name_change(Duration::ZERO)
                        .unwrap()
                        .is_none());
                } else {
                    let delivered = if combined {
                        receiver
                            .capture_any(Duration::ZERO)
                            .await
                            .unwrap()
                            .map(event)
                    } else {
                        receiver.next_event(Duration::ZERO).await.unwrap()
                    };
                    assert!(delivered.is_some());
                    assert!(receiver
                        .receiver()
                        .next_event(Duration::ZERO)
                        .unwrap()
                        .is_none());
                }
            }
        }
    }
    std::env::remove_var("GRAFTON_FAKE_SOURCE_NAME_SCRIPT");
}

fn calls(source_name: bool) -> usize {
    let prefix = if source_name {
        "source-name:"
    } else {
        "capture:"
    };
    read_fake_log()
        .lines()
        .filter(|line| line.starts_with(prefix))
        .count()
}

fn destroyed() -> usize {
    read_fake_log()
        .lines()
        .filter(|&line| line == "receiver-destroy")
        .count()
}

async fn cancelled_waits<R: AsyncRuntime>(ndi: &Ndi) {
    for kind in 0..4 {
        let receiver = AsyncReceiver::<R>::new(
            Receiver::new(
                ndi,
                &ReceiverOptions::new(Source::new("cancel fixture").unwrap()),
            )
            .unwrap(),
        );
        let before = calls(kind == 3);
        let before_destroyed = destroyed();
        let mut future = Box::pin(async {
            let budget = Duration::from_secs(30);
            match kind {
                0 => {
                    receiver.capture_video(budget).await.unwrap();
                }
                1 => {
                    receiver.capture_any(budget).await.unwrap();
                }
                2 => {
                    receiver.next_event(budget).await.unwrap();
                }
                _ => {
                    receiver.wait_for_source_name_change(budget).await.unwrap();
                }
            }
        });
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
        let deadline = Instant::now() + Duration::from_secs(3);
        while calls(kind == 3) == before {
            assert!(
                Instant::now() < deadline,
                "wait {kind} never entered the SDK"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        drop(future);
        drop(receiver);
        while destroyed() == before_destroyed {
            assert!(
                Instant::now() < deadline,
                "wait {kind} retained its receiver"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            calls(kind == 3),
            before + 1,
            "wait {kind} attempted after cancellation"
        );
        assert_eq!(destroyed(), before_destroyed + 1);
    }
    for line in read_fake_log()
        .lines()
        .filter(|line| line.starts_with("source-name:"))
    {
        assert!(
            line.strip_prefix("source-name:")
                .unwrap()
                .parse::<u32>()
                .unwrap()
                <= 100
        );
    }
}

#[test]
fn abandoned_waits_release_the_receiver_without_spending_their_timeout() {
    if let Some(library) = std::env::var_os("GRAFTON_CANCEL_CHILD") {
        let ndi = Ndi::with_options(&RuntimeOptions::new().library_path(library)).unwrap();
        #[cfg(feature = "tokio")]
        ::tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                cancelled_waits::<grafton_ndi::asynchronous::Tokio>(&ndi).await;
                replayed_notifications::<grafton_ndi::asynchronous::Tokio>(&ndi).await;
            });
        #[cfg(feature = "smol")]
        ::smol::block_on(async {
            cancelled_waits::<grafton_ndi::asynchronous::Smol>(&ndi).await;
            replayed_notifications::<grafton_ndi::asynchronous::Smol>(&ndi).await;
        });
        return;
    }
    let directory = common::FixtureDir::create("async-cancellation");
    let library = directory.path().join("libndi_cancel.so");
    runtime_fixture::FakeRuntime {
        cfgs: vec!["fake_objects", "fake_capture", "fake_source_name"],
        ..Default::default()
    }
    .build(&library);
    TestChild::new("abandoned_waits_release_the_receiver_without_spending_their_timeout")
        .env("GRAFTON_CANCEL_CHILD", library)
        .env("GRAFTON_FAKE_NDI_LOG", directory.path().join("cancel.log"))
        .env("GRAFTON_FAKE_CAPTURE_DELAY_MS", "80")
        .env("GRAFTON_FAKE_SOURCE_NAME_DELAY_MS", "80")
        .run();
}

async fn source_name_worker_retains_runtime<R: AsyncRuntime>(ndi: Ndi) {
    let receiver = AsyncReceiver::<R>::new(
        Receiver::new(&ndi, &ReceiverOptions::new(Source::new("").unwrap())).unwrap(),
    );
    drop(ndi);
    let before = calls(true);
    let mut future = Box::pin(receiver.wait_for_source_name_change(Duration::from_secs(30)));
    assert!(matches!(
        future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    await_condition(|| calls(true) > before, "source-name worker never started");
    drop(future);
    drop(receiver);
    assert!(Ndi::is_running(), "worker lost its initialized runtime");
    assert_eq!(destroyed(), 0, "receiver destroyed during its native query");
    fs::write(
        std::env::var_os("GRAFTON_FAKE_SOURCE_NAME_RELEASE_FILE").unwrap(),
        [],
    )
    .unwrap();
    await_condition(
        || !Ndi::is_running(),
        "worker retained the runtime after its query",
    );
    // Stopping already reports not running, before the native destroy has
    // necessarily finished writing its lifecycle evidence.
    await_condition(
        || read_fake_log().lines().any(|line| line == "destroy"),
        "runtime destruction did not complete",
    );
    let events = read_fake_log();
    assert_eq!(destroyed(), 1);
    assert_eq!(events.lines().filter(|line| *line == "destroy").count(), 1);
    assert!(events.find("receiver-destroy").unwrap() < events.find("\ndestroy").unwrap());
}

#[test]
fn a_source_name_worker_keeps_its_runtime_until_its_binding_drops() {
    if let Some(library) = std::env::var_os("GRAFTON_BINDING_CHILD") {
        let ndi = Ndi::with_options(&RuntimeOptions::new().library_path(library)).unwrap();
        #[cfg(feature = "tokio")]
        if std::env::var("GRAFTON_BINDING_RUNTIME").unwrap() == "tokio" {
            ::tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
                .block_on(source_name_worker_retains_runtime::<
                    grafton_ndi::asynchronous::Tokio,
                >(ndi));
            return;
        }
        #[cfg(feature = "smol")]
        ::smol::block_on(source_name_worker_retains_runtime::<
            grafton_ndi::asynchronous::Smol,
        >(ndi));
        return;
    }
    let directory = common::FixtureDir::create("async-source-binding");
    let library = directory.path().join("libndi_binding.so");
    runtime_fixture::FakeRuntime {
        cfgs: vec!["fake_objects", "fake_capture", "fake_source_name"],
        ..Default::default()
    }
    .build(&library);
    let runtimes = [
        #[cfg(feature = "tokio")]
        "tokio",
        #[cfg(feature = "smol")]
        "smol",
    ];
    for runtime in runtimes {
        TestChild::new("a_source_name_worker_keeps_its_runtime_until_its_binding_drops")
            .env("GRAFTON_BINDING_CHILD", &library)
            .env("GRAFTON_BINDING_RUNTIME", runtime)
            .env(
                "GRAFTON_FAKE_NDI_LOG",
                directory.path().join(format!("{runtime}.log")),
            )
            .env(
                "GRAFTON_FAKE_SOURCE_NAME_RELEASE_FILE",
                directory.path().join(format!("{runtime}.release")),
            )
            .run();
    }
}

async fn capture_queue_delay<R: AsyncRuntime>(ndi: &Ndi, gates: &std::path::Path) {
    use runtime_fixture::NativeGate;

    const BUDGET: Duration = Duration::from_millis(30);
    let runtime = std::any::type_name::<R>().rsplit("::").next().unwrap();
    for (operation, slots) in [("event", "000"), ("mixed", "111"), ("scoped", "100")] {
        for cancelled in [false, true] {
            let gate = gates.join(format!("{runtime}-{operation}-{cancelled}"));
            fs::create_dir(&gate).unwrap();
            std::env::set_var("GRAFTON_FAKE_CAPTURE_GATE", &gate);
            std::env::set_var(
                "GRAFTON_FAKE_CAPTURE_SCRIPT",
                format!("{runtime}-{operation}-{cancelled}:hold"),
            );
            let release = NativeGate(&gate);
            let receiver = AsyncReceiver::<R>::new(
                Receiver::new(ndi, &ReceiverOptions::new(Source::new("").unwrap())).unwrap(),
            );
            let before = read_fake_log().len();
            let before_destroyed = destroyed();
            let mut occupied = Box::pin(receiver.capture_video(Duration::ZERO));
            let mut context = Context::from_waker(Waker::noop());
            assert!(matches!(
                occupied.as_mut().poll(&mut context),
                Poll::Pending
            ));
            release.wait_for_entry();
            let mut request = Box::pin(async {
                match operation {
                    "event" => assert!(receiver.next_event(BUDGET).await.unwrap().is_none()),
                    "mixed" => assert!(receiver.capture_any(BUDGET).await.unwrap().is_none()),
                    "scoped" => assert!(receiver
                        .capture_video_with(BUDGET, |_| panic!("unexpected callback"))
                        .await
                        .unwrap()
                        .is_none()),
                    _ => unreachable!(),
                }
            });
            assert!(matches!(request.as_mut().poll(&mut context), Poll::Pending));
            if cancelled {
                drop(request);
                drop(release);
            } else {
                // The one blocking worker is still inside the fake SDK call.
                // Let the new operation's budget expire in the queue.
                std::thread::sleep(BUDGET + Duration::from_millis(10));
                let log = read_fake_log();
                assert_eq!(
                    log[before..]
                        .lines()
                        .filter(|line| line.starts_with("capture:"))
                        .count(),
                    1,
                    "the queued operation ran before the blocker was released",
                );
                drop(release);
                request.await;
            }
            assert!(occupied.await.unwrap().is_none());
            drop(receiver);
            await_condition(
                || destroyed() == before_destroyed + 1,
                "queued worker retained the receiver",
            );
            let log = read_fake_log();
            let native_waits: Vec<_> = log[before..]
                .lines()
                .filter(|line| line.starts_with("capture:"))
                .collect();
            if cancelled {
                assert_eq!(native_waits, ["capture:hold:100:0"]);
            } else {
                assert_eq!(
                    native_waits,
                    ["capture:hold:100:0", &format!("capture:none:{slots}:0")]
                );
            }
        }
    }
}

async fn combined_capture_during_gated_connect<R: AsyncRuntime>(receiver: &AsyncReceiver<R>) {
    let before = read_fake_log().len();
    assert!(receiver
        .capture_any(Duration::ZERO)
        .await
        .unwrap()
        .is_none());
    let log = read_fake_log();
    assert_eq!(
        log[before..]
            .lines()
            .filter(|line| line.starts_with("capture:"))
            .collect::<Vec<_>>(),
        ["capture:none:111:0"],
    );
    let budget = Duration::from_millis(25);
    let began = Instant::now();
    assert!(receiver
        .capture_any_with(budget, |_| panic!("unexpected callback"))
        .await
        .unwrap()
        .is_none());
    assert!(began.elapsed() >= budget);
    assert!(read_fake_log()[log.len()..]
        .lines()
        .any(|line| line.starts_with("capture:none:111:")));
}

fn async_capture_while_connect_is_gated<R: AsyncRuntime>(
    ndi: &Ndi,
    gates: &std::path::Path,
    run: impl FnOnce(&AsyncReceiver<R>) + Send,
) {
    let runtime = std::any::type_name::<R>().rsplit("::").next().unwrap();
    let gate = gates.join(format!("{runtime}-connect"));
    fs::create_dir(&gate).unwrap();
    std::env::set_var("GRAFTON_FAKE_CONNECT_GATE", &gate);
    let receiver = AsyncReceiver::<R>::new(
        Receiver::new(ndi, &ReceiverOptions::new(Source::new("").unwrap())).unwrap(),
    );
    std::thread::scope(|scope| {
        let receiver = &receiver;
        let release = runtime_fixture::NativeGate(&gate);
        scope.spawn(|| {
            receiver
                .receiver()
                .connect_to(&Source::new("Blocked").unwrap())
        });
        release.wait_for_entry();
        let (send, done) = std::sync::mpsc::channel();
        scope.spawn(move || {
            run(receiver);
            send.send(()).unwrap();
        });
        let completed = done.recv_timeout(Duration::from_secs(2));
        drop(release);
        completed.expect("async capture waited for native connect");
    });
}

#[test]
fn queued_captures_spend_their_budget_and_skipped_jobs_never_reach_the_sdk() {
    if let Some(library) = std::env::var_os("GRAFTON_QUEUE_DELAY_CHILD") {
        let ndi = Ndi::with_options(&RuntimeOptions::new().library_path(library)).unwrap();
        let gates = std::path::PathBuf::from(std::env::var_os("GRAFTON_FAKE_GATE_DIR").unwrap());
        #[cfg(feature = "tokio")]
        {
            let runtime = ::tokio::runtime::Builder::new_current_thread()
                .max_blocking_threads(1)
                .build()
                .unwrap();
            runtime.block_on(capture_queue_delay::<grafton_ndi::asynchronous::Tokio>(
                &ndi, &gates,
            ));
            async_capture_while_connect_is_gated(
                &ndi,
                &gates,
                |receiver: &AsyncReceiver<grafton_ndi::asynchronous::Tokio>| {
                    runtime.block_on(combined_capture_during_gated_connect(receiver));
                },
            );
        }
        #[cfg(feature = "smol")]
        {
            ::smol::block_on(capture_queue_delay::<grafton_ndi::asynchronous::Smol>(
                &ndi, &gates,
            ));
            async_capture_while_connect_is_gated(
                &ndi,
                &gates,
                |receiver: &AsyncReceiver<grafton_ndi::asynchronous::Smol>| {
                    ::smol::block_on(combined_capture_during_gated_connect(receiver));
                },
            );
        }
        return;
    }
    let directory = common::FixtureDir::create("async-queue-delay");
    let library = runtime_fixture::FakeRuntime {
        cfgs: vec!["fake_objects", "fake_capture", "fake_blocking_connect"],
        ..Default::default()
    }
    .build(&directory.join("libndi-queue-delay.so"));
    TestChild::new("queued_captures_spend_their_budget_and_skipped_jobs_never_reach_the_sdk")
        .env("GRAFTON_QUEUE_DELAY_CHILD", library)
        .env("GRAFTON_FAKE_NDI_LOG", directory.join("queue.log"))
        .env("GRAFTON_FAKE_GATE_DIR", directory.path())
        .env("BLOCKING_MAX_THREADS", "1")
        .run_with_timeout(Duration::from_secs(30));
}
