//! One-shot native reports must wake every concurrent observer in Rust.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

#[path = "support/fixture.rs"]
mod fixture;
#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;
#[path = "../src/test_alloc.rs"]
#[allow(dead_code)]
mod test_alloc;

use fixture::FixtureDir;
use grafton_ndi::{Finder, FinderOptions, Ndi, RuntimeOptions};
use runtime_fixture::FakeRuntime;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{mpsc, Barrier},
    thread,
    time::{Duration, Instant},
};

const PATIENCE: Duration = Duration::from_secs(5);

fn events() -> String {
    fs::read_to_string(std::env::var_os("GRAFTON_FAKE_NDI_LOG").unwrap()).unwrap_or_default()
}

fn wait_for_entries(label: &str, count: usize) {
    let end = Instant::now() + PATIENCE;
    while events().lines().filter(|line| *line == label).count() < count {
        assert!(Instant::now() < end, "missing {label}: {}", events());
        thread::sleep(Duration::from_millis(1));
    }
}

fn no_overlapping_waits(label: &str) {
    let mut active = std::collections::HashSet::new();
    for event in events().lines().filter(|line| line.starts_with(label)) {
        if let Some(selector) = event.strip_suffix("-wait-begin") {
            assert!(
                active.insert(selector.to_owned()),
                "overlapping {selector} waits: {}",
                events()
            );
        } else if let Some(selector) = event.strip_suffix("-wait-end") {
            assert!(active.remove(selector), "unmatched {event}");
        }
    }
    assert!(
        active.is_empty(),
        "native waits were retained: {}",
        events()
    );
}

#[test]
fn shared_native_change_reports() {
    if let Some(library) = std::env::var_os("GRAFTON_CHANGES_LIBRARY") {
        let library = PathBuf::from(library);
        let ndi = Ndi::with_options(&RuntimeOptions::new().library_path(&library)).unwrap();
        // SAFETY: the fixture exports this test-only function with this exact
        // signature; keeping the library live keeps that entry point live.
        let control = unsafe { libloading::Library::new(&library) }.unwrap();
        let publish =
            unsafe { control.get::<unsafe extern "C" fn()>(b"GRAFTON_fake_source_change\0") }
                .unwrap();
        match std::env::var("GRAFTON_CHANGES_CASE").unwrap().as_str() {
            "find" => find(&ndi, || unsafe { publish() }),
            "wait" => wait(&ndi, || unsafe { publish() }),
            "handoff" => handoff(&ndi, || unsafe { publish() }),
            "cursor" => cursor(&ndi, || unsafe { publish() }),
            "snapshot" => snapshot_ordering(&ndi, || unsafe { publish() }),
            "cold-watch" => cold_watch(&ndi),
            "failed-snapshot" => failed_snapshot(&ndi, || unsafe { publish() }, true),
            "failed-snapshot-pending" => failed_snapshot(&ndi, || unsafe { publish() }, false),
            #[cfg(feature = "tokio")]
            "async-tokio" => ::tokio::runtime::Builder::new_current_thread()
                .max_blocking_threads(1)
                .build()
                .unwrap()
                .block_on(async_discovery::<grafton_ndi::asynchronous::Tokio>(
                    &ndi,
                    || unsafe { publish() },
                )),
            #[cfg(feature = "tokio")]
            "async-queued-tokio" => ::tokio::runtime::Builder::new_current_thread()
                .max_blocking_threads(1)
                .build()
                .unwrap()
                .block_on(async_queued_discovery(&ndi)),
            #[cfg(feature = "smol")]
            "async-smol" => ::smol::block_on(async_discovery::<grafton_ndi::asynchronous::Smol>(
                &ndi,
                || unsafe { publish() },
            )),
            #[cfg(feature = "advanced")]
            "keyframe" => keyframe(&ndi, false),
            #[cfg(feature = "advanced")]
            "keyframe-cleared" => keyframe(&ndi, true),
            case => panic!("unknown change scenario {case}"),
        }
        return;
    }
    let directory = FixtureDir::create("change-notifications");
    let library = directory.path().join("libndi_advanced.so");
    FakeRuntime {
        cfgs: vec![
            "fake_objects",
            "fake_advanced",
            "fake_finder",
            "fake_changes",
        ],
        version: Some("6.4.0.2"),
        ..Default::default()
    }
    .build(&library);
    let mut cases = vec![
        "find",
        "wait",
        "handoff",
        "cursor",
        "snapshot",
        "cold-watch",
        "failed-snapshot",
        "failed-snapshot-pending",
    ];
    if cfg!(feature = "tokio") {
        cases.extend(["async-tokio", "async-queued-tokio"]);
    }
    if cfg!(feature = "smol") {
        cases.push("async-smol");
    }
    if cfg!(feature = "advanced") {
        cases.extend(["keyframe", "keyframe-cleared"]);
    }
    for case in cases {
        run_child(directory.path(), &library, case);
    }
}

