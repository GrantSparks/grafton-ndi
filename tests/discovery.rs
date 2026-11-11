//! Discovery Server objects and routing against a test-owned runtime: version
//! gates, malformed text, identifier round trips and transfer between threads.
//!
//! Every scenario runs in its own process, because a process selects its
//! runtime once.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

#[path = "support/fixture.rs"]
mod fixture;

use fixture::FakeRuntime;
use grafton_ndi::{
    capabilities::{CapabilityStatus, CapabilityUnavailable, RuntimeFeature, SdkEdition},
    discovery::{
        AdvertiserOptions, ListenerOptions, ReceiverCommand, ReceiverId, ReceiverStream,
        RecvAdvertisement, RecvAdvertiser, RecvListener, SendAdvertisement, SendAdvertiser,
        SendListener, Wait,
    },
    routing::{Router, RouterOptions},
    runtime::RuntimeOptions,
    Error, Receiver, ReceiverOptions, Sender, SenderOptions, Source, SourceAddress, NDI,
};
use std::{fs, path::PathBuf, process::Command, time::Duration};

/// The capabilities whose objects talk to a Discovery Server, with the names
/// their errors carry.
const DISCOVERY_SERVER: [(RuntimeFeature, &str); 4] = [
    (RuntimeFeature::SenderAdvertising, "sender advertising"),
    (RuntimeFeature::ReceiverAdvertising, "receiver advertising"),
    (RuntimeFeature::SenderDiscovery, "sender discovery"),
    (RuntimeFeature::ReceiverDiscovery, "receiver discovery"),
];

/// The Discovery Server every object here names. The SDK creates advertisers
/// and listeners only for an address, and does not need to reach it.
const SERVER: &str = "192.0.2.1";

// What the fake runtime's fixed membership advertises; see `fake_runtime.rs`.
const SENDER_ID: &[u8] = b"sender-\xff\xfe";
const RECEIVER_ID: &[u8] = b"receiver-\xfe\xff";
const INPUT_ID: &[u8] = b"input-\xfd";

/// One build of the fake runtime and the scenarios that load it.
struct Variant {
    file: &'static str,
    version: &'static str,
    /// Whether the routing, advertiser and listener functions are exported.
    exports_functions: bool,
    scenarios: &'static [&'static str],
}

const VARIANTS: &[Variant] = &[
    Variant {
        file: "libndi_older.so",
        version: "6.3.2.0",
        exports_functions: true,
        scenarios: &["older_runtime_is_refused"],
    },
    Variant {
        file: "libndi_unversioned.so",
        version: "unreleased",
        exports_functions: true,
        scenarios: &["unrecognized_version_is_refused"],
    },
    Variant {
        file: "libndi_bare.so",
        version: "6.4.0.2",
        exports_functions: false,
        scenarios: &["missing_functions_are_named"],
    },
    Variant {
        file: "libndi_fixed.so",
        version: "6.4.0.0",
        exports_functions: true,
        scenarios: &[
            "fixed_runtime_is_accepted",
            "creation_needs_an_address",
            "advertisement_options_reach_the_sdk",
            "malformed_text_is_decoded_lossily",
            "identifiers_round_trip_exactly",
            "event_batches",
            "large_event_batches",
            "owners_move_between_threads",
            "routing",
        ],
    },
];

