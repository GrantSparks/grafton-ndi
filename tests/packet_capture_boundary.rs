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
use runtime_fixture::read_fake_log;
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

        // A reconnect waits only for the attempt already holding the shared
        // capture guard. The following capture retains the original deadline.
        std::env::set_var("GRAFTON_FAKE_CAPTURE_SCRIPT", "26:none");
        std::env::set_var("GRAFTON_FAKE_CAPTURE_DELAY_MS", "40");
        let calls = read_fake_log()
            .lines()
            .filter(|line| line.starts_with("capture:"))
            .count();
        std::thread::scope(|scope| {
            let capture = scope.spawn(|| {
                assert!(packets
                    .capture_any_ref(Duration::from_millis(110))
                    .unwrap()
                    .is_none());
            });
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while read_fake_log()
                .lines()
                .filter(|line| line.starts_with("capture:"))
                .count()
                == calls
            {
                assert!(
                    std::time::Instant::now() < deadline,
                    "capture did not enter native code"
                );
                std::thread::yield_now();
            }
            packets.receiver().reconnect();
            capture.join().unwrap();
        });
        std::env::remove_var("GRAFTON_FAKE_CAPTURE_DELAY_MS");

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
        cfgs: vec!["fake_objects", "fake_advanced", "fake_capture"],
        version: Some("6.4.0.2"),
        ..Default::default()
    }
    .build(&library);
    TestChild::new("borrowed_packet_capture_retains_native_buffers_and_preserves_events")
        .env("GRAFTON_PACKET_CAPTURE_CHILD", library)
        .env("GRAFTON_FAKE_NDI_LOG", dir.path().join("capture.log"))
        .run();
}