fn run_child(directory: &Path, library: &Path, case: &str) {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "shared_native_change_reports", "--nocapture"])
        .env("GRAFTON_CHANGES_LIBRARY", library)
        .env("GRAFTON_CHANGES_CASE", case)
        .env("GRAFTON_FAKE_FINDER_CASE", "broadcast")
        .env(
            "GRAFTON_FAKE_NDI_LOG",
            directory.join(format!("{case}.log")),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{case}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // A build without Advanced declarations still exercises Finder changes.
    if !output.stderr.is_empty() {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
    }
}

// Exercise the real Rust Finder constructor even without an installed SDK,
// measuring its first watch rather than a warmed-up repeat.
fn cold_watch(ndi: &Ndi) {
    let finder = Finder::new(ndi, &FinderOptions::new()).unwrap();
    let (mut watch, allocations) = test_alloc::allocation_count(|| finder.watch());
    assert_eq!(allocations, 0);
    assert_eq!(watch.current_sources().unwrap().len(), 1);
}

fn find(ndi: &Ndi, publish: impl Fn()) {
    let finder = Finder::new(ndi, &FinderOptions::new()).unwrap();
    let ready = Barrier::new(5);
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        for camera in [1, 1, 2, 2] {
            let (finder, ready, tx) = (&finder, &ready, tx.clone());
            scope.spawn(move || {
                let mut first = true;
                let expected = format!("STUDIO (Camera {camera})");
                let result = finder.find_source(PATIENCE, |source| {
                    if std::mem::take(&mut first) {
                        ready.wait();
                    }
                    source.name() == expected
                });
                tx.send((camera, result.unwrap())).unwrap();
            });
        }
        ready.wait();
        wait_for_entries("finder-wait-begin", 1);
        publish();
        for _ in 0..2 {
            let (camera, source) = rx
                .recv_timeout(Duration::from_secs(1))
                .expect("a lookup lost the first update");
            assert_eq!(camera, 1);
            assert_eq!(source.unwrap().name(), "STUDIO (Camera 1)");
        }
        wait_for_entries("finder-wait-begin", 2);
        publish();
        for _ in 0..2 {
            let (camera, source) = rx
                .recv_timeout(Duration::from_secs(1))
                .expect("a lookup lost the second update");
            assert_eq!(camera, 2);
            assert_eq!(source.unwrap().name(), "STUDIO (Camera 2)");
        }
    });
    no_overlapping_waits("finder");
}

fn wait(ndi: &Ndi, publish: impl Fn()) {
    let finder = Finder::new(ndi, &FinderOptions::new()).unwrap();
    let ready = Barrier::new(5);
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        for _ in 0..4 {
            let (finder, ready, tx) = (&finder, &ready, tx.clone());
            scope.spawn(move || {
                let mut watch = finder.watch();
                ready.wait();
                tx.send(watch.wait_for_change(PATIENCE).unwrap()).unwrap();
            });
        }
        ready.wait();
        wait_for_entries("finder-wait-begin", 1);
        thread::sleep(Duration::from_millis(40));
        publish();
        for _ in 0..4 {
            assert!(rx
                .recv_timeout(Duration::from_secs(1))
                .expect("a source waiter lost the update"));
        }
    });
    no_overlapping_waits("finder");
    assert!(!finder.watch().wait_for_change(Duration::ZERO).unwrap());
}

fn handoff(ndi: &Ndi, publish: impl Fn()) {
    let finder = Finder::new(ndi, &FinderOptions::new()).unwrap();
    thread::scope(|scope| {
        let short = scope.spawn(|| {
            finder
                .watch()
                .wait_for_change(Duration::from_millis(100))
                .unwrap()
        });
        wait_for_entries("finder-wait-begin", 1);
        let long = scope.spawn(|| finder.watch().wait_for_change(PATIENCE).unwrap());
        assert!(!short.join().unwrap());
        wait_for_entries("finder-wait-begin", 2);
        publish();
        assert!(long.join().unwrap());
    });
    no_overlapping_waits("finder");
}

