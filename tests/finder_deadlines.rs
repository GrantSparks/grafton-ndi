//! Lookup budgets include contention with a concurrent native snapshot/copy.
#![cfg(target_os = "linux")]
#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(child, fixture);
use common::TestChild;

#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;

use grafton_ndi::{Finder, FinderOptions, Ndi, RuntimeOptions, SourceCache};
use runtime_fixture::FakeRuntime;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

struct Gate(PathBuf);

impl Gate {
    fn reset(&self) {
        for file in [
            "block",
            "entered",
            "release",
            "hold-wait",
            "waiting",
            "wait-release",
        ] {
            let _ = fs::remove_file(self.0.join(file));
        }
    }

    fn write(&self, file: &str) {
        fs::write(self.0.join(file), b"gate").unwrap();
    }

    fn wait(&self, file: &str) {
        assert!(self.wait_for(file), "native gate {file} was not entered");
    }

    fn wait_for(&self, file: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.0.join(file).exists() {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(1));
        }
        true
    }

    fn snapshots(&self) -> usize {
        fs::read_to_string(self.0.join("calls.log"))
            .unwrap_or_default()
            .lines()
            .filter(|line| *line == "finder-snapshot")
            .count()
    }
}

/// Unblocks native workers during unwinding before scoped threads are joined.
struct Release<'a>(&'a Gate);
impl Drop for Release<'_> {
    fn drop(&mut self) {
        for file in ["release", "wait-release"] {
            let _ = fs::write(self.0 .0.join(file), b"release");
        }
    }
}

fn contended<T: Send>(
    finder: &Finder,
    gate: &Gate,
    operation: impl FnOnce(&Finder) -> T + Send,
) -> T {
    gate.reset();
    gate.write("block");
    thread::scope(|scope| {
        let release = Release(gate);
        let owner = scope.spawn(|| finder.current_sources().unwrap());
        gate.wait("entered");
        let calls = gate.snapshots();
        let (tx, rx) = mpsc::channel();
        scope.spawn(move || tx.send(operation(finder)).unwrap());
        // The native snapshot stays gated until the operation returns. Thus this
        // checks lock-budget completion without depending on exact scheduling.
        let result = rx
            .recv_timeout(Duration::from_secs(1))
            .expect("lookup waited for the gated snapshot beyond its own budget");
        assert_eq!(
            gate.snapshots(),
            calls,
            "busy lookup entered another native snapshot"
        );
        drop(release);
        assert_eq!(owner.join().unwrap().len(), 1);
        result
    })
}

fn expired_during_wait<T: Send>(
    finder: &Finder,
    gate: &Gate,
    operation: impl FnOnce(&Finder) -> T + Send,
) -> T {
    gate.reset();
    gate.write("hold-wait");
    thread::scope(|scope| {
        let release = Release(gate);
        let (tx, rx) = mpsc::channel();
        scope.spawn(move || tx.send(operation(finder)).unwrap());
        gate.wait("waiting"); // The lookup already took its initial snapshot.
        gate.write("block");
        let owner = scope.spawn(|| finder.current_sources().unwrap());
        gate.wait("entered");
        let calls = gate.snapshots();
        // Spend the lookup's 20 ms budget while its native wait is gated.
        thread::sleep(Duration::from_millis(30));
        gate.write("wait-release");
        let result = rx
            .recv_timeout(Duration::from_secs(1))
            .expect("expired lookup waited for the gated snapshot");
        assert_eq!(
            gate.snapshots(),
            calls,
            "expired busy attempt entered native code"
        );
        drop(release);
        owner.join().unwrap();
        result
    })
}

