//! Finder bounds exercised through the public APIs and a valid mock SDK.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]
#[path = "support/fixture.rs"]
mod fixture;
use fixture::{FakeRuntime, FixtureDir};
use grafton_ndi::{Error, Finder, FinderOptions, Ndi, ResourceKind, RuntimeOptions};
use std::{path::PathBuf, process::Command, time::Duration};

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
        } else {
            limited(finder.current_sources());
            limited(finder.find_sources(Duration::ZERO));
            assert!(matches!(
                finder.find_source(Duration::ZERO, |_| true),
                Err(Error::ResourceLimit { .. })
            ));
            limited(cache.sources_by_host("studio", Duration::ZERO));
            assert!(cache.is_empty());
        }
        #[cfg(feature = "advanced")]
        if !scenario.starts_with("retained")
            && ndi
                .capabilities()
                .supports(grafton_ndi::capabilities::RuntimeFeature::SourceMetadata)
        {
            let result = finder.sources_with_metadata();
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
    let directory = FixtureDir::create("finder-limits");
    let library = directory.path().join("finder.so");
    FakeRuntime::new()
        .version("6.4.0.2")
        .with("fake_advanced")
        .with("fake_finder")
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
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "finder_enforces_snapshot_and_whole_window_limits",
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
