//! Plain and configured Discovery Server objects share one lifecycle.
#![cfg(all(target_os = "linux", feature = "advanced", not(ndi_link_static)))]

#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(policy, prerequisites);
#[path = "support/fixture.rs"]
mod fixture;
#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;

use grafton_ndi::{
    advanced::AdvancedConfig,
    capabilities::RuntimeFeature,
    discovery::{
        AdvertiserOptions, ListenerOptions, RecvAdvertiser, RecvListener, SendAdvertiser,
        SendListener,
    },
    Ndi, RuntimeOptions,
};
use std::{fs, path::PathBuf, process::Command};

#[test]
fn every_server_object_shares_plain_and_configured_creation_and_destruction() {
    if let Some(library) = std::env::var_os("GRAFTON_LIFECYCLE_LIBRARY") {
        let ndi =
            Ndi::with_options(&RuntimeOptions::new().library_path(PathBuf::from(library))).unwrap();
        let log_path = PathBuf::from(std::env::var_os("GRAFTON_FAKE_NDI_LOG").unwrap());
        let configured = std::env::var_os("GRAFTON_LIFECYCLE_CONFIGURED").is_some();
        if configured {
            if let Err(error) =
                ndi.require_feature(RuntimeFeature::AdvancedSenderAdvertiserConfiguration)
            {
                common::skip(
                    common::Requirement::Advanced,
                    &format!("configured lifecycle needs Advanced declarations: {error}"),
                );
                return;
            }
        }
        let mut advertisers = AdvertiserOptions::new().servers("127.0.0.1");
        let mut listeners = ListenerOptions::new().server("127.0.0.1");
        if configured {
            let config = AdvancedConfig::parse("{}").unwrap();
            advertisers = advertisers.config(config.clone());
            listeners = listeners.config(config);
        }
        let owners = (
            SendAdvertiser::new(&ndi, &advertisers).unwrap(),
            RecvAdvertiser::new(&ndi, &advertisers).unwrap(),
            SendListener::new(&ndi, &listeners).unwrap(),
            RecvListener::new(&ndi, &listeners).unwrap(),
        );
        drop(advertisers);
        drop(listeners);
        drop(owners);
        let log = fs::read_to_string(log_path).unwrap();
        for object in [
            "send-advertiser",
            "recv-advertiser",
            "send-listener",
            "recv-listener",
        ] {
            assert_eq!(
                log.matches(&format!("{object}-create\n")).count(),
                1,
                "{log}"
            );
            assert_eq!(
                log.matches(&format!("{object}-destroy\n")).count(),
                1,
                "{log}"
            );
        }
        for function in [
            "NDIlib_send_advertiser_create_ex",
            "NDIlib_recv_advertiser_create_ex",
            "NDIlib_send_listener_create_ex",
            "NDIlib_recv_listener_create_ex",
        ] {
            assert_eq!(
                log.matches(function).count(),
                usize::from(configured),
                "{log}"
            );
        }
        return;
    }
    let directory = fixture::FixtureDir::create("discovery-lifecycle");
    let library = directory.path().join("libndi_advanced.so");
    runtime_fixture::FakeRuntime {
        cfgs: vec![
            "fake_objects",
            "fake_discovery",
            "fake_advanced",
            "fake_configured_creation",
            "fake_configured_success",
        ],
        version: Some("6.4.0.2"),
        ..Default::default()
    }
    .build(&library);
    for configured in [false, true] {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "every_server_object_shares_plain_and_configured_creation_and_destruction",
                "--nocapture",
            ])
            .env("GRAFTON_LIFECYCLE_LIBRARY", &library)
            .env(
                "GRAFTON_FAKE_NDI_LOG",
                directory.path().join(if configured {
                    "configured.log"
                } else {
                    "plain.log"
                }),
            );
        if configured {
            command.env("GRAFTON_LIFECYCLE_CONFIGURED", "1");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