// A lookup can consume the native report while a different consumer is busy
// processing its last snapshot. Its cursor still sees the report under
// contention, and acknowledges it exactly once.
fn cursor(ndi: &Ndi, publish: impl Fn()) {
    let finder = Finder::new(ndi, &FinderOptions::new()).unwrap();
    let mut watch = finder.watch();
    assert_eq!(watch.current_sources().unwrap().len(), 1);
    thread::scope(|scope| {
        let first = scope.spawn(|| {
            finder
                .find_source(PATIENCE, |source| source.name() == "STUDIO (Camera 1)")
                .unwrap()
        });
        wait_for_entries("finder-wait-begin", 1);
        publish();
        assert!(first.join().unwrap().is_some());
        let second = scope.spawn(|| {
            finder
                .find_source(PATIENCE, |source| source.name() == "STUDIO (Camera 2)")
                .unwrap()
        });
        wait_for_entries("finder-wait-begin", 2);
        assert!(watch.wait_for_change(Duration::ZERO).unwrap());
        assert!(!watch.wait_for_change(Duration::ZERO).unwrap());
        // A new cursor starts after the report and must not claim it.
        assert!(!finder.watch().wait_for_change(Duration::ZERO).unwrap());
        publish();
        assert!(second.join().unwrap().is_some());
        assert!(watch.wait_for_change(Duration::ZERO).unwrap());
        assert!(!watch.wait_for_change(Duration::ZERO).unwrap());
    });
    no_overlapping_waits("finder");
}

#[cfg(feature = "tokio")]
async fn async_queued_discovery(ndi: &Ndi) {
    use grafton_ndi::asynchronous::tokio::AsyncFinder;
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    let adapter = AsyncFinder::new(Finder::new(ndi, &FinderOptions::new()).unwrap());
    for cancel in [true, false] {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let blocker = ::tokio::task::spawn_blocking(move || {
            started_tx.send(()).unwrap();
            released_wait(release_rx);
        });
        started_rx.recv_timeout(PATIENCE).unwrap();
        let before = events()
            .lines()
            .filter(|line| *line == "finder-snapshot")
            .count();
        let mut pending = Box::pin(adapter.find_source(Duration::from_millis(20), |_| false));
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(pending.as_mut().poll(&mut context), Poll::Pending));
        if cancel {
            drop(pending);
            release_tx.send(()).unwrap();
            blocker.await.unwrap();
            ::tokio::task::spawn_blocking(|| {}).await.unwrap();
            assert_eq!(
                events()
                    .lines()
                    .filter(|line| *line == "finder-snapshot")
                    .count(),
                before
            );
        } else {
            thread::sleep(Duration::from_millis(25));
            release_tx.send(()).unwrap();
            assert!(pending.await.unwrap().is_none());
            blocker.await.unwrap();
            assert_eq!(
                events()
                    .lines()
                    .filter(|line| *line == "finder-snapshot")
                    .count(),
                before + 1
            );
            assert!(!events().contains("finder-wait-begin"));
        }
    }
    // Exercise a real discovery predicate, including resuming its panic payload.
    let adapter = adapter.clone();
    let task = ::tokio::spawn(async move {
        adapter
            .find_source(Duration::ZERO, |_| std::panic::panic_any(42_u32))
            .await
    });
    let payload = task.await.unwrap_err().into_panic();
    assert_eq!(*payload.downcast::<u32>().unwrap(), 42);
}

#[cfg(feature = "tokio")]
fn released_wait(release: mpsc::Receiver<()>) {
    release.recv_timeout(PATIENCE).unwrap();
}