fn exercise(cache: &SourceCache, gate: &Gate) {
    let finder = cache.finder();
    assert!(contended(finder, gate, |finder| finder
        .find_source(Duration::ZERO, |_| true)
        .unwrap())
    .is_none());
    assert!(contended(finder, gate, |finder| finder
        .find_source_by_name("STUDIO (Camera)", Duration::ZERO)
        .unwrap())
    .is_none());
    assert!(contended(finder, gate, |finder| finder
        .find_sources(Duration::ZERO)
        .unwrap())
    .is_empty());
    let (sources, cached) = contended(finder, gate, |_| {
        (
            cache
                .find_sources_by_host("studio", Duration::ZERO)
                .unwrap(),
            cache.len(),
        )
    });
    assert!(sources.is_empty());
    assert_eq!(cached, 0);
    let (result, waited) = contended(finder, gate, |finder| {
        let start = Instant::now();
        (
            finder
                .find_source(Duration::from_millis(40), |_| true)
                .unwrap(),
            start.elapsed(),
        )
    });
    assert!(result.is_none());
    assert!(
        waited >= Duration::from_millis(40),
        "bounded contention returned prematurely: {waited:?}"
    );
    assert!(expired_during_wait(finder, gate, |finder| finder
        .find_source(Duration::from_millis(20), |_| false)
        .unwrap())
    .is_none());
    let accumulated = expired_during_wait(finder, gate, |finder| {
        finder.find_sources(Duration::from_millis(20)).unwrap()
    });
    assert_eq!(accumulated.len(), 1);
    let sources = expired_during_wait(finder, gate, |_| {
        cache
            .find_sources_by_host("studio", Duration::from_millis(20))
            .unwrap()
    });
    assert_eq!(sources.len(), 1);

    // Ordinary current_sources keeps its blocking serialization; a lookup can
    // acquire the released lock and select a source within its remaining budget.
    gate.reset();
    gate.write("block");
    thread::scope(|scope| {
        let release = Release(gate);
        scope.spawn(|| finder.current_sources().unwrap());
        gate.wait("entered");
        let (tx, rx) = mpsc::channel();
        scope.spawn(move || {
            tx.send(
                finder
                    .find_source(Duration::from_secs(5), |_| true)
                    .unwrap(),
            )
            .unwrap()
        });
        assert!(matches!(
            rx.recv_timeout(Duration::from_millis(20)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        drop(release);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1))
                .unwrap()
                .unwrap()
                .name(),
            "STUDIO (Camera)"
        );
    });

    // A long native source-list wait holds no snapshot lock.
    gate.reset();
    gate.write("hold-wait");
    thread::scope(|scope| {
        let release = Release(gate);
        scope.spawn(|| {
            finder
                .watch()
                .wait_for_change(Duration::from_secs(5))
                .unwrap()
        });
        gate.wait("waiting");
        let (tx, rx) = mpsc::channel();
        scope.spawn(move || {
            tx.send(finder.find_source(Duration::ZERO, |_| true).unwrap())
                .unwrap()
        });
        assert!(rx.recv_timeout(Duration::from_secs(1)).unwrap().is_some());
        drop(release);
    });
    // Zero still takes one snapshot when the mutex is available, and a predicate
    // may safely reenter the ordinary blocking snapshot API after copying.
    assert!(finder
        .find_source(Duration::ZERO, |_| !finder
            .current_sources()
            .unwrap()
            .is_empty())
        .unwrap()
        .is_some());
}

#[test]
fn finder_lookup_snapshot_contention_spends_one_wait_budget() {
    if let Some(library) = std::env::var_os("GRAFTON_FAKE_FINDER_RUNTIME") {
        let ndi =
            Ndi::with_options(&RuntimeOptions::new().library_path(PathBuf::from(library))).unwrap();
        let finder = Finder::new(&ndi, &FinderOptions::new()).unwrap();
        let gate = Gate(PathBuf::from(
            std::env::var_os("GRAFTON_FAKE_FINDER_GATE").unwrap(),
        ));
        exercise(&SourceCache::new(finder), &gate);
        return;
    }
    let directory = common::FixtureDir::create("finder-deadlines");
    let library = directory.path().join("finder.so");
    FakeRuntime {
        cfgs: vec!["fake_finder"],
        version: Some("6.4.0.2"),
        ..Default::default()
    }
    .build(&library);
    run_child(directory.path(), &library);
}

fn run_child(directory: &Path, library: &Path) {
    TestChild::new("finder_lookup_snapshot_contention_spends_one_wait_budget")
        .env("GRAFTON_FAKE_FINDER_RUNTIME", library)
        .env("GRAFTON_FAKE_FINDER_CASE", "deadline")
        .env("GRAFTON_FAKE_FINDER_GATE", directory)
        .env("GRAFTON_FAKE_NDI_LOG", directory.join("calls.log"))
        .run();
}

#[cfg(any(feature = "tokio", feature = "smol"))]
mod async_predicate {
    use super::*;
    use grafton_ndi::asynchronous::{AsyncFinder, AsyncRuntime};
    use std::{
        future::Future,
        task::{Context, Waker},
    };

