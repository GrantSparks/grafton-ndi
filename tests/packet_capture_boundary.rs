//! Packet capture shares raw capture's bounded wait and ownership boundary.
#![cfg(all(
    target_os = "linux",
    feature = "advanced",
    ndi_has_advanced_header,
    not(ndi_link_static)
))]
#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(child, fixture);
use common::TestChild;

#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;
use grafton_ndi::{
    advanced::*, Error, Ndi, ReceiverEvent, ReceiverOptions, RuntimeOptions, SdkResultDefect,
    Source,
};
use runtime_fixture::{read_fake_log, NativeGate};
use std::time::Duration;

#[test]
fn borrowed_packet_capture_retains_native_buffers_and_preserves_events() {
    if let Some(library) = std::env::var_os("GRAFTON_PACKET_CAPTURE_CHILD") {
        let ndi = Ndi::with_options(&RuntimeOptions::new().library_path(library)).unwrap();
        let packets = PacketReceiver::new(
            &ndi,
            &ReceiverOptions::new(Source::new("fixture").unwrap()),
            PassthroughPreset::ProtectedWithAudio,
        )
        .unwrap();
        let frees = |kind: &str| {
            read_fake_log()
                .lines()
                .filter(|line| *line == format!("free-{kind}"))
                .count()
        };
        for (serial, kind) in [(1, "packet-video"), (2, "packet-audio")] {
            std::env::set_var("GRAFTON_FAKE_CAPTURE_SCRIPT", format!("{serial}:{kind}"));
            let medium = if kind == "packet-video" {
                "video"
            } else {
                "audio"
            };
            let before = frees(medium);
            let frame = packets.capture_any_ref(Duration::ZERO).unwrap().unwrap();
            assert_eq!(frees(medium), before);
            let pointer = match &frame {
                PacketItemRef::Video(ReceivedVideoRef::Compressed(frame)) => {
                    assert_eq!(frame.metadata(), Some("<packet/>"));
                    frame.data().as_ptr()
                }
                PacketItemRef::Audio(ReceivedAudioRef::Compressed(frame)) => {
                    assert_eq!(frame.data(), [1, 2, 3, 4]);
                    frame.data().as_ptr()
                }
                other => panic!("{other:?}"),
            };
            assert!(read_fake_log().contains(&format!("{medium}-data:{}", pointer as usize)));
            packets.receiver().connection_stats().unwrap();
            drop(frame);
            assert_eq!(frees(medium), before + 1);
            std::env::set_var(
                "GRAFTON_FAKE_CAPTURE_SCRIPT",
                format!("{}:{kind}", serial + 10),
            );
            assert!(packets.capture_any(Duration::ZERO).unwrap().is_some());
            assert_eq!(frees(medium), before + 2);
        }
        for (serial, kind) in [
            (20, "video"),
            (21, "audio"),
            (22, "metadata"),
            (23, "status"),
        ] {
            std::env::set_var("GRAFTON_FAKE_CAPTURE_SCRIPT", format!("{serial}:{kind}"));
            assert!(packets.capture_any_ref(Duration::ZERO).unwrap().is_some());
        }
        std::env::set_var("GRAFTON_FAKE_CAPTURE_SCRIPT", "24:bad-packet-video");
        let before = frees("video");
        assert!(matches!(
            packets.capture_any_ref(Duration::ZERO),
            Err(Error::InvalidFrame(_))
        ));
        assert_eq!(frees("video"), before + 1);
        std::env::set_var("GRAFTON_FAKE_CAPTURE_SCRIPT", "25:none");
        assert!(packets.capture_any_ref(Duration::ZERO).unwrap().is_none());
        assert!(matches!(
            packets.capture_any_ref(Duration::MAX),
            Err(Error::InvalidInput(_))
        ));

        // The connection-change permit stays held during both captures.
        // The fake log proves both entry points still reached the SDK.
        let gate = std::path::PathBuf::from(std::env::var_os("GRAFTON_FAKE_CONNECT_GATE").unwrap());
        std::env::set_var("GRAFTON_FAKE_CAPTURE_SCRIPT", "26:none");
        std::thread::scope(|scope| {
            let release = NativeGate(&gate);
            scope.spawn(|| {
                packets
                    .receiver()
                    .connect_to(&Source::new("Blocked").unwrap())
            });
            release.wait_for_entry();
            let (send, completed) = std::sync::mpsc::channel();
            let packets = &packets;
            scope.spawn(move || {
                let before = read_fake_log().len();
                assert!(packets.capture_any_ref(Duration::ZERO).unwrap().is_none());
                let first = read_fake_log();
                assert_eq!(
                    first[before..]
                        .lines()
                        .filter(|line| line.starts_with("capture:"))
                        .collect::<Vec<_>>(),
                    ["capture:none:111:0"]
                );
                let budget = Duration::from_millis(110);
                let start = std::time::Instant::now();
                assert!(packets.capture_any(budget).unwrap().is_none());
                assert!(start.elapsed() >= budget);
                assert!(read_fake_log()[first.len()..]
                    .lines()
                    .any(|line| line.starts_with("capture:none:111:")));
                send.send(()).unwrap();
            });
            let captured = completed.recv_timeout(Duration::from_secs(2));
            drop(release);
            captured.expect("packet capture waited for native connect");
        });

        // A malformed status snapshot must not pin the shared AllSlots kernel
        // ahead of source notifications or either compressed media family.
        std::env::set_var("GRAFTON_FAKE_NEGATIVE_COUNT", "connections");
        std::env::set_var(
            "GRAFTON_FAKE_CAPTURE_SCRIPT",
            "27:status,source,packet-video,packet-audio",
        );
        assert!(matches!(
            packets.capture_any_ref(Duration::ZERO),
            Err(Error::UnexpectedSdkResult(SdkResultDefect::NegativeCount { operation, value }))
                if operation.name().to_str().unwrap() == "NDIlib_recv_get_no_connections"
                    && value.get() == -1
        ));
        assert!(matches!(
            packets.capture_any(Duration::ZERO).unwrap(),
            Some(PacketItem::Event(ReceiverEvent::SourceChanged))
        ));
        let captures = || {
            read_fake_log()
                .lines()
                .filter(|line| line.starts_with("capture:"))
                .count()
        };
        let before = captures();
        assert!(matches!(
            packets.capture_any_ref(Duration::ZERO).unwrap(),
            Some(PacketItemRef::Video(ReceivedVideoRef::Compressed(_)))
        ));
        assert_eq!(
            captures(),
            before + 1,
            "packet capture did not reach the SDK"
        );
        assert!(matches!(
            packets.capture_any(Duration::ZERO).unwrap(),
            Some(PacketItem::Audio(ReceivedAudio::Compressed(_)))
        ));
        std::env::remove_var("GRAFTON_FAKE_NEGATIVE_COUNT");
        let before = captures();
        assert!(matches!(
            packets.capture_any(Duration::ZERO).unwrap(),
            Some(PacketItem::Event(ReceiverEvent::StatusChanged(status)))
                if status.connections() == 0
        ));
        assert_eq!(captures(), before, "recovered status was not retained");
        assert!(packets.capture_any_ref(Duration::ZERO).unwrap().is_none());
        return;
    }
    let dir = common::FixtureDir::create("packet-capture");
    let library = dir.path().join("libndi_packet.so");
    runtime_fixture::FakeRuntime {
        cfgs: vec![
            "fake_objects",
            "fake_advanced",
            "fake_capture",
            "fake_blocking_connect",
        ],
        version: Some("6.4.0.2"),
        ..Default::default()
    }
    .build(&library);
    TestChild::new("borrowed_packet_capture_retains_native_buffers_and_preserves_events")
        .env("GRAFTON_PACKET_CAPTURE_CHILD", library)
        .env("GRAFTON_FAKE_CONNECT_GATE", dir.path())
        .env("GRAFTON_FAKE_NDI_LOG", dir.path().join("capture.log"))
        .run();
}