#[cfg(any(feature = "tokio", feature = "smol"))]
async fn async_discovery<R: grafton_ndi::asynchronous::AsyncRuntime>(
    ndi: &Ndi,
    publish: impl Fn() + Sync,
) {
    use grafton_ndi::{asynchronous::AsyncFinder, SourceCache};
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    fn count(label: &str) -> usize {
        events().lines().filter(|line| *line == label).count()
    }
    fn wait_for_idle() {
        let deadline = Instant::now() + Duration::from_millis(150);
        while count("finder-wait-end") != count("finder-wait-begin") {
            assert!(
                Instant::now() < deadline,
                "an abandoned native wait stayed active: {}",
                events()
            );
            thread::sleep(Duration::from_millis(1));
        }
    }
    let adapter = AsyncFinder::<R>::new(Finder::new(ndi, &FinderOptions::new()).unwrap());
    assert!(adapter
        .find_source_by_name("OTHER (Unrelated)", Duration::ZERO)
        .await
        .unwrap()
        .is_some());
    assert_eq!(adapter.find_sources(Duration::ZERO).await.unwrap().len(), 1);
    let mut watch = adapter.watch();
    assert_eq!(watch.current_sources().await.unwrap().len(), 1);
    // The worker may finish and publish a report, but a dropped future must
    // leave the cursor uncommitted even when its result is already ready.
    let before = count("finder-wait-begin");
    let mut abandoned = Box::pin(watch.wait_for_change(Duration::from_secs(3)));
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(
        abandoned.as_mut().poll(&mut context),
        Poll::Pending
    ));
    wait_for_entries("finder-wait-begin", before + 1);
    publish();
    wait_for_entries("finder-wait-end", before + 1);
    drop(abandoned);
    assert!(watch.wait_for_change(Duration::ZERO).await.unwrap());
    assert_eq!(watch.current_sources().await.unwrap().len(), 2);
    assert!(!watch.wait_for_change(Duration::ZERO).await.unwrap());
    // Synchronous followers share the async leader's report without another
    // native wait, and the follower does not require an async runtime.
    let before = count("finder-wait-begin");
    let mut pending = Box::pin(watch.wait_for_change(Duration::from_secs(3)));
    assert!(matches!(pending.as_mut().poll(&mut context), Poll::Pending));
    wait_for_entries("finder-wait-begin", before + 1);
    let mut sync_watch = adapter.finder().watch();
    thread::scope(|scope| {
        let follower = scope.spawn(|| sync_watch.wait_for_change(PATIENCE).unwrap());
        publish();
        assert!(follower.join().unwrap());
    });
    assert!(pending.await.unwrap());
    assert_eq!(count("finder-wait-begin"), before + 1);

    let before = count("finder-wait-begin");
    let mut abandoned = Box::pin(adapter.find_source(Duration::from_secs(3), |_| false));
    assert!(matches!(
        abandoned.as_mut().poll(&mut context),
        Poll::Pending
    ));
    wait_for_entries("finder-wait-begin", before + 1);
    drop(abandoned);
    wait_for_idle();

    let cache = AsyncFinder::<R, SourceCache>::new(SourceCache::new(
        Finder::new(ndi, &FinderOptions::new()).unwrap(),
    ));
    let before = count("finder-wait-begin");
    let mut abandoned = Box::pin(cache.find_sources_by_host("STUDIO", Duration::from_secs(3)));
    assert!(matches!(
        abandoned.as_mut().poll(&mut context),
        Poll::Pending
    ));
    wait_for_entries("finder-wait-begin", before + 1);
    drop(abandoned);
    wait_for_idle();
    // Let publication/cleanup finish after the native call returned.
    for _ in 0..20 {
        thread::yield_now();
    }
    assert!(
        cache.cache().is_empty(),
        "an incomplete window must not fill the cache"
    );
    assert_eq!(
        cache
            .find_sources_by_host("STUDIO", Duration::ZERO)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(cache.cache().len(), 1);
    let calls = count("finder-wait-begin");
    assert_eq!(
        cache
            .find_sources_by_host("STUDIO", Duration::ZERO)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(count("finder-wait-begin"), calls);
    // A failed async snapshot also leaves reports published by another
    // consumer outstanding, just like the synchronous SourceWatch.
    publish();
    assert!(adapter
        .finder()
        .watch()
        .wait_for_change(Duration::ZERO)
        .unwrap());
    let directory =
        PathBuf::from(std::env::var_os("GRAFTON_FAKE_NDI_LOG").unwrap()).with_extension("gate");
    fs::create_dir(&directory).unwrap();
    std::env::set_var("GRAFTON_FAKE_FINDER_GATE", &directory);
    fs::write(directory.join("fail-snapshot"), b"").unwrap();
    assert!(watch.current_sources().await.is_err());
    assert!(watch.wait_for_change(Duration::ZERO).await.unwrap());
    assert_eq!(watch.current_sources().await.unwrap().len(), 4);
    assert!(!watch.wait_for_change(Duration::ZERO).await.unwrap());
    assert!(events()
        .lines()
        .filter_map(|line| line.strip_prefix("finder-wait-timeout:"))
        .all(|timeout| timeout.parse::<u32>().unwrap() <= 100));
    no_overlapping_waits("finder");
}

// A failed snapshot must acknowledge neither a report another consumer already
// published nor a report still waiting in the SDK.
fn failed_snapshot(ndi: &Ndi, publish: impl Fn(), consumed: bool) {
    let finder = Finder::new(ndi, &FinderOptions::new()).unwrap();
    let mut watch = finder.watch();
    assert_eq!(watch.current_sources().unwrap().len(), 1);
    publish();
    if consumed {
        assert!(finder.watch().wait_for_change(Duration::ZERO).unwrap());
    }
    let directory =
        PathBuf::from(std::env::var_os("GRAFTON_FAKE_NDI_LOG").unwrap()).with_extension("gate");
    fs::create_dir(&directory).unwrap();
    std::env::set_var("GRAFTON_FAKE_FINDER_GATE", &directory);
    fs::write(directory.join("fail-snapshot"), b"").unwrap();
    assert!(matches!(
        watch.current_sources(),
        Err(grafton_ndi::Error::UnexpectedSdkResult(
            grafton_ndi::SdkResultDefect::MalformedDiscoveryData(
                grafton_ndi::DiscoveryDefect::NullArray
            )
        ))
    ));
    assert!(watch.wait_for_change(Duration::ZERO).unwrap());
    assert_eq!(watch.current_sources().unwrap().len(), 2);
    assert!(!watch.wait_for_change(Duration::ZERO).unwrap());
}

// Publish and consume a native report while the snapshot itself is blocked.
// Recording the cursor after copying would silently acknowledge that report.
fn snapshot_ordering(ndi: &Ndi, publish: impl Fn() + Sync) {
    let finder = Finder::new(ndi, &FinderOptions::new()).unwrap();
    let mut watch = finder.watch();
    let directory =
        PathBuf::from(std::env::var_os("GRAFTON_FAKE_NDI_LOG").unwrap()).with_extension("gate");
    fs::create_dir(&directory).unwrap();
    std::env::set_var("GRAFTON_FAKE_FINDER_GATE", &directory);
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            tx.send(finder.watch().wait_for_change(PATIENCE).unwrap())
                .unwrap()
        });
        wait_for_entries("finder-wait-begin", 1);
        fs::write(directory.join("block"), b"").unwrap();
        let directory = &directory;
        let publish = &publish;
        let control = scope.spawn(move || {
            let end = Instant::now() + PATIENCE;
            while !directory.join("entered").exists() {
                assert!(Instant::now() < end, "snapshot did not enter its gate");
                thread::yield_now();
            }
            publish();
            assert!(rx.recv_timeout(PATIENCE).unwrap());
            fs::write(directory.join("release"), b"").unwrap();
        });
        assert_eq!(watch.current_sources().unwrap().len(), 2);
        control.join().unwrap();
    });
    assert!(watch.wait_for_change(Duration::ZERO).unwrap());
    assert!(!watch.wait_for_change(Duration::ZERO).unwrap());
    no_overlapping_waits("finder");
}