    const PATIENCE: Duration = Duration::from_secs(5);

    // This is dropped when the predicate leaves the worker, so the count
    // includes every call the worker made before and after cancellation.
    struct Probe {
        calls: usize,
        finished: mpsc::Sender<usize>,
    }

    impl Probe {
        fn called(&mut self) -> usize {
            self.calls += 1;
            self.calls
        }
    }

    impl Drop for Probe {
        fn drop(&mut self) {
            let _ = self.finished.send(self.calls);
        }
    }

    fn exercise<R: AsyncRuntime>(finder: Finder, gate: &Gate) {
        let adapter = AsyncFinder::<R>::new(finder);
        for during_snapshot in [true, false] {
            gate.reset();
            if during_snapshot {
                gate.write("block");
            }
            let release_snapshot = Release(gate);
            let snapshots = gate.snapshots();
            let (entered, entry) = mpsc::channel();
            let (release_predicate, resume) = mpsc::channel();
            let (finished, complete) = mpsc::channel();
            let mut probe = Probe { calls: 0, finished };
            let mut wait = Box::pin(adapter.find_source(Duration::from_secs(30), move |_| {
                if probe.called() == 1 && !during_snapshot {
                    let _ = entered.send(());
                    let _ = resume.recv();
                }
                false
            }));
            let poll = wait.as_mut().poll(&mut Context::from_waker(Waker::noop()));
            let at_gate = poll.is_pending()
                && if during_snapshot {
                    gate.wait_for("entered")
                } else {
                    entry.recv_timeout(PATIENCE).is_ok()
                };
            drop(wait);
            drop(release_snapshot);
            let _ = release_predicate.send(());
            assert!(poll.is_pending());
            assert!(at_gate, "the blocking worker did not reach its gate");
            assert_eq!(
                complete.recv_timeout(PATIENCE).unwrap(),
                usize::from(!during_snapshot)
            );
            assert_eq!(gate.snapshots(), snapshots + 1);
        }
    }

    #[test]
    fn cancelled_lookup_stops_before_the_next_predicate_call() {
        if let Ok(runtime) = std::env::var("GRAFTON_PREDICATE_CANCEL_RUNTIME") {
            let library = PathBuf::from(std::env::var_os("GRAFTON_FAKE_FINDER_RUNTIME").unwrap());
            let ndi = Ndi::with_options(&RuntimeOptions::new().library_path(library)).unwrap();
            let finder = Finder::new(&ndi, &FinderOptions::new()).unwrap();
            let gate = Gate(PathBuf::from(
                std::env::var_os("GRAFTON_FAKE_FINDER_GATE").unwrap(),
            ));
            match runtime.as_str() {
                #[cfg(feature = "tokio")]
                "tokio" => ::tokio::runtime::Builder::new_current_thread()
                    .build()
                    .unwrap()
                    .block_on(async {
                        exercise::<grafton_ndi::asynchronous::Tokio>(finder, &gate)
                    }),
                #[cfg(feature = "smol")]
                "smol" => ::smol::block_on(async {
                    exercise::<grafton_ndi::asynchronous::Smol>(finder, &gate)
                }),
                _ => panic!("unexpected async runtime: {runtime}"),
            }
            return;
        }

        let directory = common::FixtureDir::create("finder-predicate-cancel");
        let library = directory.path().join("finder.so");
        FakeRuntime {
            cfgs: vec!["fake_finder"],
            version: Some("6.4.0.2"),
            ..Default::default()
        }
        .build(&library);
        for (runtime, enabled) in [
            ("tokio", cfg!(feature = "tokio")),
            ("smol", cfg!(feature = "smol")),
        ] {
            if !enabled {
                continue;
            }
            let gate = common::FixtureDir::create("finder-predicate-gate");
            TestChild::new(
                "async_predicate::cancelled_lookup_stops_before_the_next_predicate_call",
            )
            .env("GRAFTON_PREDICATE_CANCEL_RUNTIME", runtime)
            .env("GRAFTON_FAKE_FINDER_RUNTIME", &library)
            .env("GRAFTON_FAKE_FINDER_CASE", "many-short")
            .env("GRAFTON_FAKE_FINDER_GATE", gate.path())
            .env("GRAFTON_FAKE_NDI_LOG", gate.path().join("calls.log"))
            .run_with_timeout(Duration::from_secs(20));
        }
    }
}
