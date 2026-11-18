//! Finder bounds exercised through the public APIs and a valid mock SDK.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]
#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(child, fixture);
use common::TestChild;

#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;
use grafton_ndi::{Error, Finder, FinderOptions, Ndi, ResourceKind, RuntimeOptions};
use runtime_fixture::FakeRuntime;
use std::{path::PathBuf, time::Duration};

#[test]
fn finder_enforces_snapshot_and_whole_window_limits() {
    if let Ok(scenario) = std::env::var("GRAFTON_FAKE_FINDER_CASE") {
        let path = PathBuf::from(std::env::var_os("GRAFTON_FAKE_FINDER_RUNTIME").unwrap());
        let ndi = Ndi::with_options(&RuntimeOptions::new().library_path(path)).unwrap();
        let cache =
            grafton_ndi::SourceCache::new(Finder::new(&ndi, &FinderOptions::new()).unwrap());
        let finder = cache.finder();
        let limited = |result: grafton_ndi::Result<Vec<grafton_ndi::Source>>| {
            let (expected, maximum) = if scenario == "count" {
                (ResourceKind::FinderSnapshotSources, 16_384)
            } else {
                (ResourceKind::FinderSnapshotBytes, 16 * 1024 * 1024)
            };
            assert!(
                matches!(result, Err(Error::ResourceLimit { resource, limit }) if resource == expected && limit == maximum),
                "{result:?}"
            );
        };
        if scenario.starts_with("retained") {
            assert_eq!(finder.current_sources().unwrap().len(), 1);
            let resource = if scenario == "retained-count" {
                ResourceKind::FinderDiscoverySources
            } else {
                ResourceKind::FinderDiscoveryBytes
            };
            assert!(
                matches!(finder.find_sources(Duration::from_secs(10)), Err(Error::ResourceLimit { resource: actual, .. }) if actual == resource)
            );
        } else if scenario == "many-short" {
            assert_eq!(finder.current_sources().unwrap().len(), 16_384);
            assert_eq!(finder.find_sources(Duration::ZERO).unwrap().len(), 1);
            assert_eq!(
                finder
                    .find_source_by_name("xxxxxxxx", Duration::ZERO)
                    .unwrap()
                    .unwrap()
                    .name(),
                "xxxxxxxx"
            );
        } else {
            limited(finder.current_sources());
            limited(finder.find_sources(Duration::ZERO));
            if scenario == "bytes" {
                let name = "x".repeat(8_192);
                assert!(matches!(
                    finder.find_source_by_name(&name, Duration::ZERO),
                    Err(Error::ResourceLimit {
                        resource: ResourceKind::FinderSnapshotBytes,
                        ..
                    })
                ));
            }
            assert!(matches!(
                finder.find_source(Duration::ZERO, |_| true),
                Err(Error::ResourceLimit { .. })
            ));
            limited(cache.find_sources_by_host("studio", Duration::ZERO));
            assert!(cache.is_empty());
        }
        #[cfg(feature = "advanced")]
        if !scenario.starts_with("retained")
            && ndi
                .capabilities()
                .supports(grafton_ndi::capabilities::RuntimeFeature::SourceMetadata)
        {
            let result = finder.current_sources_with_metadata();
            if scenario == "many-short" {
                assert_eq!(result.unwrap().len(), 16_384);
            } else {
                assert!(
                    matches!(result, Err(Error::ResourceLimit { .. })),
                    "{result:?}"
                );
            }
        }
        return;
    }
    let directory = common::FixtureDir::create("finder-limits");
    let library = directory.path().join("finder.so");
    FakeRuntime {
        cfgs: vec!["fake_advanced", "fake_finder"],
        version: Some("6.4.0.2"),
        ..Default::default()
    }
    .build(&library);
    for scenario in [
        "count",
        "many-short",
        "bytes",
        "lossy",
        "unterminated",
        "metadata",
        "retained-count",
        "retained-bytes",
    ] {
        TestChild::new("finder_enforces_snapshot_and_whole_window_limits")
            .env("GRAFTON_FAKE_FINDER_CASE", scenario)
            .env("GRAFTON_FAKE_FINDER_RUNTIME", &library)
            .run();
    }
}
