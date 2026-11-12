//! Qualification must reject a different runtime even when it loads cleanly.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

mod common;
#[path = "support/fixture.rs"]
mod fixture;

#[test]
fn provisioned_identity_is_checked_in_the_process_that_loads_the_runtime() {
    if std::env::var_os("GRAFTON_IDENTITY_CHILD").is_some() {
        assert!(common::ndi().is_some());
        return;
    }
    let directory = fixture::FixtureDir::create("identity");
    let edition = env!("GRAFTON_NDI_EDITION");
    let (name, wrong_edition) = if edition == "advanced" {
        ("libndi_advanced.so.6", "standard")
    } else {
        ("libndi.so.6", "advanced")
    };
    let runtime = directory.path().join(name);
    let fake = fixture::FakeRuntime::new().version("6.4.0.2");
    let fake = if edition == "advanced" {
        fake.with("fake_advanced")
    } else {
        fake
    };
    fake.build(&runtime);
    let other = directory.path().join("other.so");
    std::fs::copy(&runtime, &other).unwrap();
    for (version, edition, path, accepted) in [
        ("6.4.0.2", edition, &runtime, true),
        ("6.0.0.0", edition, &runtime, false),
        ("7.99.0.0", edition, &runtime, false),
        ("6.4.0.2", wrong_edition, &runtime, false),
        ("6.4.0.2", edition, &other, false),
    ] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "provisioned_identity_is_checked_in_the_process_that_loads_the_runtime",
            ])
            .env("GRAFTON_IDENTITY_CHILD", "1")
            .env("NDI_RUNTIME_DIR_V6", directory.path())
            .env("GRAFTON_NDI_REQUIRE_RUNTIME", "1")
            .env("GRAFTON_NDI_EXPECTED_VERSION", version)
            .env("GRAFTON_NDI_EXPECTED_EDITION", edition)
            .env("GRAFTON_NDI_EXPECTED_RUNTIME", path)
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            accepted,
            "version={version}, edition={edition}, path={path:?}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
