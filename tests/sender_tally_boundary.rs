//! Polling must retain the SDK's one-shot tally change for one waiter.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

#[path = "support/fixture.rs"]
mod fixture;
#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;

use fixture::FixtureDir;
use grafton_ndi::{
    Ndi, Receiver, ReceiverOptions, RuntimeOptions, Sender, SenderOptions, Source, Tally,
};
use runtime_fixture::FakeRuntime;
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

fn events() -> String {
    fs::read_to_string(std::env::var_os("GRAFTON_FAKE_NDI_LOG").unwrap()).unwrap_or_default()
}

fn await_native_wait(count: usize) {
    let end = Instant::now() + Duration::from_secs(3);
    while events()
        .lines()
        .filter(|line| *line == "tally-wait-begin")
        .count()
        < count
    {
        assert!(
            Instant::now() < end,
            "native tally wait did not start: {}",
            events()
        );
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn polls_preserve_changes_and_never_enter_a_waiters_native_query() {
    if let Some(library) = std::env::var_os("GRAFTON_TALLY_LIBRARY") {
        let ndi =
            Ndi::with_options(&RuntimeOptions::new().library_path(PathBuf::from(library))).unwrap();
        let sender = Sender::new(&ndi, &SenderOptions::new("fake tally")).unwrap();
        let receiver = Receiver::new(&ndi, &ReceiverOptions::new(Source::new(""))).unwrap();
        // Poll first, then wait: the report must be returned exactly once.
        receiver.set_tally(Tally::new(true, false));
        for _ in 0..3 {
            assert_eq!(sender.tally(), Tally::new(true, false));
        }
        assert_eq!(
            sender.wait_for_tally_change(Duration::ZERO).unwrap(),
            Some(Tally::new(true, false))
        );
        assert_eq!(sender.wait_for_tally_change(Duration::ZERO).unwrap(), None);

        // Several changes seen only by polls coalesce into the latest state.
        receiver.set_tally(Tally::new(false, true));
        assert_eq!(sender.tally(), Tally::new(false, true));
        receiver.set_tally(Tally::new(true, false));
        assert_eq!(sender.tally(), Tally::new(true, false));
        assert_eq!(
            sender.wait_for_tally_change(Duration::ZERO).unwrap(),
            Some(Tally::new(true, false))
        );
        assert_eq!(sender.wait_for_tally_change(Duration::ZERO).unwrap(), None);

        thread::scope(|scope| {
            let waiting = scope.spawn(|| {
                sender
                    .wait_for_tally_change(Duration::from_secs(3))
                    .unwrap()
            });
            await_native_wait(1);
            let start = Instant::now();
            for _ in 0..1000 {
                assert_eq!(sender.tally(), Tally::new(true, false));
            }
            assert!(
                start.elapsed() < Duration::from_secs(1),
                "tally polls blocked behind the native waiter"
            );
            receiver.set_tally(Tally::new(false, true));
            assert_eq!(waiting.join().unwrap(), Some(Tally::new(false, true)));
        });
        assert_eq!(sender.wait_for_tally_change(Duration::ZERO).unwrap(), None);

        // A second waiter spends its own budget on the lock, without calling
        // into the SDK beside the first or consuming the first waiter's change.
        thread::scope(|scope| {
            let first = scope.spawn(|| {
                sender
                    .wait_for_tally_change(Duration::from_secs(3))
                    .unwrap()
            });
            await_native_wait(2);
            let (tx, rx) = mpsc::channel();
            let sender = &sender;
            scope.spawn(move || {
                tx.send(
                    sender
                        .wait_for_tally_change(Duration::from_millis(30))
                        .unwrap(),
                )
                .unwrap()
            });
            assert_eq!(
                rx.recv_timeout(Duration::from_secs(1))
                    .expect("lock contention exceeded the waiter's budget"),
                None
            );
            assert_eq!(
                events()
                    .lines()
                    .filter(|line| *line == "tally-wait-begin")
                    .count(),
                2
            );
            receiver.set_tally(Tally::new(true, true));
            assert_eq!(first.join().unwrap(), Some(Tally::new(true, true)));
        });
        assert_eq!(sender.wait_for_tally_change(Duration::ZERO).unwrap(), None);
        // Two waits can also each receive a distinct successive change.
        let ready = std::sync::Barrier::new(3);
        thread::scope(|scope| {
            let (tx, rx) = mpsc::channel();
            for _ in 0..2 {
                let (ready, tx, sender) = (&ready, tx.clone(), &sender);
                scope.spawn(move || {
                    ready.wait();
                    tx.send(
                        sender
                            .wait_for_tally_change(Duration::from_secs(3))
                            .unwrap(),
                    )
                    .unwrap();
                });
            }
            ready.wait();
            await_native_wait(3);
            receiver.set_tally(Tally::new(true, false));
            assert_eq!(
                rx.recv_timeout(Duration::from_secs(1)).unwrap(),
                Some(Tally::new(true, false))
            );
            await_native_wait(4);
            receiver.set_tally(Tally::new(false, true));
            assert_eq!(
                rx.recv_timeout(Duration::from_secs(1)).unwrap(),
                Some(Tally::new(false, true))
            );
        });
        let mut active = false;
        for event in events().lines() {
            match event {
                "tally-wait-begin" => {
                    assert!(!active, "overlapping native waits");
                    active = true;
                }
                "tally-wait-end" => {
                    assert!(active);
                    active = false;
                }
                "tally-poll" => {
                    assert!(!active, "a tally poll consumed the waiter's native report")
                }
                _ => {}
            }
        }
        assert!(!active);
        return;
    }
    let directory = FixtureDir::create("sender-tally");
    let library = directory.path().join("libndi.so");
    FakeRuntime {
        cfgs: vec!["fake_objects", "fake_tally"],
        ..Default::default()
    }
    .build(&library);
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "polls_preserve_changes_and_never_enter_a_waiters_native_query",
            "--nocapture",
        ])
        .env("GRAFTON_TALLY_LIBRARY", &library)
        .env("GRAFTON_FAKE_NDI_LOG", directory.path().join("tally.log"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
