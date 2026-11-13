//! Lookup budgets include contention with a concurrent native snapshot/copy.
#![cfg(target_os = "linux")]
#[path = "support/fixture.rs"]
mod fixture;

use fixture::{FakeRuntime, FixtureDir};
use grafton_ndi::{Finder, FinderOptions, Ndi, RuntimeOptions, SourceCache};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
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
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.0.join(file).exists() {
            assert!(
                Instant::now() < deadline,
                "native gate {file} was not entered"
            );
            thread::sleep(Duration::from_millis(1));
        }
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

fn exercise(finder: &Finder, gate: &Gate) {
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
    let (sources, cached) = contended(finder, gate, |finder| {
        let cache = SourceCache::new(finder);
        (
            cache.sources_by_host("studio", Duration::ZERO).unwrap(),
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
    let sources = expired_during_wait(finder, gate, |finder| {
        SourceCache::new(finder)
            .sources_by_host("studio", Duration::from_millis(20))
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
        scope.spawn(|| finder.wait_for_sources(Duration::from_secs(5)).unwrap());
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
        exercise(&finder, &gate);
        return;
    }
    let directory = FixtureDir::create("finder-deadlines");
    let library = directory.path().join("finder.so");
    FakeRuntime::new()
        .version("6.4.0.2")
        .with("fake_finder")
        .build(&library);
    run_child(directory.path(), &library);
}

fn run_child(directory: &Path, library: &Path) {
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "finder_lookup_snapshot_contention_spends_one_wait_budget",
            "--nocapture",
        ])
        .env("GRAFTON_FAKE_FINDER_RUNTIME", library)
        .env("GRAFTON_FAKE_FINDER_CASE", "deadline")
        .env("GRAFTON_FAKE_FINDER_GATE", directory)
        .env("GRAFTON_FAKE_NDI_LOG", directory.join("calls.log"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
