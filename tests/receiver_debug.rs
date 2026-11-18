//! Receiver identities are available through every wrapper without SDK work.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(child, fixture);

#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;

use grafton_ndi::{FrameSync, Ndi, Receiver, ReceiverOptions, RuntimeOptions, Source};

#[test]
fn wrappers_print_the_named_receiver_without_native_calls() {
    if let Some(library) = std::env::var_os("GRAFTON_DEBUG_RECEIVER_RUNTIME") {
        exercise(&Ndi::with_options(&RuntimeOptions::new().library_path(library)).unwrap());
        return;
    }
    let fixture = common::FixtureDir::create("receiver-debug");
    let library = runtime_fixture::FakeRuntime {
        cfgs: vec![
            "fake_objects",
            "fake_advanced",
            "fake_receiver_controls",
            "fake_capability_descriptors",
        ],
        ..Default::default()
    }
    .build(&fixture.path().join("fake-ndi.so"));
    common::TestChild::new("wrappers_print_the_named_receiver_without_native_calls")
        .env("GRAFTON_DEBUG_RECEIVER_RUNTIME", library)
        .env("GRAFTON_FAKE_NDI_LOG", fixture.path().join("native.log"))
        .run();
}

const NAME: &str = "debug studio";

fn exercise(ndi: &Ndi) {
    let options = ReceiverOptions::new(Source::new("").unwrap()).name(NAME);
    let receiver = || Receiver::new(ndi, &options).unwrap();

    check(&receiver(), "Receiver", "RawCapture");
    let sync = FrameSync::new(receiver()).unwrap();
    check(&sync, "FrameSync", "FrameSyncAttached");

    #[cfg(feature = "tokio")]
    check(
        &grafton_ndi::asynchronous::tokio::AsyncReceiver::new(receiver()),
        "AsyncReceiver",
        "RawCapture",
    );
    #[cfg(feature = "smol")]
    check(
        &grafton_ndi::asynchronous::smol::AsyncReceiver::new(receiver()),
        "AsyncReceiver",
        "RawCapture",
    );

    #[cfg(feature = "advanced")]
    advanced(ndi, &options, receiver, sync);
}

fn check(value: &dyn std::fmt::Debug, kind: &str, mode: &str) {
    let log = runtime_fixture::read_fake_log;
    let before = log();
    let debug = format!("{value:?}");
    assert!(debug.starts_with(kind), "{debug}");
    assert!(debug.contains(NAME), "{debug}");
    assert!(debug.contains(mode), "{debug}");
    assert!(debug.ends_with(".. }"), "{debug}");
    assert!(!debug.contains("0x"), "{debug}");
    assert_eq!(log(), before);
}

#[cfg(feature = "advanced")]
fn advanced(
    ndi: &Ndi,
    options: &ReceiverOptions,
    receiver: impl Fn() -> Receiver,
    mut sync: FrameSync,
) {
    use grafton_ndi::{
        advanced::{AllocatorLimits, AllocatorService, AvSync, PacketReceiver, PassthroughPreset},
        CapabilityRefusal, CapabilityStatus, RuntimeFeature,
    };

    let compiled = |feature| match ndi.capabilities().status(feature) {
        CapabilityStatus::Available => true,
        CapabilityStatus::Unavailable(CapabilityRefusal::BuildAbiUnavailable(_)) => false,
        status => panic!("the fake runtime does not provide {feature:?}: {status:?}"),
    };

    let (capture, controls) = sync.split();
    check(&capture, "FrameSyncCapture", "FrameSyncAttached");
    check(&controls, "ReceiverControls", "FrameSyncAttached");
    if compiled(RuntimeFeature::Kvm) {
        let session = controls.kvm().unwrap();
        check(&session, "KvmSession", "FrameSyncAttached");
        check(&session.receiver(), "KvmReceiver", "FrameSyncAttached");
    }

    if compiled(RuntimeFeature::AvSync) {
        let mut av = AvSync::new(receiver()).unwrap();
        check(&av, "AvSync", "AvSyncAttached");
        let (capture, controls) = av.split();
        check(&capture, "AvSyncCapture", "AvSyncAttached");
        check(&controls, "ReceiverControls", "AvSyncAttached");
    }

    let preset = PassthroughPreset::SpeedHqOnly;
    if compiled(RuntimeFeature::Passthrough(preset)) {
        let packets = PacketReceiver::new(ndi, options, preset).unwrap();
        check(&packets, "PacketReceiver", "PacketAttached");
    }

    if compiled(RuntimeFeature::ReceiveAllocators) {
        let service =
            AllocatorService::new(ndi.process_lifetime(), &AllocatorLimits::default()).unwrap();
        let allocated = service.attach(receiver()).unwrap();
        check(&allocated, "AllocatedReceiver", "RawCapture");
    }
}