#[test]
fn discovery_server_objects() {
    if let Ok(scenario) = std::env::var("GRAFTON_DISCOVERY_CHILD") {
        child(&scenario);
        return;
    }
    let directory =
        std::env::temp_dir().join(format!("grafton-ndi-discovery-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    for variant in VARIANTS {
        let mut runtime = FakeRuntime::new().version(variant.version);
        if variant.exports_functions {
            runtime = runtime.with("fake_objects").with("fake_discovery");
        }
        let library = runtime.build(&directory.join(variant.file));
        for scenario in variant.scenarios {
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "discovery_server_objects", "--nocapture"])
                .env("GRAFTON_DISCOVERY_CHILD", scenario)
                .env("GRAFTON_DISCOVERY_LIBRARY", &library)
                .env(
                    "GRAFTON_FAKE_NDI_LOG",
                    directory.join(format!("{scenario}.log")),
                )
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{scenario}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    fs::remove_dir_all(directory).unwrap();
}

/// Everything the fake runtime has logged so far.
fn log() -> String {
    fs::read_to_string(std::env::var_os("GRAFTON_FAKE_NDI_LOG").unwrap()).unwrap_or_default()
}

/// Bytes as the fake runtime logs the identifiers it is given.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The error of each Discovery Server constructor, in `DISCOVERY_SERVER` order.
fn constructor_errors(
    ndi: &NDI,
    advertiser: &AdvertiserOptions,
    listener: &ListenerOptions,
) -> [Option<Error>; 4] {
    [
        SendAdvertiser::new(ndi, advertiser).err(),
        RecvAdvertiser::new(ndi, advertiser).err(),
        SendListener::new(ndi, listener).err(),
        RecvListener::new(ndi, listener).err(),
    ]
}

/// The options of an advertiser and a listener for [`SERVER`].
fn server() -> (AdvertiserOptions, ListenerOptions) {
    (
        AdvertiserOptions::default().servers(SERVER),
        ListenerOptions::default().server(SERVER).unwrap(),
    )
}

/// Every Discovery Server capability is unavailable for `reason`, and every
/// constructor says so, under the capability's name, without calling the SDK.
fn assert_refused(ndi: &NDI, reason: CapabilityUnavailable, explanation: &str) {
    let (advertisers, listeners) = server();
    let errors = constructor_errors(ndi, &advertisers, &listeners);
    for ((feature, name), error) in DISCOVERY_SERVER.into_iter().zip(errors) {
        assert_eq!(
            ndi.capabilities().status(feature),
            CapabilityStatus::Unavailable(reason),
            "{feature:?}"
        );
        match error {
            Some(Error::UnsupportedCapability { capability, reason }) => {
                assert_eq!(capability, name);
                assert!(reason.contains(explanation), "{name}: {reason}");
            }
            Some(other) => panic!("{name}: unexpected error {other}"),
            None => panic!("{name}: the constructor succeeded"),
        }
    }
    assert!(!log().contains("-create\n"), "{}", log());
}

fn child(scenario: &str) {
    let library = PathBuf::from(std::env::var_os("GRAFTON_DISCOVERY_LIBRARY").unwrap());
    let ndi = NDI::with_options(
        &RuntimeOptions::default()
            .library_path(library)
            .edition(SdkEdition::Standard),
    )
    .unwrap();
    let (advertisers, listeners) = server();
    match scenario {
        "older_runtime_is_refused" => {
            assert_refused(
                &ndi,
                CapabilityUnavailable::RuntimeTooOld,
                "it needs NDI 6.4.0.0 or later and the runtime reports \"NDI SDK TEST 6.3.2.0\"",
            );
            // Routing has no destroy-time defect and stays usable.
            drop(Router::new(&ndi, &RouterOptions::new("older")).unwrap());
        }
        "unrecognized_version_is_refused" => {
            assert_refused(
                &ndi,
                CapabilityUnavailable::RuntimeVersionUnknown,
                "it needs NDI 6.4.0.0 or later and the runtime reports \"NDI SDK TEST unreleased\"",
            );
            drop(Router::new(&ndi, &RouterOptions::new("unversioned")).unwrap());
        }
        "missing_functions_are_named" => {
            // A missing function is reported even though the version is new
            // enough, and the error names the first one.
            let first_missing = [
                "NDIlib_send_advertiser_create",
                "NDIlib_recv_advertiser_create",
                "NDIlib_send_listener_create",
                "NDIlib_recv_listener_create",
            ];
            for (((feature, name), error), symbol) in DISCOVERY_SERVER
                .into_iter()
                .zip(constructor_errors(&ndi, &advertisers, &listeners))
                .zip(first_missing)
            {
                assert_eq!(
                    ndi.capabilities().status(feature),
                    CapabilityStatus::Unavailable(CapabilityUnavailable::SymbolUnavailable)
                );
                let Some(Error::UnsupportedCapability { capability, reason }) = error else {
                    panic!("{name}: expected an unsupported capability");
                };
                assert_eq!(capability, name);
                assert!(reason.ends_with(symbol), "{name}: {reason}");
            }
            let Err(Error::UnsupportedCapability { capability, reason }) =
                Router::new(&ndi, &RouterOptions::new("bare"))
            else {
                panic!("routing needs its functions");
            };
            assert_eq!(capability, "source routing");
            assert!(reason.ends_with("NDIlib_routing_create"), "{reason}");
        }
        "fixed_runtime_is_accepted" => {
            for (feature, _) in DISCOVERY_SERVER {
                assert!(ndi.capabilities().supports(feature), "{feature:?}");
            }
            let send_advertiser = SendAdvertiser::new(&ndi, &advertisers).unwrap();
            let recv_advertiser = RecvAdvertiser::new(&ndi, &advertisers).unwrap();
            let send_listener = SendListener::new(&ndi, &listeners).unwrap();
            let recv_listener = RecvListener::new(&ndi, &listeners).unwrap();
            assert!(send_listener.is_connected() && recv_listener.is_connected());
            assert_eq!(
                send_listener.server_url().as_deref(),
                Some("192.0.2.1:5959")
            );
            assert_eq!(
                recv_listener.server_url().as_deref(),
                Some("192.0.2.1:5959")
            );
            drop((
                send_advertiser,
                recv_advertiser,
                send_listener,
                recv_listener,
            ));
            let events = log();
            for object in [
                "send-advertiser",
                "recv-advertiser",
                "send-listener",
                "recv-listener",
            ] {
                assert_eq!(events.matches(&format!("{object}-create\n")).count(), 1);
                assert_eq!(events.matches(&format!("{object}-destroy\n")).count(), 1);
            }
        }
        "creation_needs_an_address" => {
            // With no server named and none configured the SDK creates
            // nothing, and the error says that rather than blaming the network.
            let errors = constructor_errors(
                &ndi,
                &AdvertiserOptions::default(),
                &ListenerOptions::default(),
            );
            for ((_, name), error) in DISCOVERY_SERVER.into_iter().zip(errors) {
                let Some(Error::InitializationFailed(reason)) = error else {
                    panic!("{name}: expected a creation failure, got {error:?}");
                };
                assert!(
                    reason.contains("no Discovery Server was named"),
                    "{name}: {reason}"
                );
                assert!(!reason.contains("reachable"), "{name}: {reason}");
            }
            assert!(!log().contains("-create\n"), "{}", log());
        }
        "advertisement_options_reach_the_sdk" => {
            let receiver = |name: &str| {
                let options = ReceiverOptions::builder(Source::default()).name(name);
                Receiver::new(&ndi, &options.build()).unwrap()
            };
            let (controlled, monitored) = (receiver("controlled"), receiver("monitored"));
            let mut advertiser = RecvAdvertiser::new(&ndi, &advertisers).unwrap();
            let in_group = advertiser
                .register(
                    &controlled,
                    RecvAdvertisement::default()
                        .allow_controlling(true)
                        .input_group("Program feed"),
                )
                .unwrap();
            advertiser
                .register(
                    &monitored,
                    RecvAdvertisement::default().allow_monitoring(true),
                )
                .unwrap();
            let events = log();
            // Each flag arrives as the one it was set as, for the receiver it
            // was set for.
            let registered: Vec<_> = events
                .lines()
                .filter(|event| event.starts_with("recv-advertiser-add "))
                .collect();
            assert_eq!(
                registered,
                [
                    "recv-advertiser-add controlling=true monitoring=false",
                    "recv-advertiser-add controlling=false monitoring=true",
                ]
            );
            // The group name outlives the call that registered it: this
            // runtime reads it only when the registration ends.
            assert!(advertiser.unregister(in_group).unwrap());
            drop(advertiser);
            let events = log();
            assert!(
                events.contains(&format!(
                    "recv-advertiser-remove group={}\n",
                    hex(b"Program feed")
                )),
                "{events}"
            );
            assert!(events.contains("recv-advertiser-remove group=null\n"));

            let sender = |name: &str| Sender::new(&ndi, &SenderOptions::builder(name).build());
            let (plain, monitored) = (sender("plain").unwrap(), sender("monitored").unwrap());
            let mut advertiser = SendAdvertiser::new(&ndi, &advertisers).unwrap();
            advertiser
                .register(&plain, SendAdvertisement::default())
                .unwrap();
            advertiser
                .register(
                    &monitored,
                    SendAdvertisement::default().allow_monitoring(true),
                )
                .unwrap();
            let events = log();
            assert_eq!(
                events
                    .matches("send-advertiser-add monitoring=false\n")
                    .count(),
                1
            );
            assert_eq!(
                events
                    .matches("send-advertiser-add monitoring=true\n")
                    .count(),
                1
            );
        }
        "malformed_text_is_decoded_lossily" => {
            let mut listener = SendListener::new(&ndi, &listeners).unwrap();
            let senders = listener.snapshot().unwrap();
            let [malformed, plain] = senders.as_slice() else {
                panic!("expected two senders, got {senders:?}");
            };
            let id = malformed.id().unwrap();
            assert_eq!(id.as_bytes(), SENDER_ID);
            assert_eq!(id.to_string(), "sender-\u{fffd}\u{fffd}");
            assert_eq!(malformed.name(), Some("STUDIO (Caf\u{fffd})"));
            assert_eq!(malformed.metadata(), Some("<ndi_product/>"));
            assert_eq!(malformed.address(), Some("192.0.2.10"));
            assert_eq!(malformed.port(), Some(5961));
            assert_eq!(malformed.groups(), ["public", "gro\u{fffd}up"]);
            assert!(!malformed.events_subscribed());
            assert_eq!(plain.id().unwrap().as_bytes(), b"sender-plain");
            assert_eq!(plain.name(), Some("STUDIO (Camera 2)"));
            assert_eq!((plain.metadata(), plain.address()), (None, None));
            assert_eq!(plain.port(), None);
            assert!(plain.groups().is_empty());
            assert!(plain.events_subscribed());

            let mut listener = RecvListener::new(&ndi, &listeners).unwrap();
            let receivers = listener.snapshot().unwrap();
            let [malformed, fixed] = receivers.as_slice() else {
                panic!("expected two receivers, got {receivers:?}");
            };
            assert_eq!(malformed.id().unwrap().as_bytes(), RECEIVER_ID);
            assert_eq!(malformed.name(), Some("Monitor \u{fffd}"));
            assert_eq!(malformed.input_id().unwrap().as_bytes(), INPUT_ID);
            assert_eq!(malformed.input_name(), Some("Program"));
            assert_eq!(malformed.address(), Some("192.0.2.20"));
            assert_eq!(
                malformed.streams(),
                [ReceiverStream::Video, ReceiverStream::Audio]
            );
            assert_eq!(malformed.commands(), [ReceiverCommand::Connect]);
            assert_eq!(fixed.name(), Some("Fixed"));
            assert!(fixed.input_id().is_none() && fixed.input_name().is_none());
            assert!(fixed.streams().is_empty() && fixed.commands().is_empty());
        }
        "identifiers_round_trip_exactly" => {
            let mut senders = SendListener::new(&ndi, &listeners).unwrap();
            let sender = senders.snapshot().unwrap().remove(0);
            let id = sender.id().unwrap();
            senders.subscribe(id);
            senders.unsubscribe(id);
            let expected = hex(SENDER_ID);
            let events = log();
            assert!(events.contains(&format!("send-listener-subscribe {expected}\n")));
            assert!(events.contains(&format!("send-listener-unsubscribe {expected}\n")));

            let mut receivers = RecvListener::new(&ndi, &listeners).unwrap();
            let snapshot = receivers.snapshot().unwrap();
            let controllable = snapshot[0].id().unwrap();
            receivers.subscribe(controllable);
            receivers
                .request_connect(controllable, "STUDIO (Camera 2)")
                .unwrap();
            receivers.request_disconnect(controllable).unwrap();
            let expected = hex(RECEIVER_ID);
            let events = log();
            assert!(events.contains(&format!("recv-listener-subscribe {expected}\n")));
            assert!(events.contains(&format!(
                "recv-listener-connect {expected} {}\n",
                hex(b"STUDIO (Camera 2)")
            )));
            assert!(events.contains(&format!("recv-listener-connect {expected} null\n")));

            // The decoded text is for display: it names no advertised peer.
            // Neither does a receiver that is absent, and one that does not
            // accept the command is refused. None of these reaches the SDK.
            let displayed = ReceiverId::new(controllable.to_string()).unwrap();
            let absent = ReceiverId::new("absent").unwrap();
            let fixed = snapshot[1].id().unwrap();
            for id in [&displayed, &absent, fixed] {
                assert!(matches!(
                    receivers.request_connect(id, "STUDIO (Camera 2)"),
                    Err(Error::UnsupportedCapability { .. })
                ));
            }
            assert_eq!(log().matches("recv-listener-connect ").count(), 2);
        }
        "event_batches" => {
            let mut senders = SendListener::new(&ndi, &listeners).unwrap();
            let events = senders
                .capture_events(Wait::For(Duration::from_millis(5)))
                .unwrap();
            let [first, second] = events.as_slice() else {
                panic!("expected two events, got {events:?}");
            };
            assert_eq!(first.id().unwrap().as_bytes(), SENDER_ID);
            assert_eq!(first.name(), Some("tally"));
            assert_eq!(first.value(), Some("<tally on=\"\u{fffd}\"/>"));
            assert!(second.id().is_none() && second.name().is_none() && second.value().is_none());
            // A poll that finds nothing is an empty batch with nothing to free.
            assert!(senders
                .capture_events(Wait::For(Duration::ZERO))
                .unwrap()
                .is_empty());
            assert_eq!(
                log().matches("send-listener-free-events\n").count(),
                1,
                "{}",
                log()
            );

            let mut receivers = RecvListener::new(&ndi, &listeners).unwrap();
            let events = receivers.capture_events(Wait::Forever).unwrap();
            assert_eq!(events[0].id().unwrap().as_bytes(), RECEIVER_ID);
            assert!(log().contains("recv-listener-events 4294967295\n"));
            assert_eq!(log().matches("recv-listener-free-events\n").count(), 1);
        }
        "large_event_batches" => {
            let mut listener = SendListener::new(&ndi, &listeners).unwrap();
            let wait = Wait::For(Duration::from_millis(5));
            // The number of events is not itself a limit: only the bytes they
            // take to copy are.
            std::env::set_var("GRAFTON_FAKE_EVENT_COUNT", "70000");
            let events = listener.capture_events(wait).unwrap();
            assert_eq!(events.len(), 70_000);
            assert_eq!(events[69_998].name(), Some("tally"));
            assert!(events[69_999].name().is_none());

            // A batch whose descriptors alone exceed 16 MiB is refused for its
            // size, and released: the listener keeps working afterwards.
            std::env::set_var("GRAFTON_FAKE_EVENT_COUNT", "700000");
            let Err(Error::InvalidFrame(reason)) = listener.capture_events(wait) else {
                panic!("a batch larger than the copy limit must be refused");
            };
            assert!(reason.contains("16 MiB"), "{reason}");
            assert_eq!(log().matches("send-listener-free-events\n").count(), 2);
            std::env::remove_var("GRAFTON_FAKE_EVENT_COUNT");
            assert_eq!(listener.capture_events(wait).unwrap().len(), 2);
        }
        "owners_move_between_threads" => {
            let sender = Sender::new(&ndi, &SenderOptions::builder("moved").build()).unwrap();
            let receiver =
                Receiver::new(&ndi, &ReceiverOptions::builder(Source::default()).build()).unwrap();
            let mut router = Router::new(&ndi, &RouterOptions::new("moved router")).unwrap();
            let mut send_advertiser = SendAdvertiser::new(&ndi, &advertisers).unwrap();
            let mut recv_advertiser = RecvAdvertiser::new(&ndi, &advertisers).unwrap();
            let mut send_listener = SendListener::new(&ndi, &listeners).unwrap();
            let mut recv_listener = RecvListener::new(&ndi, &listeners).unwrap();
            let sent = send_advertiser
                .register(&sender, SendAdvertisement::default())
                .unwrap();
            let received = recv_advertiser
                .register(&receiver, RecvAdvertisement::default())
                .unwrap();
            // Each owner is used, and then destroyed, on a thread that did
            // not create it.
            std::thread::spawn(move || {
                router.clear().unwrap();
                assert_eq!(router.source().unwrap().name, "moved router");
                assert!(send_advertiser.unregister(sent).unwrap());
                assert!(recv_advertiser.unregister(received).unwrap());
                assert_eq!(send_listener.snapshot().unwrap().len(), 2);
                assert_eq!(recv_listener.snapshot().unwrap().len(), 2);
            })
            .join()
            .unwrap();
            let events = log();
            for destroyed in [
                "router-destroy",
                "send-advertiser-destroy",
                "recv-advertiser-destroy",
                "send-listener-destroy",
                "recv-listener-destroy",
            ] {
                assert_eq!(events.matches(&format!("{destroyed}\n")).count(), 1);
            }
            // The handles this thread kept are still the senders' and
            // receivers' owners.
            assert!(!events.contains("sender-destroy\n"));
            assert!(!events.contains("receiver-destroy\n"));
        }
        "routing" => {
            let mut router =
                Router::new(&ndi, &RouterOptions::new("Program").groups("public")).unwrap();
            assert_eq!(router.source().unwrap().name, "Program");
            assert_eq!(router.connection_count(Duration::ZERO).unwrap(), 0);
            router
                .change(&Source {
                    name: "STUDIO (Camera 2)".into(),
                    address: SourceAddress::None,
                })
                .unwrap();
            router.clear().unwrap();
            assert!(router.connection_count(Duration::MAX).is_err());
            drop(router);
            assert_eq!(
                log(),
                "initialize\nrouter-create\nrouter-change STUDIO (Camera 2)\nrouter-clear\nrouter-destroy\n"
            );
        }
        _ => panic!("unknown discovery scenario {scenario}"),
    }
    drop(ndi);
    assert!(!NDI::is_running());
}
