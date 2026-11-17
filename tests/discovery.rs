//! Discovery Server objects and routing against a test-owned runtime: version
//! gates, malformed text, identifier round trips and transfer between threads.
//!
//! Every scenario runs in its own process, because a process selects its
//! runtime once.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;

use grafton_ndi::{
    capabilities::{CapabilityStatus, CapabilityUnavailable, RuntimeFeature, SdkEdition},
    discovery_server::{
        AdvertiserOptions, ListenerOptions, ReceiverCommand, ReceiverId, ReceiverStream,
        RecvAdvertisement, RecvAdvertiser, RecvListener, SendAdvertisement, SendAdvertiser,
        SendListener, SenderId,
    },
    routing::{Router, RouterOptions},
    runtime::RuntimeOptions,
    Error, Ndi, Receiver, ReceiverOptions, ResourceKind, Sender, SenderOptions, Source,
};
use runtime_fixture::FakeRuntime;
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
            "subscription_identifiers_are_retained",
            "event_batches",
            "large_event_batches",
            "null_event_batch_with_a_count_is_refused",
            "nested_snapshot_arrays_include_owned_storage",
            "malformed_membership_entries_are_omitted",
            "connection_requests_ignore_unrelated_snapshot_copy_limits",
            "malformed_events_are_omitted",
            "null_snapshot_arrays_are_refused",
            "owners_move_between_threads",
            "routing",
            "native_failure_names",
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
        let mut runtime = FakeRuntime {
            version: Some(variant.version),
            ..Default::default()
        };
        if variant.exports_functions {
            runtime.cfgs.extend(["fake_objects", "fake_discovery"]);
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
    ndi: &Ndi,
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
        AdvertiserOptions::default().servers([SERVER]),
        ListenerOptions::default().server(SERVER),
    )
}

/// Every Discovery Server capability is unavailable for `expected`, and every
/// constructor says so, reporting the capability with `evidence`, without
/// calling the SDK. `Ndi::require_feature` gives the same error.
fn assert_refused(ndi: &Ndi, expected: CapabilityUnavailable) {
    let (advertisers, listeners) = server();
    for ((feature, name), error) in
        DISCOVERY_SERVER
            .into_iter()
            .zip(constructor_errors(ndi, &advertisers, &listeners))
    {
        for error in [error, ndi.require_feature(feature).err()] {
            let Some(Error::UnsupportedCapability {
                capability: grafton_ndi::Capability::Runtime(refused),
                refusal,
            }) = error
            else {
                panic!("{name}: expected unsupported capability");
            };
            assert_eq!((refused, refusal.reason()), (feature, expected));
            assert_eq!(
                ndi.capabilities().status(feature),
                CapabilityStatus::Unavailable(refusal)
            );
            let (grafton_ndi::CapabilityRefusal::RuntimeTooOld { required }
            | grafton_ndi::CapabilityRefusal::RuntimeVersionUnknown { required }) = refusal
            else {
                panic!("{name}: expected required version evidence");
            };
            assert_eq!(
                (
                    required.major(),
                    required.minor(),
                    required.patch(),
                    required.build()
                ),
                (6, 4, 0, 0)
            );
        }
    }
    assert!(!log().contains("-create\n"), "{}", log());
}

