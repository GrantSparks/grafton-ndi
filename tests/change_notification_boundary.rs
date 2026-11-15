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
    ];
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
    let receiver = Receiver::new(ndi, &ReceiverOptions::new(Source::new(""))).unwrap();
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
                tx.send(sender.wait_for_keyframe_change(PATIENCE, format).unwrap())
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
