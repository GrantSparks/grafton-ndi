//! The fake SDK records the exact configuration pointer each constructor lends.
#![cfg(all(feature = "advanced", target_os = "linux", not(ndi_link_static)))]

#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(runtime, advanced, child, fixture);

#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;

use grafton_ndi::{
    advanced::{
        AdvancedConfig, CompressedSender, Genlock, GenlockOptions, PacketReceiver,
        PassthroughPreset,
    },
    discovery_server::{
        AdvertiserOptions, ListenerOptions, RecvAdvertiser, RecvListener, SendAdvertiser,
        SendListener,
    },
    routing::{Router, RouterOptions},
    CapabilityRefusal, CapabilityStatus, Finder, FinderOptions, Ndi, Receiver, ReceiverOptions,
    RuntimeFeature, RuntimeOptions, Sender, SenderOptions, Source,
};

#[test]
fn advanced_creation_lends_the_original_buffer_or_null() {
    if let Some(library) = std::env::var_os("GRAFTON_CONFIG_LENDING_LIBRARY") {
        exercise(&Ndi::with_options(&RuntimeOptions::new().library_path(library)).unwrap());
        return;
    }
    let directory = common::FixtureDir::create("config-lending");
    let library = runtime_fixture::FakeRuntime {
        cfgs: vec![
            "fake_objects",
            "fake_advanced",
            "fake_finder",
            "fake_discovery",
            "fake_capability_descriptors",
            "fake_configured_creation",
            "fake_configured_success",
            "fake_config_lending",
        ],
        version: Some("6.4.0.2"),
        ..Default::default()
    }
    .build(&directory.path().join("fake-ndi.so"));
    common::TestChild::new("advanced_creation_lends_the_original_buffer_or_null")
        .env("GRAFTON_CONFIG_LENDING_LIBRARY", library)
        .env("GRAFTON_FAKE_NDI_LOG", directory.path().join("native.log"))
        .env("GRAFTON_FAKE_FINDER_CASE", "config")
        .run();
}