fn child(scenario: &str) {
    let library = PathBuf::from(std::env::var_os("GRAFTON_DISCOVERY_LIBRARY").unwrap());
    let ndi = Ndi::with_options(
        &RuntimeOptions::default()
            .library_path(library)
            .edition(SdkEdition::Standard),
    )
    .unwrap();
    let (advertisers, listeners) = server();
    match scenario {
        "older_runtime_is_refused" => {
            assert_refused(&ndi, CapabilityUnavailable::RuntimeTooOld);
            // Routing has no destroy-time defect and stays usable.
            drop(Router::new(&ndi, &RouterOptions::new("older")).unwrap());
        }
        "unrecognized_version_is_refused" => {
            assert_refused(&ndi, CapabilityUnavailable::RuntimeVersionUnknown);
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
                    (ndi.capabilities().status(feature)).reason(),
                    Some(CapabilityUnavailable::SymbolUnavailable)
                );
                // The capability's own gate names the function too.
                for error in [error, ndi.require_feature(feature).err()] {
                    let Some(Error::UnsupportedCapability {
                        capability: grafton_ndi::Capability::Runtime(refused),
                        refusal,
                    }) = error
                    else {
                        panic!("{name}: expected an unsupported capability");
                    };
                    assert_eq!(refused, feature);
                    assert_eq!(refusal.reason(), CapabilityUnavailable::SymbolUnavailable);
                    assert!(
                        matches!(refusal, grafton_ndi::CapabilityRefusal::SymbolUnavailable(missing) if missing.to_str().unwrap() == symbol),
                        "{name}"
                    );
                }
            }
            let Err(Error::UnsupportedCapability {
                capability: grafton_ndi::Capability::Runtime(feature),
                refusal,
            }) = Router::new(&ndi, &RouterOptions::new("bare"))
            else {
                panic!("routing needs its functions");
            };
            assert_eq!(feature, RuntimeFeature::Routing);
            assert_eq!(refusal.reason(), CapabilityUnavailable::SymbolUnavailable);
            assert_eq!(
                refusal,
                grafton_ndi::CapabilityRefusal::SymbolUnavailable(c"NDIlib_routing_create")
            );
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
                let Some(Error::InvalidInput(reason)) = error else {
                    panic!("{name}: expected a creation failure, got {error:?}");
                };
                assert!(
                    matches!(
                        reason,
                        grafton_ndi::InputDefect::DiscoveryConfigurationRejected { .. }
                    ),
                    "{name}: {reason}"
                );
            }
            // A listener uses one server, so a list is refused before the SDK
            // is asked for anything.
            let several = ListenerOptions::default().server("192.0.2.1,192.0.2.2");
            for error in [
                SendListener::new(&ndi, &several).err(),
                RecvListener::new(&ndi, &several).err(),
            ] {
                let Some(Error::InvalidInput(reason)) = error else {
                    panic!("expected a refused server list, got {error:?}");
                };
                assert!(
                    matches!(reason, grafton_ndi::InputDefect::MultipleDiscoveryServers),
                    "{reason}"
                );
            }
            assert!(!log().contains("-create\n"), "{}", log());
        }
        "advertisement_options_reach_the_sdk" => {
            let receiver = |name: &str| {
                let options = ReceiverOptions::new(Source::new("").unwrap()).name(name);
                Receiver::new(&ndi, &options).unwrap()
            };
            let (controlled, monitored) = (receiver("controlled"), receiver("monitored"));
            let mut advertiser = RecvAdvertiser::new(&ndi, &advertisers).unwrap();
            let in_group = advertiser
                .register(
                    &controlled,
                    &RecvAdvertisement::default()
                        .allow_controlling(true)
                        .input_group("Program feed"),
                )
                .unwrap();
            advertiser
                .register(
                    &monitored,
                    &RecvAdvertisement::default().allow_monitoring(true),
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
            advertiser.unregister(in_group).unwrap();
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

            let sender = |name: &str| Sender::new(&ndi, &SenderOptions::new(name));
            let (plain, monitored) = (sender("plain").unwrap(), sender("monitored").unwrap());
            let mut advertiser = SendAdvertiser::new(&ndi, &advertisers).unwrap();
            advertiser
                .register(&plain, &SendAdvertisement::default())
                .unwrap();
            advertiser
                .register(
                    &monitored,
                    &SendAdvertisement::default().allow_monitoring(true),
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
            let senders = listener.current_senders().unwrap();
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
            let receivers = listener.current_receivers().unwrap();
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
        "subscription_identifiers_are_retained" => {
            std::env::set_var("GRAFTON_FAKE_RETAIN_SOURCES", "1");
            macro_rules! check_retention {
                ($Listener:ident, $Id:ident, $bytes:ident, $snapshot:ident, $prefix:literal) => {{
                    let mut listener = $Listener::new(&ndi, &listeners).unwrap();
                    let id = $Id::new($bytes).unwrap();
                    let caller_pointer = id.as_bytes().as_ptr() as usize;
                    listener.subscribe(&id);
                    listener.subscribe(&id);
                    assert!(
                        !log()
                            .contains(&format!("{}-retained-pointer {caller_pointer}\n", $prefix)),
                        "the listener must pass its own allocation"
                    );
                    drop(id);
                    listener.$snapshot().unwrap();
                    listener.next_events(Duration::from_millis(1)).unwrap();
                    listener.unsubscribe(&$Id::new($bytes).unwrap());
                    listener.$snapshot().unwrap();
                    assert!(log().contains(concat!($prefix, "-retained-snapshot 0\n")));
                    // Unknown unsubscriptions keep their borrowed-call contract.
                    listener.unsubscribe(&$Id::new("never-subscribed").unwrap());
                    let id = $Id::new($bytes).unwrap();
                    listener.subscribe(&id);
                    drop(id);
                    drop(listener);
                    let log = log();
                    for operation in ["snapshot", "events", "unsubscribe", "destroy"] {
                        assert!(
                            log.contains(&format!("{}-retained-{operation} 1\n", $prefix)),
                            "{log}"
                        );
                    }
                }};
            }
            check_retention!(
                SendListener,
                SenderId,
                SENDER_ID,
                current_senders,
                "send-listener"
            );
            check_retention!(
                RecvListener,
                ReceiverId,
                RECEIVER_ID,
                current_receivers,
                "recv-listener"
            );
        }
        "identifiers_round_trip_exactly" => {
            let mut senders = SendListener::new(&ndi, &listeners).unwrap();
            let sender = senders.current_senders().unwrap().remove(0);
            let id = sender.id().unwrap();
            senders.subscribe(id);
            senders.unsubscribe(id);
            let expected = hex(SENDER_ID);
            let events = log();
            assert!(events.contains(&format!("send-listener-subscribe {expected}\n")));
            assert!(events.contains(&format!("send-listener-unsubscribe {expected}\n")));

            let mut receivers = RecvListener::new(&ndi, &listeners).unwrap();
            let snapshot = receivers.current_receivers().unwrap();
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
            // accept the command is refused. None of these reaches the SDK,
            // and none is blamed on the runtime.
            let displayed = ReceiverId::new(controllable.to_string()).unwrap();
            let absent = ReceiverId::new("absent").unwrap();
            let fixed = snapshot[1].id().unwrap();
            for (id, listed) in [(&displayed, false), (&absent, false), (fixed, true)] {
                for refused in [
                    receivers.request_connect(id, "STUDIO (Camera 2)"),
                    receivers.request_disconnect(id),
                ] {
                    match refused {
                        Err(Error::ReceiverRequestNotAccepted { advertised }) => {
                            assert_eq!(advertised, listed, "{id}");
                        }
                        other => panic!("{id}: expected a refused request, got {other:?}"),
                    }
                }
            }
            assert_eq!(log().matches("recv-listener-connect ").count(), 2);
        }
        "event_batches" => {
            let mut senders = SendListener::new(&ndi, &listeners).unwrap();
            let events = senders.next_events(Duration::from_millis(5)).unwrap();
            let [first, second] = events.as_slice() else {
                panic!("expected two events, got {events:?}");
            };
            assert_eq!(first.id().unwrap().as_bytes(), SENDER_ID);
            assert_eq!(first.name(), Some("tally"));
            assert_eq!(first.value(), Some("<tally on=\"\u{fffd}\"/>"));
            assert!(second.id().is_none() && second.name().is_none() && second.value().is_none());
            // A poll that finds nothing is an empty batch with nothing to free.
            assert!(senders.next_events(Duration::ZERO).unwrap().is_empty());
            assert_eq!(
                log().matches("send-listener-free-events\n").count(),
                1,
                "{}",
                log()
            );

            let mut receivers = RecvListener::new(&ndi, &listeners).unwrap();
            // The longest wait never reaches the SDK's indefinite 0xFFFFFFFF.
            let events = receivers.next_events(grafton_ndi::MAX_TIMEOUT).unwrap();
            assert_eq!(events[0].id().unwrap().as_bytes(), RECEIVER_ID);
            assert!(log().contains("recv-listener-events 4294967294\n"));
            assert_eq!(log().matches("recv-listener-free-events\n").count(), 1);
        }
        "large_event_batches" => {
            let mut listener = SendListener::new(&ndi, &listeners).unwrap();
            let wait = Duration::from_millis(5);
            // The number of events is not itself a limit: only the bytes they
            // take to copy are.
            std::env::set_var("GRAFTON_FAKE_EVENT_COUNT", "70000");
            let events = listener.next_events(wait).unwrap();
            assert_eq!(events.len(), 70_000);
            assert_eq!(events[69_998].name(), Some("tally"));
            assert!(events[69_999].name().is_none());

            // Native descriptors fit, but owned event storage and strings
            // together exceed the same budget. Both listener kinds free a
            // rejected batch exactly once and remain usable.
            std::env::set_var("GRAFTON_FAKE_EVENT_COUNT", "250000");
            assert!(matches!(
                listener.next_events(wait),
                Err(Error::ResourceLimit {
                    resource: ResourceKind::DiscoveryServerCopyBytes,
                    limit: 16_777_216,
                })
            ));
            let mut receivers = RecvListener::new(&ndi, &listeners).unwrap();
            assert!(matches!(
                receivers.next_events(wait),
                Err(Error::ResourceLimit {
                    resource: ResourceKind::DiscoveryServerCopyBytes,
                    limit: 16_777_216,
                })
            ));
            assert_eq!(log().matches("recv-listener-free-events\n").count(), 1);

            // A batch whose descriptors alone exceed 16 MiB is refused for its
            // size, and released: the listener keeps working afterwards.
            std::env::set_var("GRAFTON_FAKE_EVENT_COUNT", "1500000");
            let Err(Error::ResourceLimit {
                resource: ResourceKind::DiscoveryServerCopyBytes,
                limit: 16_777_216,
            }) = listener.next_events(wait)
            else {
                panic!("a batch larger than the copy limit must be refused");
            };
            assert_eq!(log().matches("send-listener-free-events\n").count(), 3);
            std::env::remove_var("GRAFTON_FAKE_EVENT_COUNT");
            assert_eq!(listener.next_events(wait).unwrap().len(), 2);
            assert_eq!(receivers.next_events(wait).unwrap().len(), 2);
        }
        "null_event_batch_with_a_count_is_refused" => {
            let mut listener = SendListener::new(&ndi, &listeners).unwrap();
            std::env::set_var("GRAFTON_FAKE_NULL_EVENT_BATCH", "1");
            assert!(matches!(
                listener.next_events(Duration::from_millis(5)),
                Err(Error::UnexpectedSdkResult(_))
            ));
            assert_eq!(log().matches("send-listener-free-events\n").count(), 0);
            std::env::remove_var("GRAFTON_FAKE_NULL_EVENT_BATCH");
            assert_eq!(
                listener
                    .next_events(Duration::from_millis(5))
                    .unwrap()
                    .len(),
                2
            );
            assert_eq!(log().matches("send-listener-free-events\n").count(), 1);
        }
        "nested_snapshot_arrays_include_owned_storage" => {
            for array in ["groups", "streams", "commands"] {
                std::env::set_var("GRAFTON_FAKE_DISCOVERY_ARRAY", array);
                let result = if array == "groups" {
                    SendListener::new(&ndi, &listeners)
                        .unwrap()
                        .current_senders()
                        .map(drop)
                } else {
                    RecvListener::new(&ndi, &listeners)
                        .unwrap()
                        .current_receivers()
                        .map(drop)
                };
                let Err(Error::ResourceLimit {
                    resource: ResourceKind::DiscoveryServerCopyBytes,
                    limit: 16_777_216,
                }) = result
                else {
                    panic!("the owned {array} must spend the snapshot budget");
                };
            }
            std::env::remove_var("GRAFTON_FAKE_DISCOVERY_ARRAY");
            assert_eq!(
                SendListener::new(&ndi, &listeners)
                    .unwrap()
                    .current_senders()
                    .unwrap()
                    .len(),
                2
            );
            assert_eq!(
                RecvListener::new(&ndi, &listeners)
                    .unwrap()
                    .current_receivers()
                    .unwrap()
                    .len(),
                2
            );
        }
        "malformed_membership_entries_are_omitted" => {
            for malformed in [
                "sender-id",
                "sender-name",
                "sender-metadata",
                "sender-address",
                "sender-group",
                "groups-null",
            ] {
                std::env::set_var("GRAFTON_FAKE_DISCOVERY_MALFORMED", malformed);
                let mut listener = SendListener::new(&ndi, &listeners).unwrap();
                let snapshot = listener.current_senders().unwrap();
                assert_eq!(snapshot.len(), 1, "{malformed}: {snapshot:?}");
                assert_eq!(
                    snapshot[0].id().unwrap().as_bytes(),
                    SENDER_ID,
                    "{malformed}"
                );
                assert_eq!(snapshot[0].groups(), ["public", "gro\u{fffd}up"]);
                // A separately valid snapshot still includes both entries;
                // omission must not affect discovery beyond this copy.
                std::env::remove_var("GRAFTON_FAKE_DISCOVERY_MALFORMED");
                let restored = SendListener::new(&ndi, &listeners)
                    .unwrap()
                    .current_senders()
                    .unwrap();
                assert_eq!(restored.len(), 2, "{malformed}: {restored:?}");
                assert_eq!(restored[0].id().unwrap().as_bytes(), SENDER_ID);
            }
            let mut requested = 0;
            for malformed in [
                "receiver-id",
                "receiver-name",
                "receiver-input-id",
                "receiver-input-name",
                "receiver-address",
                "streams-null",
                "commands-null",
            ] {
                std::env::set_var("GRAFTON_FAKE_DISCOVERY_MALFORMED", malformed);
                let mut listener = RecvListener::new(&ndi, &listeners).unwrap();
                let snapshot = listener.current_receivers().unwrap();
                assert_eq!(snapshot.len(), 1, "{malformed}: {snapshot:?}");
                let survivor = snapshot[0].id().unwrap();
                assert_eq!(survivor.as_bytes(), RECEIVER_ID, "{malformed}");
                listener
                    .request_connect(survivor, "STUDIO (Camera 2)")
                    .unwrap();
                listener.request_disconnect(survivor).unwrap();
                requested += 2;
                assert_eq!(log().matches("recv-listener-connect ").count(), requested);
                let omitted = ReceiverId::new("receiver-fixed").unwrap();
                match malformed {
                    "receiver-id" | "commands-null" => {
                        for refused in [
                            listener.request_connect(&omitted, "STUDIO (Camera 2)"),
                            listener.request_disconnect(&omitted),
                        ] {
                            let Err(Error::ReceiverRequestNotAccepted { advertised }) = refused
                            else {
                                panic!("{malformed}: expected a refused request, got {refused:?}")
                            };
                            assert_eq!(advertised, malformed == "commands-null", "{malformed}");
                        }
                    }
                    _ => {
                        // This variant explicitly advertises Connect. Its bad
                        // unrelated field still omits it from snapshots, but
                        // cannot prevent a request to its exact identifier.
                        listener
                            .request_connect(&omitted, "STUDIO (Camera 2)")
                            .unwrap();
                        listener.request_disconnect(&omitted).unwrap();
                        requested += 2;
                        assert!(log().contains(&format!(
                            "recv-listener-connect {} null\n",
                            hex(b"receiver-fixed")
                        )));
                    }
                }
                assert_eq!(log().matches("recv-listener-connect ").count(), requested);
                std::env::remove_var("GRAFTON_FAKE_DISCOVERY_MALFORMED");
                let restored = RecvListener::new(&ndi, &listeners)
                    .unwrap()
                    .current_receivers()
                    .unwrap();
                assert_eq!(restored.len(), 2, "{malformed}: {restored:?}");
                assert_eq!(restored[0].id().unwrap().as_bytes(), RECEIVER_ID);
            }
            assert!(log().contains(&format!(
                "recv-listener-connect {} {}\n",
                hex(RECEIVER_ID),
                hex(b"STUDIO (Camera 2)")
            )));
            std::env::remove_var("GRAFTON_FAKE_DISCOVERY_MALFORMED");
        }
        "connection_requests_ignore_unrelated_snapshot_copy_limits" => {
            std::env::set_var("GRAFTON_FAKE_DISCOVERY_MALFORMED", "receiver-copy-budget");
            let mut listener = RecvListener::new(&ndi, &listeners).unwrap();
            assert!(matches!(
                listener.current_receivers(),
                Err(Error::ResourceLimit {
                    resource: ResourceKind::DiscoveryServerCopyBytes,
                    limit: 16_777_216,
                })
            ));
            let target = ReceiverId::new(RECEIVER_ID).unwrap();
            listener
                .request_connect(&target, "STUDIO (Camera 2)")
                .unwrap();
            listener.request_disconnect(&target).unwrap();
            assert_eq!(log().matches("recv-listener-connect ").count(), 2);
            assert!(log().contains(&format!(
                "recv-listener-connect {} {}\n",
                hex(RECEIVER_ID),
                hex(b"STUDIO (Camera 2)")
            )));
            assert!(log().contains(&format!(
                "recv-listener-connect {} null\n",
                hex(RECEIVER_ID)
            )));
            std::env::remove_var("GRAFTON_FAKE_DISCOVERY_MALFORMED");
        }
        "malformed_events_are_omitted" => {
            let mut senders = SendListener::new(&ndi, &listeners).unwrap();
            let mut receivers = RecvListener::new(&ndi, &listeners).unwrap();
            let wait = Duration::from_millis(5);
            for (index, malformed) in ["event-id", "event-name", "event-value"]
                .into_iter()
                .enumerate()
            {
                std::env::set_var("GRAFTON_FAKE_DISCOVERY_MALFORMED", malformed);
                let events = senders.next_events(wait).unwrap();
                assert_eq!(events.len(), 1, "{malformed}: {events:?}");
                assert!(
                    events[0].id().is_none()
                        && events[0].name().is_none()
                        && events[0].value().is_none()
                );
                let events = receivers.next_events(wait).unwrap();
                assert_eq!(events.len(), 1, "{malformed}: {events:?}");
                assert!(
                    events[0].id().is_none()
                        && events[0].name().is_none()
                        && events[0].value().is_none()
                );
                assert_eq!(
                    log().matches("send-listener-free-events\n").count(),
                    index + 1
                );
                assert_eq!(
                    log().matches("recv-listener-free-events\n").count(),
                    index + 1
                );
            }
            std::env::remove_var("GRAFTON_FAKE_DISCOVERY_MALFORMED");
            assert_eq!(senders.next_events(wait).unwrap().len(), 2);
            assert_eq!(receivers.next_events(wait).unwrap().len(), 2);
            assert_eq!(log().matches("send-listener-free-events\n").count(), 4);
            assert_eq!(log().matches("recv-listener-free-events\n").count(), 4);
        }
        "null_snapshot_arrays_are_refused" => {
            let mut senders = SendListener::new(&ndi, &listeners).unwrap();
            let mut receivers = RecvListener::new(&ndi, &listeners).unwrap();
            std::env::set_var("GRAFTON_FAKE_DISCOVERY_MALFORMED", "senders-null");
            assert!(matches!(
                senders.current_senders(),
                Err(Error::UnexpectedSdkResult(_))
            ));
            std::env::set_var("GRAFTON_FAKE_DISCOVERY_MALFORMED", "receivers-null");
            assert!(matches!(
                receivers.current_receivers(),
                Err(Error::UnexpectedSdkResult(_))
            ));
            let target = ReceiverId::new(RECEIVER_ID).unwrap();
            assert!(matches!(
                receivers.request_connect(&target, "STUDIO (Camera 2)"),
                Err(Error::UnexpectedSdkResult(_))
            ));
            assert!(matches!(
                receivers.request_disconnect(&target),
                Err(Error::UnexpectedSdkResult(_))
            ));
            assert_eq!(log().matches("recv-listener-connect ").count(), 0);
            std::env::remove_var("GRAFTON_FAKE_DISCOVERY_MALFORMED");
            assert_eq!(senders.current_senders().unwrap().len(), 2);
            assert_eq!(receivers.current_receivers().unwrap().len(), 2);
        }
        "owners_move_between_threads" => {
            let sender = Sender::new(&ndi, &SenderOptions::new("moved")).unwrap();
            let receiver =
                Receiver::new(&ndi, &ReceiverOptions::new(Source::new("").unwrap())).unwrap();
            let mut router = Router::new(&ndi, &RouterOptions::new("moved router")).unwrap();
            let mut send_advertiser = SendAdvertiser::new(&ndi, &advertisers).unwrap();
            let mut recv_advertiser = RecvAdvertiser::new(&ndi, &advertisers).unwrap();
            let mut send_listener = SendListener::new(&ndi, &listeners).unwrap();
            let mut recv_listener = RecvListener::new(&ndi, &listeners).unwrap();
            let sent = send_advertiser
                .register(&sender, &SendAdvertisement::default())
                .unwrap();
            let received = recv_advertiser
                .register(&receiver, &RecvAdvertisement::default())
                .unwrap();
            // Each owner is used, and then destroyed, on a thread that did
            // not create it.
            std::thread::spawn(move || {
                router.clear_route().unwrap();
                assert_eq!(router.source().unwrap().name(), "moved router");
                send_advertiser.unregister(sent).unwrap();
                recv_advertiser.unregister(received).unwrap();
                assert_eq!(send_listener.current_senders().unwrap().len(), 2);
                assert_eq!(recv_listener.current_receivers().unwrap().len(), 2);
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

        "native_failure_names" => {
            let assert_name = |error: Error, expected: &std::ffi::CStr| {
                assert!(
                    matches!(error, Error::NativeOperationFailed { operation } if operation == expected)
                );
            };
            let mut router = Router::new(&ndi, &RouterOptions::new("refusal names")).unwrap();
            std::env::set_var("GRAFTON_FAKE_ROUTING_CHANGE", "refuse");
            assert_name(
                router
                    .set_route(&Source::new("target").unwrap())
                    .unwrap_err(),
                c"NDIlib_routing_change",
            );
            std::env::remove_var("GRAFTON_FAKE_ROUTING_CHANGE");
            std::env::set_var("GRAFTON_FAKE_ROUTING_CLEAR", "refuse");
            assert_name(router.clear_route().unwrap_err(), c"NDIlib_routing_clear");
            std::env::remove_var("GRAFTON_FAKE_ROUTING_CLEAR");

            let sender = Sender::new(&ndi, &SenderOptions::new("refused registration")).unwrap();
            let receiver =
                Receiver::new(&ndi, &ReceiverOptions::new(Source::new("").unwrap())).unwrap();
            let mut send_advertiser = SendAdvertiser::new(&ndi, &advertisers).unwrap();
            let mut recv_advertiser = RecvAdvertiser::new(&ndi, &advertisers).unwrap();
            std::env::set_var("GRAFTON_FAKE_ADVERTISER_ADD", "refuse");
            assert_name(
                send_advertiser
                    .register(&sender, &SendAdvertisement::default())
                    .unwrap_err(),
                c"NDIlib_send_advertiser_add_sender",
            );
            assert_name(
                recv_advertiser
                    .register(&receiver, &RecvAdvertisement::default())
                    .unwrap_err(),
                c"NDIlib_recv_advertiser_add_receiver",
            );
            std::env::remove_var("GRAFTON_FAKE_ADVERTISER_ADD");
            let send_registration = send_advertiser
                .register(&sender, &SendAdvertisement::default())
                .unwrap();
            let recv_registration = recv_advertiser
                .register(&receiver, &RecvAdvertisement::default())
                .unwrap();
            std::env::set_var("GRAFTON_FAKE_ADVERTISER_REMOVE", "refuse");
            for (rejected, expected) in [
                (
                    send_advertiser
                        .unregister(send_registration)
                        .map_err(|rejected| rejected.into_parts().0),
                    c"NDIlib_send_advertiser_del_sender",
                ),
                (
                    recv_advertiser
                        .unregister(recv_registration)
                        .map_err(|rejected| rejected.into_parts().0),
                    c"NDIlib_recv_advertiser_del_receiver",
                ),
            ] {
                assert_name(rejected.unwrap_err(), expected);
            }
            std::env::remove_var("GRAFTON_FAKE_ADVERTISER_REMOVE");

            let mut listener = RecvListener::new(&ndi, &listeners).unwrap();
            let snapshot = listener.current_receivers().unwrap();
            let receiver = snapshot[0].id().unwrap();
            std::env::set_var("GRAFTON_FAKE_LISTENER_CONNECT", "refuse");
            assert_name(
                listener.request_connect(receiver, "target").unwrap_err(),
                c"NDIlib_recv_listener_send_connect",
            );
            assert_name(
                listener.request_disconnect(receiver).unwrap_err(),
                c"NDIlib_recv_listener_send_connect",
            );
            std::env::remove_var("GRAFTON_FAKE_LISTENER_CONNECT");
        }
        "routing" => {
            let mut router =
                Router::new(&ndi, &RouterOptions::new("Program").groups(["public"])).unwrap();
            assert_eq!(router.source().unwrap().name(), "Program");
            assert_eq!(router.connection_count().unwrap(), 0);
            router
                .set_route(&Source::new("STUDIO (Camera 2)").unwrap())
                .unwrap();
            router.clear_route().unwrap();
            assert!(router.wait_for_connection(Duration::MAX).is_err());
            drop(router);
            assert_eq!(
                log(),
                "initialize\nrouter-create\nrouter-change STUDIO (Camera 2)\nrouter-clear\nrouter-destroy\n"
            );
        }
        _ => panic!("unknown discovery scenario {scenario}"),
    }
    drop(ndi);
    assert!(!Ndi::is_running());
}
