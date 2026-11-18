//! Configured, staggered selection and set-valued scoped caching at the native boundary.
#![cfg(target_os = "linux")]
#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(child, fixture);
use common::TestChild;

#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;

use grafton_ndi::{
    Error, Finder, FinderOptions, Ndi, RuntimeOptions, Source, SourceAddress, SourceCache,
};
use runtime_fixture::{read_fake_log, FakeRuntime};
use std::{
    path::PathBuf,
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
            .find_sources_by_host("studio", Duration::from_millis(80))
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
                worker
                    .find_sources_by_host("STUDIO.", Duration::ZERO)
                    .unwrap(),
                feeds
            );
            worker.invalidate("studio");
            assert!(worker.is_empty());
            assert_eq!(
                worker
                    .find_sources_by_host("studio", Duration::ZERO)
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
        let calls = read_fake_log();
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
    let directory = common::FixtureDir::create("owned-source-cache");
    let library = directory.path().join("finder.so");
    FakeRuntime {
        cfgs: vec!["fake_finder"],
        version: Some("6.4.0.2"),
        ..Default::default()
    }
    .build(&library);
    TestChild::new("owning_cache_retains_discovery_scope_and_runtime_across_server_workers")
        .env("GRAFTON_OWNED_CACHE_CHILD", "1")
        .env("GRAFTON_FAKE_FINDER_CASE", "selection-forward")
        .env("GRAFTON_FAKE_FINDER_RUNTIME", &library)
        .env("GRAFTON_FAKE_NDI_LOG", directory.path().join("calls.log"))
        .run();
}

fn finder(ndi: &Ndi, group: &str) -> Finder {
    Finder::new(
        ndi,
        &FinderOptions::new()
            .groups([group])
            .extra_ips(["192.0.2.10", "192.0.2.20"])
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
            Some(&SourceAddress::new("192.0.2.10:5962").unwrap())
        );
        assert!(studio
            .find_source_by_name("camera 2", Duration::ZERO)
            .unwrap()
            .is_none());
        assert!(matches!(
            studio.find_source_by_name("", Duration::ZERO),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            studio.find_source_by_name("bad\0name", Duration::ZERO),
            Err(Error::InvalidCString(_))
        ));
        assert!(matches!(
            studio.find_source_by_name(selected.name(), Duration::MAX),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            studio.find_source(Duration::MAX, |_| panic!("invalid wait")),
            Err(Error::InvalidInput(_))
        ));
        // Reentrant snapshots prove predicates run after releasing the SDK snapshot lock.
        let target = SourceAddress::new("192.0.2.10:5962").unwrap();
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
        let feeds = cache
            .find_sources_by_host("STUDIO", Duration::ZERO)
            .unwrap();
        assert_eq!(
            feeds.len(),
            4,
            "the host's distinct feeds and duplicate addresses survive"
        );
        assert!(feeds.contains(&selected));
        assert_eq!(
            cache
                .find_sources_by_host("studio.", Duration::ZERO)
                .unwrap(),
            feeds
        );
        assert_eq!(cache.len(), 1);
        assert!(matches!(
            cache.find_sources_by_host("studio", Duration::MAX),
            Err(Error::InvalidInput(_))
        ));
        assert!(cache
            .find_sources_by_host("absent", Duration::ZERO)
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
            .find_sources_by_host("studio", Duration::ZERO)
            .unwrap();
        assert_eq!(remote_feeds.len(), 4);
        assert!(remote_feeds
            .iter()
            .all(|feed| feed.matches_host("192.0.2.20")));
        let studio_feeds = cache
            .find_sources_by_host("studio", Duration::ZERO)
            .unwrap();
        assert!(studio_feeds
            .iter()
            .all(|feed| feed.matches_host("192.0.2.10")));
        // An initially incomplete host snapshot must enumerate the full window.
        let staggered = finder(&ndi, "Studio");
        let staggered_cache = SourceCache::new(staggered);
        let started = Instant::now();
        let all = staggered_cache
            .find_sources_by_host("studio", Duration::from_millis(80))
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
    let directory = common::FixtureDir::create("finder-selection");
    let library = directory.path().join("finder.so");
    FakeRuntime {
        cfgs: vec!["fake_finder"],
        version: Some("6.4.0.2"),
        ..Default::default()
    }
    .build(&library);
    for scenario in ["selection-forward", "selection-reverse"] {
        TestChild::new("exact_selection_preserves_scope_and_complete_host_sets")
            .env("GRAFTON_FAKE_FINDER_CASE", scenario)
            .env("GRAFTON_FAKE_FINDER_RUNTIME", &library)
            .run();
    }
}

#[test]
fn cache_matches_only_the_outer_endpoint_host() {
    if let Some(library) = std::env::var_os("GRAFTON_OUTER_ENDPOINT_RUNTIME") {
        let ndi =
            Ndi::with_options(&RuntimeOptions::new().library_path(PathBuf::from(library))).unwrap();
        let cache = SourceCache::new(finder(&ndi, "Studio"));

        assert!(cache
            .find_sources_by_host("camera-b.local", Duration::ZERO)
            .unwrap()
            .is_empty());
        assert!(cache.is_empty());

        let expected = Source::new("OTHER (Feed)").unwrap().with_address(
            SourceAddress::new("camera-a.local:5960?next=https://camera-b.local:7443/live")
                .unwrap(),
        );
        let sources = cache
            .find_sources_by_host("camera-a.local", Duration::ZERO)
            .unwrap();
        assert_eq!(sources, vec![expected]);
        assert_eq!(
            cache
                .find_sources_by_host("CAMERA-A.LOCAL.", Duration::ZERO)
                .unwrap(),
            sources
        );
        assert_eq!(cache.len(), 1);
        assert_eq!(
            read_fake_log()
                .lines()
                .filter(|line| *line == "finder-snapshot")
                .count(),
            2,
            "the repeated outer-host lookup should hit the cache"
        );
        return;
    }

    let directory = common::FixtureDir::create("outer-endpoint");
    let library = directory.path().join("finder.so");
    FakeRuntime {
        cfgs: vec!["fake_finder"],
        version: Some("6.4.0.2"),
        ..Default::default()
    }
    .build(&library);
    TestChild::new("cache_matches_only_the_outer_endpoint_host")
        .env("GRAFTON_OUTER_ENDPOINT_RUNTIME", &library)
        .env("GRAFTON_FAKE_FINDER_CASE", "outer-endpoint")
        .env("GRAFTON_FAKE_NDI_LOG", directory.path().join("calls.log"))
        .run();
}
