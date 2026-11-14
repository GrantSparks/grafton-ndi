//! Packet capture shares raw capture's bounded wait and ownership boundary.
#![cfg(all(
    target_os = "linux",
    feature = "advanced",
    ndi_has_advanced_header,
    not(ndi_link_static)
))]
#[path = "support/fixture.rs"]
mod fixture;
#[path = "support/runtime_fixture.rs"]
mod runtime_fixture;
use grafton_ndi::{advanced::*, Error, Ndi, ReceiverOptions, RuntimeOptions, Source};
use std::{fs, process::Command, time::Duration};

#[test]
fn borrowed_packet_capture_retains_native_buffers_and_preserves_events() {
    if let Some(library) = std::env::var_os("GRAFTON_PACKET_CAPTURE_CHILD") {
        let ndi = Ndi::with_options(&RuntimeOptions::new().library_path(library)).unwrap();
        let packets = PacketReceiver::new(
            &ndi,
            &ReceiverOptions::new(Source::new("fixture")),
            PassthroughPreset::ProtectedWithAudio,
        )
        .unwrap();
        let log_path = std::env::var_os("GRAFTON_FAKE_NDI_LOG").unwrap();
        let log = || fs::read_to_string(&log_path).unwrap();
        let frees = |kind: &str| {
            log()
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
            let frame = packets.capture_ref(Duration::ZERO).unwrap().unwrap();
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
            assert!(log().contains(&format!("{medium}-data:{}", pointer as usize)));
            packets.receiver().connection_stats();
            drop(frame);
            assert_eq!(frees(medium), before + 1);
            std::env::set_var(
                "GRAFTON_FAKE_CAPTURE_SCRIPT",
                format!("{}:{kind}", serial + 10),
            );
            assert!(packets.capture(Duration::ZERO).unwrap().is_some());
            assert_eq!(frees(medium), before + 2);
        }
        for (serial, kind) in [
            (20, "video"),
            (21, "audio"),
            (22, "metadata"),
            (23, "status"),
        ] {
            std::env::set_var("GRAFTON_FAKE_CAPTURE_SCRIPT", format!("{serial}:{kind}"));
            assert!(packets.capture_ref(Duration::ZERO).unwrap().is_some());
        }
        std::env::set_var("GRAFTON_FAKE_CAPTURE_SCRIPT", "24:bad-packet-video");
        let before = frees("video");
        assert!(matches!(
            packets.capture_ref(Duration::ZERO),
            Err(Error::InvalidFrame(_))
        ));
        assert_eq!(frees("video"), before + 1);
        std::env::set_var("GRAFTON_FAKE_CAPTURE_SCRIPT", "25:none");
        assert!(packets.capture_ref(Duration::ZERO).unwrap().is_none());
        assert!(matches!(
            packets.capture_ref(Duration::MAX),
            Err(Error::InvalidInput(_))
        ));

        // A reconnect waits only for the attempt already holding the shared
        // capture guard. The following capture retains the original deadline.
        std::env::set_var("GRAFTON_FAKE_CAPTURE_SCRIPT", "26:none");
        std::env::set_var("GRAFTON_FAKE_CAPTURE_DELAY_MS", "40");
        let calls = log()
            .lines()
            .filter(|line| line.starts_with("capture:"))
            .count();
        std::thread::scope(|scope| {
            let capture = scope.spawn(|| {
                assert!(packets
                    .capture_ref(Duration::from_millis(110))
                    .unwrap()
                    .is_none());
            });
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while log()
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
        return;
    }
    let dir = fixture::FixtureDir::create("packet-capture");
    let library = dir.path().join("libndi_packet.so");
    runtime_fixture::FakeRuntime {
        cfgs: vec!["fake_objects", "fake_advanced", "fake_capture"],
        version: Some("6.4.0.2"),
        ..Default::default()
    }
    .build(&library);
    let result = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "borrowed_packet_capture_retains_native_buffers_and_preserves_events",
            "--nocapture",
        ])
        .env("GRAFTON_PACKET_CAPTURE_CHILD", library)
        .env("GRAFTON_FAKE_NDI_LOG", dir.path().join("capture.log"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