fn exercise(ndi: &Ndi) {
    let source = Source::new("test reference").unwrap();
    let finder = FinderOptions::new();
    let sender = SenderOptions::new("test sender");
    let receiver = ReceiverOptions::new(source.clone());
    let router = RouterOptions::new("test router");
    let advertiser = AdvertiserOptions::new().servers(["127.0.0.1"]);
    let listener = ListenerOptions::new().server("127.0.0.1");
    let log = runtime_fixture::read_fake_log;

    drop(Finder::new(ndi, &finder).unwrap());
    drop(Sender::new(ndi, &sender).unwrap());
    drop(Receiver::new(ndi, &receiver).unwrap());
    drop(Router::new(ndi, &router).unwrap());
    drop(SendAdvertiser::new(ndi, &advertiser).unwrap());
    drop(RecvAdvertiser::new(ndi, &advertiser).unwrap());
    drop(SendListener::new(ndi, &listener).unwrap());
    drop(RecvListener::new(ndi, &listener).unwrap());
    assert!(!log().lines().any(|line| line.starts_with("config:")));

    let config = AdvancedConfig::parse(r#"{"marker":"retained"}"#).unwrap();
    let address = config.expose_json().as_ptr();
    let empty = std::ptr::null::<u8>();
    let mut expected = Vec::new();
    let compiled = |feature| match ndi.capabilities().status(feature) {
        CapabilityStatus::Available => true,
        CapabilityStatus::Unavailable(CapabilityRefusal::BuildAbiUnavailable(_)) => false,
        status => panic!("the fake runtime does not provide {feature:?}: {status:?}"),
    };
    macro_rules! check {
        ($feature:expr, $kind:literal, $pointer:expr, $create:expr) => {
            if compiled($feature) {
                drop($create.unwrap());
                expected.push(format!("config:{}:{:p}", $kind, $pointer));
            }
        };
    }
    check!(
        RuntimeFeature::AdvancedFinderConfiguration,
        "finder",
        address,
        Finder::new(ndi, &finder.config(config.clone()))
    );
    check!(
        RuntimeFeature::AdvancedSenderConfiguration,
        "sender",
        empty,
        CompressedSender::new(ndi, &sender)
    );
    check!(
        RuntimeFeature::AdvancedSenderConfiguration,
        "sender",
        address,
        Sender::new(ndi, &sender.config(config.clone()))
    );
    let preset = PassthroughPreset::SpeedHqOnly;
    check!(
        RuntimeFeature::Passthrough(preset),
        "receiver",
        empty,
        PacketReceiver::new(ndi, &receiver, preset)
    );
    check!(
        RuntimeFeature::AdvancedReceiverConfiguration,
        "receiver",
        address,
        Receiver::new(ndi, &receiver.config(config.clone()))
    );
    check!(
        RuntimeFeature::AdvancedRouterConfiguration,
        "NDIlib_routing_create_v2",
        address,
        Router::new(ndi, &router.config(config.clone()))
    );
    check!(
        RuntimeFeature::AdvancedSenderAdvertiserConfiguration,
        "NDIlib_send_advertiser_create_ex",
        address,
        SendAdvertiser::new(ndi, &advertiser.clone().config(config.clone()))
    );
    check!(
        RuntimeFeature::AdvancedReceiverAdvertiserConfiguration,
        "NDIlib_recv_advertiser_create_ex",
        address,
        RecvAdvertiser::new(ndi, &advertiser.config(config.clone()))
    );
    check!(
        RuntimeFeature::AdvancedSenderListenerConfiguration,
        "NDIlib_send_listener_create_ex",
        address,
        SendListener::new(ndi, &listener.clone().config(config.clone()))
    );
    check!(
        RuntimeFeature::AdvancedReceiverListenerConfiguration,
        "NDIlib_recv_listener_create_ex",
        address,
        RecvListener::new(ndi, &listener.config(config.clone()))
    );

    let options = GenlockOptions::new().source(source);
    check!(
        RuntimeFeature::Genlock,
        "genlock",
        empty,
        Genlock::new(ndi, &options)
    );
    if compiled(RuntimeFeature::Genlock) {
        let genlock = Genlock::new(ndi, &options.config(config)).unwrap();
        expected.push(format!("config:genlock:{address:p}"));
        drop(genlock);
        assert!(log().contains(r#"genlock-destroy:test reference:{"marker":"retained"}"#));
    }

    let events = log();
    let observed: Vec<_> = events
        .lines()
        .filter(|event| event.starts_with("config:"))
        .collect();
    assert_eq!(observed, expected, "{events}");
}

#[test]
fn compressed_sender_inherits_machine_name_without_a_configuration_override() {
    const MACHINE: &str = "grafton-compressed-default";
    const CHILD: &str = "GRAFTON_COMPRESSED_DEFAULT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let directory = common::FixtureDir::create("compressed-default");
        std::fs::write(
            directory.path().join("ndi-config.v1.json"),
            format!(r#"{{"ndi":{{"machinename":"{MACHINE}"}}}}"#),
        )
        .unwrap();
        common::TestChild::new(
            "compressed_sender_inherits_machine_name_without_a_configuration_override",
        )
        .env(CHILD, "1")
        .env("NDI_CONFIG_DIR", directory.path())
        .run();
        return;
    }
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let options = SenderOptions::new(format!("grafton compressed default {}", std::process::id()));
    let sender = CompressedSender::new(&ndi, &options).unwrap();
    let source = sender.sender().source().unwrap();
    let (machine, label) = source.name().split_once(" (").unwrap();
    assert!(machine.eq_ignore_ascii_case(MACHINE), "{}", source.name());
    assert_eq!(label.strip_suffix(')'), Some(options.configured_name()));
}