#[cfg(feature = "advanced")]
fn keyframe(ndi: &Ndi, clear_after_query: bool) {
    use grafton_ndi::{
        advanced::{VideoCodec, VideoPacketFormat},
        capabilities::RuntimeFeature,
        Receiver, ReceiverOptions, Sender, SenderOptions, Source,
    };
    if let Err(error) = ndi.require_feature(RuntimeFeature::KeyframeDemand) {
        eprintln!("skipped: keyframe change fixture needs Advanced declarations: {error}");
        return;
    }
    let sender = Sender::new(ndi, &SenderOptions::new("keyframe fixture")).unwrap();
    let receiver = Receiver::new(ndi, &ReceiverOptions::new(Source::new("").unwrap())).unwrap();
    let h264 = VideoPacketFormat::new(VideoCodec::H264, 64, 32).unwrap();
    let different_size = VideoPacketFormat::new(VideoCodec::H264, 128, 64).unwrap();
    if clear_after_query {
        std::env::set_var("GRAFTON_FAKE_CLEAR_KEYFRAME_AFTER_QUERY", "1");
    }
    let formats = if clear_after_query {
        vec![Some(h264), Some(different_size)]
    } else {
        vec![None, None, Some(h264), Some(different_size)]
    };
    let waiters = formats.len();
    let ready = Barrier::new(waiters + 1);
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        for format in formats {
            let (sender, ready, tx) = (&sender, &ready, tx.clone());
            scope.spawn(move || {
                ready.wait();
                tx.send(sender.wait_for_keyframe_change(format, PATIENCE).unwrap())
                    .unwrap();
            });
        }
        ready.wait();
        let end = Instant::now() + PATIENCE;
        while events()
            .lines()
            .filter(|line| line.starts_with("keyframe-") && line.ends_with("-wait-begin"))
            .count()
            < 1
        {
            assert!(
                Instant::now() < end,
                "keyframe waiters did not enter: {}",
                events()
            );
            thread::sleep(Duration::from_millis(1));
        }
        thread::sleep(Duration::from_millis(40));
        receiver.request_keyframe().unwrap();
        for _ in 0..waiters {
            assert!(rx
                .recv_timeout(Duration::from_secs(1))
                .expect("a keyframe waiter lost the update"));
        }
    });
    no_overlapping_waits("keyframe-");
}
