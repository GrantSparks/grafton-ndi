//! Standard Windows receivers may be monitored but cannot grant remote control.
#![cfg(windows)]

#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(runtime, prerequisites, name, child, fixture);

#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;

use grafton_ndi::{
    discovery_server::{AdvertiserOptions, RecvAdvertisement, RecvAdvertiser},
    Capability, CapabilityRefusal, Error, Ndi, Receiver, ReceiverOptions, RuntimeFeature,
    RuntimeOptions, SdkEdition, Source,
};

fn exercise(ndi: &Ndi) {
    let receiver = Receiver::new(
        ndi,
        &ReceiverOptions::new(Source::new("").unwrap())
            .name(common::unique_name("standard windows receiver")),
    )
    .unwrap();
    // Registration does not need a reachable server.
    let options = AdvertiserOptions::new().servers(["127.0.0.1:1"]);
    let mut advertiser = RecvAdvertiser::new(ndi, &options).unwrap();
    let monitoring = RecvAdvertisement::new()
        .allow_monitoring(true)
        .input_group("Windows studio");
    let result = advertiser.register(&receiver, &monitoring.clone().allow_controlling(true));
    assert!(matches!(
        result,
        Err(Error::UnsupportedCapability {
            capability: Capability::Runtime(RuntimeFeature::RemoteReceiverControl),
            refusal: CapabilityRefusal::WrongEdition(None),
        })
    ));
    let registration = advertiser.register(&receiver, &monitoring).unwrap();
    advertiser.unregister(registration).unwrap();
}

#[test]
fn fake_standard_runtime_refuses_remote_control_before_native_add() {
    if let Some(library) = std::env::var_os("GRAFTON_FAKE_REMOTE_CONTROL_RUNTIME") {
        let ndi = Ndi::with_options(&RuntimeOptions::new().library_path(library)).unwrap();
        exercise(&ndi);
        let log = runtime_fixture::read_fake_log();
        assert_eq!(
            log.lines()
                .filter(|line| line.starts_with("recv-advertiser-add"))
                .count(),
            1,
            "{log}"
        );
        return;
    }

    let fixture = common::FixtureDir::create("fake-remote-receiver-control");
    let library = runtime_fixture::FakeRuntime {
        cfgs: vec!["fake_objects", "fake_discovery"],
        version: Some("6.4.0.2"),
        ..Default::default()
    }
    .build(&fixture.path().join("fake-ndi.dll"));
    common::TestChild::new("fake_standard_runtime_refuses_remote_control_before_native_add")
        .env("GRAFTON_FAKE_REMOTE_CONTROL_RUNTIME", library)
        .env("GRAFTON_FAKE_NDI_LOG", fixture.path().join("native.log"))
        .run();
}

#[test]
fn standard_runtime_refuses_remote_control_and_permits_monitoring() {
    let Some(ndi) = common::ndi() else { return };
    if ndi.info().edition() != SdkEdition::Standard {
        common::skip(
            common::Requirement::DiscoveryServer,
            "Standard Windows runtime required",
        );
        return;
    }
    if let Err(error) = ndi.require_feature(RuntimeFeature::ReceiverAdvertising) {
        common::skip(common::Requirement::DiscoveryServer, &error.to_string());
        return;
    }
    exercise(&ndi);
}
