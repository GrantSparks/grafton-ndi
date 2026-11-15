//! Concurrent discovery lookups receive the same native invalidations.
#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(runtime, name);

use grafton_ndi::{Finder, FinderOptions, Ndi, Sender, SenderOptions};
use std::{
    sync::{mpsc, Barrier},
    thread,
    time::{Duration, Instant},
};

const PATIENCE: Duration = Duration::from_secs(15);

fn isolated_finder(ndi: &Ndi, group: &str) -> Finder {
    Finder::new(
        ndi,
        &FinderOptions::new().groups(group).extra_ips("127.0.0.1"),
    )
    .unwrap()
}

fn drain_startup(finder: &Finder, ndi: &Ndi) {
    let deadline = Instant::now() + PATIENCE;
    let mut watch = finder.watch();
    while watch.wait_for_change(Duration::from_millis(500)).unwrap() {
        assert!(
            Instant::now() < deadline,
            "startup change reports never settled: {:?}",
            ndi.info()
        );
    }
}

/// Poll snapshots only: a native wait here would consume the report under test.
fn wait_visible(finder: &Finder, name: &str, ndi: &Ndi) {
    let suffix = format!("({name})");
    let deadline = Instant::now() + PATIENCE;
    loop {
        if finder
            .current_sources()
            .unwrap()
            .iter()
            .any(|source| source.name().ends_with(&suffix))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "sender {name:?} was not discovered: {:?}",
            ndi.info()
        );
        thread::sleep(Duration::from_millis(20));
    }
}

/// Retention: SourceWatch records its generation before a snapshot and waits
/// afterwards. A native change must survive that interval without a waiter.
#[test]
fn a_report_waits_for_the_next_native_wait() {
    let Some(ndi) = common::ndi() else { return };
    let group = common::unique_name("retained-report");
    let finder = isolated_finder(&ndi, &group);
    let observer = isolated_finder(&ndi, &group);
    drain_startup(&finder, &ndi);
    let mut watch = finder.watch();
    watch.current_sources().unwrap();
    let name = common::unique_name("retained-source");
    let _sender = Sender::new(&ndi, &SenderOptions::new(&name).groups(&group)).unwrap();
    wait_visible(&observer, &name, &ndi);
    // Neither a snapshot nor a native wait touches `finder` while discovery
    // learns the change. The independent observer establishes visibility.
    thread::sleep(Duration::from_secs(1));
    assert!(
        watch.wait_for_change(Duration::ZERO).unwrap(),
        "a change without an active native waiter was lost: {:?}",
        ndi.info()
    );
}

/// Snapshot independence: another SourceCache consumer's snapshot must not
/// clear a pending native report that this SourceWatch still needs.
#[test]
fn another_consumers_snapshot_leaves_the_report_for_the_next_wait() {
    let Some(ndi) = common::ndi() else { return };
    let group = common::unique_name("snapshot-report");
    let finder = isolated_finder(&ndi, &group);
    drain_startup(&finder, &ndi);
    let mut watch = finder.watch();
    watch.current_sources().unwrap();
    let name = common::unique_name("snapshot-source");
    let _sender = Sender::new(&ndi, &SenderOptions::new(&name).groups(&group)).unwrap();
    wait_visible(&finder, &name, &ndi);
    assert!(
        watch.wait_for_change(Duration::ZERO).unwrap(),
        "another consumer's snapshot cleared the pending report: {:?}",
        ndi.info()
    );
}

/// Consumption: ChangeFeed publishes each native `true` as a generation, so
/// a wait must consume its report even when no snapshot follows it.
#[test]
fn a_native_wait_consumes_the_report_it_returns() {
    let Some(ndi) = common::ndi() else { return };
    let group = common::unique_name("consumed-report");
    let finder = isolated_finder(&ndi, &group);
    drain_startup(&finder, &ndi);
    let name = common::unique_name("consumed-source");
    let _sender = thread::scope(|scope| {
        let mut watch = finder.watch();
        let wait = scope.spawn(move || watch.wait_for_change(PATIENCE).unwrap());
        thread::sleep(Duration::from_millis(200));
        let sender = Sender::new(&ndi, &SenderOptions::new(&name).groups(&group)).unwrap();
        assert!(wait.join().unwrap(), "no change report: {:?}", ndi.info());
        sender
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    // Fresh cursors observe the current generation, forcing another native
    // query. Reannouncements may produce a few additional reports first.
    while finder.watch().wait_for_change(Duration::ZERO).unwrap() {
        assert!(
            Instant::now() < deadline,
            "native waits repeat a report until a snapshot consumes it: {:?}",
            ndi.info()
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn concurrent_source_lookups_return_promptly_for_successive_announcements() {
    let Some(ndi) = common::ndi() else { return };
    let finder = Finder::new(&ndi, &FinderOptions::new().extra_ips("127.0.0.1")).unwrap();
    for round in 0..3 {
        let names = [
            common::unique_name(&format!("broadcast-{round}-A")),
            common::unique_name(&format!("broadcast-{round}-B")),
        ];
        let ready = Barrier::new(3);
        let (tx, rx) = mpsc::channel();
        thread::scope(|scope| {
            for (index, name) in names.iter().enumerate() {
                let (finder, ready, tx) = (&finder, &ready, tx.clone());
                scope.spawn(move || {
                    let suffix = format!("({name})");
                    ready.wait();
                    let source = finder
                        .find_source(Duration::from_secs(8), |source| {
                            source.name().ends_with(&suffix)
                        })
                        .unwrap();
                    tx.send((index, source)).unwrap();
                });
            }
            ready.wait();
            thread::sleep(Duration::from_millis(100));
            let first = Sender::new(&ndi, &SenderOptions::new(&names[0])).unwrap();
            let (index, source) = rx
                .recv_timeout(Duration::from_secs(2))
                .expect("a lookup missed the first source-list update");
            assert_eq!(index, 0);
            assert!(source.is_some());
            let second = Sender::new(&ndi, &SenderOptions::new(&names[1])).unwrap();
            let (index, source) = rx
                .recv_timeout(Duration::from_secs(2))
                .expect("a lookup missed the second source-list update");
            assert_eq!(index, 1);
            assert!(source.is_some());
            drop((first, second));
        });
    }
}
