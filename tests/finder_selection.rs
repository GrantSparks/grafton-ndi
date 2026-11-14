//! Configured, staggered selection and set-valued scoped caching at the native boundary.
#![cfg(target_os = "linux")]
#[path = "support/fixture.rs"]
mod fixture;

use fixture::{FakeRuntime, FixtureDir};
use grafton_ndi::{
    Error, Finder, FinderOptions, Ndi, RuntimeOptions, Source, SourceAddress, SourceCache,
};
use std::{
    path::PathBuf,
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

#[test]
fn owning_cache_retains_discovery_scope_and_runtime_across_server_workers() {
    if std::env::var_os("GRAFTON_OWNED_CACHE_CHILD").is_some() {
        let cache = {
            let ndi = Ndi::with_options(&RuntimeOptions::new().library_path(PathBuf::from(
                std::env::var_os("GRAFTON_FAKE_FINDER_RUNTIME").unwrap(),
            )))
            .unwrap();
            SourceCache::new(finder(&ndi, "Remote"))
        };
        assert!(Ndi::is_running(), "the cache keeps its runtime initialized");
        let feeds = cache
            .sources_by_host("studio", Duration::from_millis(80))
            .unwrap();
        assert_eq!(feeds.len(), 4);
        assert!(feeds.iter().all(|feed| feed.matches_host("192.0.2.20")));
        let selected = cache
            .finder()
            .find_source_by_name("STUDIO (Camera 2)", Duration::ZERO)
            .unwrap()
            .unwrap();
        assert!(feeds.contains(&selected));

        let state = Arc::new(cache);
        let worker = Arc::clone(&state);
        drop(state);
        std::thread::spawn(move || {
            assert!(Ndi::is_running());
            assert_eq!(
                worker.sources_by_host("STUDIO.", Duration::ZERO).unwrap(),
                feeds
            );
            worker.invalidate("studio");
            assert!(worker.is_empty());
            assert_eq!(
                worker
                    .sources_by_host("studio", Duration::ZERO)
                    .unwrap()
                    .len(),
                4
            );
            worker.clear();
            assert!(worker.is_empty());
        })
        .join()
        .unwrap();
        assert!(
            !Ndi::is_running(),
            "the last cache owner releases its runtime"
        );
        let calls =
            std::fs::read_to_string(std::env::var_os("GRAFTON_FAKE_NDI_LOG").unwrap()).unwrap();
        for operation in ["initialize", "finder-create", "finder-destroy", "destroy"] {
            assert_eq!(
                calls.lines().filter(|line| *line == operation).count(),
                1,
                "{calls}"
            );
        }
        assert!(calls.find("finder-destroy").unwrap() < calls.rfind("destroy").unwrap());
        return;
    }
    let directory = FixtureDir::create("owned-source-cache");
    let library = directory.path().join("finder.so");
    FakeRuntime::new()
        .version("6.4.0.2")
        .with("fake_finder")
        .build(&library);
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "owning_cache_retains_discovery_scope_and_runtime_across_server_workers",
            "--nocapture",
        ])
        .env("GRAFTON_OWNED_CACHE_CHILD", "1")
        .env("GRAFTON_FAKE_FINDER_CASE", "selection-forward")
        .env("GRAFTON_FAKE_FINDER_RUNTIME", &library)
        .env("GRAFTON_FAKE_NDI_LOG", directory.path().join("calls.log"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn finder(ndi: &Ndi, group: &str) -> Finder {
    Finder::new(
        ndi,
        &FinderOptions::new()
            .groups(group)
            .extra_ips("192.0.2.10,192.0.2.20")
            .show_local_sources(false),
    )
    .unwrap()
}

#[test]
fn exact_selection_preserves_scope_and_complete_host_sets() {
    if let Ok(scenario) = std::env::var("GRAFTON_FAKE_FINDER_CASE") {
        let ndi = Ndi::with_options(&RuntimeOptions::new().library_path(PathBuf::from(
            std::env::var_os("GRAFTON_FAKE_FINDER_RUNTIME").unwrap(),
        )))
        .unwrap();
        let studio = finder(&ndi, "Studio");
        assert!(studio
            .find_source_by_name("STUDIO (Camera 2)", Duration::ZERO)
            .unwrap()
            .is_none());
        let before = Instant::now();
        let selected = studio
            .find_source_by_name("STUDIO (Camera 2)", Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert!(
            before.elapsed() < Duration::from_secs(1),
            "selection waited out its budget"
        );
        assert_eq!(
            selected.address(),
            Some(&SourceAddress::Ip("192.0.2.10:5962".into()))
        );
        assert!(studio
            .find_source_by_name("camera 2", Duration::ZERO)
            .unwrap()
            .is_none());
        for name in ["", "bad\0name"] {
            assert!(matches!(
                studio.find_source_by_name(name, Duration::ZERO),
                Err(Error::InvalidInput(_))
            ));
        }
        assert!(matches!(
            studio.find_source_by_name(selected.name(), Duration::MAX),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            studio.find_source(Duration::MAX, |_| panic!("invalid wait")),
            Err(Error::InvalidInput(_))
        ));
        // Reentrant snapshots prove predicates run after releasing the SDK snapshot lock.
        let target = SourceAddress::Ip("192.0.2.10:5962".into());
        let program = studio
            .find_source(Duration::ZERO, |source| {
                assert!(!studio.current_sources().unwrap().is_empty());
                source.name() == "STUDIO (Program)" && source.address() == Some(&target)
            })
            .unwrap()
            .unwrap();
        assert_eq!(program.address(), Some(&target));
        let first_program = studio
            .find_source_by_name("STUDIO (Program)", Duration::ZERO)
            .unwrap()
            .unwrap();
        let expected_port = if scenario == "selection-reverse" {
            5962
        } else {
            5961
        };
        assert_eq!(first_program.address().unwrap().port(), Some(expected_port));
        let before = Instant::now();
        assert!(studio
            .find_source_by_name("missing", Duration::from_millis(40))
            .unwrap()
            .is_none());
        assert!(before.elapsed() >= Duration::from_millis(40));
        let cache = SourceCache::new(studio);
        let feeds = cache.sources_by_host("STUDIO", Duration::ZERO).unwrap();
        assert_eq!(
            feeds.len(),
            4,
            "the host's distinct feeds and duplicate addresses survive"
        );
        assert!(feeds.contains(&selected));
        assert_eq!(
            cache.sources_by_host("studio.", Duration::ZERO).unwrap(),
            feeds
        );
        assert_eq!(cache.len(), 1);
        assert!(matches!(
            cache.sources_by_host("studio", Duration::MAX),
            Err(Error::InvalidInput(_))
        ));
        assert!(cache
            .sources_by_host("absent", Duration::ZERO)
            .unwrap()
            .is_empty());
        assert_eq!(cache.len(), 1, "empty sets are not cached");
        cache.invalidate("sTuDiO.");
        assert!(cache.is_empty());
        let remote = finder(&ndi, "Remote");
        let remote_cache = SourceCache::new(remote);
        let _ = remote_cache
            .finder()
            .find_source_by_name("STUDIO (Camera 2)", Duration::from_secs(1))
            .unwrap()
            .unwrap();
        let remote_feeds = remote_cache
            .sources_by_host("studio", Duration::ZERO)
            .unwrap();
        assert_eq!(remote_feeds.len(), 4);
        assert!(remote_feeds
            .iter()
            .all(|feed| feed.matches_host("192.0.2.20")));
        let studio_feeds = cache.sources_by_host("studio", Duration::ZERO).unwrap();
        assert!(studio_feeds
            .iter()
            .all(|feed| feed.matches_host("192.0.2.10")));
        // An initially incomplete host snapshot must enumerate the full window.
        let staggered = finder(&ndi, "Studio");
        let staggered_cache = SourceCache::new(staggered);
        let started = Instant::now();
        let all = staggered_cache
            .sources_by_host("studio", Duration::from_millis(80))
            .unwrap();
        assert!(started.elapsed() >= Duration::from_millis(80));
        assert_eq!(all.len(), 4);
        assert!(all.iter().any(|feed| feed == &selected));
        // A caller may deliberately request first match without implying host identity.
        let _: Option<Source> = cache
            .finder()
            .find_source(Duration::ZERO, |_| true)
            .unwrap();
        return;
    }
    let directory = FixtureDir::create("finder-selection");
    let library = directory.path().join("finder.so");
    FakeRuntime::new()
        .version("6.4.0.2")
        .with("fake_finder")
        .build(&library);
    for scenario in ["selection-forward", "selection-reverse"] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "exact_selection_preserves_scope_and_complete_host_sets",
                "--nocapture",
            ])
            .env("GRAFTON_FAKE_FINDER_CASE", scenario)
            .env("GRAFTON_FAKE_FINDER_RUNTIME", &library)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{scenario}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
