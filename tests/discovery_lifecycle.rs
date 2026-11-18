//! Plain and configured Discovery Server objects share one lifecycle.
#![cfg(all(target_os = "linux", feature = "advanced", not(ndi_link_static)))]

#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(policy, prerequisites, child);
use common::TestChild;
#[path = "support/fixture.rs"]
mod fixture;
#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;

use grafton_ndi::{
    advanced::AdvancedConfig,
    capabilities::RuntimeFeature,
    discovery_server::{
        AdvertiserOptions, ListenerOptions, ReceiverId, RecvAdvertisement, RecvAdvertiser,
        RecvListener, SendAdvertisement, SendAdvertiser, SendListener, SenderId,
    },
    Error, Ndi, Receiver, ReceiverOptions, RuntimeOptions, Sender, SenderOptions, Source,
};
use std::{fs, path::PathBuf, process::Command};

#[test]
fn every_server_object_shares_plain_and_configured_creation_and_destruction() {
    if let Some(library) = std::env::var_os("GRAFTON_LIFECYCLE_LIBRARY") {
        let ndi =
            Ndi::with_options(&RuntimeOptions::new().library_path(PathBuf::from(library))).unwrap();
        let log_path = PathBuf::from(std::env::var_os("GRAFTON_FAKE_NDI_LOG").unwrap());
        let configured = std::env::var_os("GRAFTON_LIFECYCLE_CONFIGURED").is_some();
        let refused = std::env::var_os("GRAFTON_LIFECYCLE_REFUSED").is_some();
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
        const SERVER: &str = "127.0.0.1:5959";
        let mut advertisers = AdvertiserOptions::new().servers([SERVER]);
        let mut listeners = ListenerOptions::new().server(SERVER);
        if configured {
            let config = AdvancedConfig::parse("{}").unwrap();
            advertisers = advertisers.config(config.clone());
            listeners = listeners.config(config);
        }
        if refused {
            let errors = [
                ("advertiser", SendAdvertiser::new(&ndi, &advertisers).err()),
                ("advertiser", RecvAdvertiser::new(&ndi, &advertisers).err()),
                ("listener", SendListener::new(&ndi, &listeners).err()),
                ("listener", RecvListener::new(&ndi, &listeners).err()),
            ];
            for (object, error) in errors {
                let Some(Error::InvalidInput(reason)) = error else {
                    panic!("{object}: expected null-creation refusal, got {error:?}");
                };
                let grafton_ndi::InputDefect::DiscoveryAddressRejected(detail) = reason else {
                    panic!("{reason}")
                };
                assert_eq!(detail.object, object);
                assert_eq!(&*detail.address, SERVER);
            }
            let log = fs::read_to_string(log_path).unwrap_or_default();
            assert!(
                !log.contains("-destroy"),
                "null objects must never be destroyed: {log}"
            );
            return;
        }
        let mut owners = (
            SendAdvertiser::new(&ndi, &advertisers).unwrap(),
            RecvAdvertiser::new(&ndi, &advertisers).unwrap(),
            SendListener::new(&ndi, &listeners).unwrap(),
            RecvListener::new(&ndi, &listeners).unwrap(),
        );
        let removal_refused = std::env::var_os("GRAFTON_LIFECYCLE_REMOVE_REFUSED").is_some();
        std::env::set_var("GRAFTON_FAKE_RETAIN_SOURCES", "1");
        let sender = Sender::new(&ndi, &SenderOptions::new("lifecycle sender")).unwrap();
        let receiver = Receiver::new(
            &ndi,
            &ReceiverOptions::new(Source::new("lifecycle source").unwrap()),
        )
        .unwrap();
        owners
            .0
            .register(&sender, &SendAdvertisement::default())
            .unwrap();
        owners
            .1
            .register(
                &receiver,
                &RecvAdvertisement::default().input_group("retained group"),
            )
            .unwrap();
        let sender_id = SenderId::new("retained sender").unwrap();
        let receiver_id = ReceiverId::new("retained receiver").unwrap();
        owners.2.subscribe(&sender_id);
        owners.3.subscribe(&receiver_id);
        drop((sender_id, receiver_id, sender, receiver));
        drop(advertisers);
        drop(listeners);
        drop(ndi);
        let log = fs::read_to_string(&log_path).unwrap();
        assert!(
            !log.lines()
                .any(|line| matches!(line, "sender-destroy" | "receiver-destroy" | "destroy")),
            "{log}"
        );
        drop(owners);
        let log = fs::read_to_string(log_path).unwrap();
        let position = |event: &str| {
            log.lines()
                .position(|line| line == event)
                .unwrap_or_else(|| panic!("missing {event}: {log}"))
        };
        for (kind, member) in [("send", "sender"), ("recv", "receiver")] {
            let remove = if removal_refused {
                format!("{kind}-advertiser-remove-refused")
            } else if kind == "recv" {
                "recv-advertiser-remove group=72657461696e65642067726f7570".into()
            } else {
                "send-advertiser-remove".into()
            };
            let destroy = format!("{kind}-advertiser-destroy");
            assert!(position(&remove) < position(&destroy), "{log}");
            assert!(
                position(&destroy) < position(&format!("{member}-destroy")),
                "{log}"
            );
            assert_eq!(
                log.lines().filter(|line| *line == remove).count(),
                1,
                "{log}"
            );
            assert!(
                log.contains(&format!("{kind}-listener-retained-destroy 1\n")),
                "{log}"
            );
        }
        assert_eq!(log.lines().last(), Some("destroy"), "{log}");
        assert_eq!(
            log.contains("recv-advertiser-destroy-group 72657461696e65642067726f7570\n"),
            removal_refused,
            "{log}"
        );
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
    for refused in [false, true] {
        let variant = directory
            .path()
            .join(if refused { "refused" } else { "accepted" });
        fs::create_dir(&variant).unwrap();
        let library = variant.join("libndi_advanced.so");
        let mut cfgs = vec![
            "fake_objects",
            "fake_discovery",
            "fake_advanced",
            "fake_configured_creation",
            "fake_configured_success",
        ];
        if refused {
            cfgs.push("fake_refuse_server_creation");
        }
        runtime_fixture::FakeRuntime {
            cfgs,
            version: Some("6.4.0.2"),
            ..Default::default()
        }
        .build(&library);
        for configured in [false, true] {
            for removal_refused in [false, true] {
                if refused && removal_refused {
                    continue;
                }
                let mut command = TestChild::new(
                    "every_server_object_shares_plain_and_configured_creation_and_destruction",
                );
                command.env("GRAFTON_LIFECYCLE_LIBRARY", &library).env(
                    "GRAFTON_FAKE_NDI_LOG",
                    variant.join(format!(
                        "configured-{configured}-removal-refused-{removal_refused}.log"
                    )),
                );
                if configured {
                    command.env("GRAFTON_LIFECYCLE_CONFIGURED", "1");
                }
                if refused {
                    command.env("GRAFTON_LIFECYCLE_REFUSED", "1");
                }
                if removal_refused {
                    command
                        .env("GRAFTON_LIFECYCLE_REMOVE_REFUSED", "1")
                        .env("GRAFTON_FAKE_ADVERTISER_REMOVE", "refuse");
                }
                command.run();
            }
        }
    }
}
