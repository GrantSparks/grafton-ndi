#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(runtime, name, loopback);
use grafton_ndi::{
    MetadataFrame, MetadataFrameRef, MetadataSource, Receiver, ReceiverOptions, RuntimeInfo,
    SdkEdition, SdkLinkage, Sender, SenderOptions, Timecode, VideoFrame,
};
use std::io::Write;
use std::time::{Duration, Instant};

#[test]
fn reused_metadata_reaches_receiver_with_its_timecode() -> Result<(), grafton_ndi::Error> {
    let Some(ndi) = common::ndi() else {
        return Ok(());
    };
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("metadata roundtrip regression"))
            .clock_video(false),
    )?;
    let connection = MetadataFrame::new("<connection/>")?.with_timecode(Timecode::from_ticks(17));
    sender.add_connection_metadata(&connection);

    let source = common::loopback_source(&ndi, &sender)?;
    let receiver = Receiver::new(&ndi, &ReceiverOptions::new(source))?;
    let video = VideoFrame::builder().resolution(16, 16).build()?;
    let metadata =
        MetadataFrame::new("<payload>é</payload>")?.with_timecode(Timecode::from_ticks(123_456));
    let pointer = metadata.data().as_ptr();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        sender.send_video(&video)?;
        sender.send_metadata(&metadata);
        assert_eq!(metadata.data().as_ptr(), pointer);
        if let Some(received) = receiver.capture_metadata_ref(Duration::from_millis(100))? {
            if received.data() == metadata.data() {
                assert_eq!(received.timecode(), metadata.timecode());
                // Forward the actual capture, including through a type-erased
                // source, and register it for future connections. The capture
                // can be freed after these synchronous copies complete.
                let source: &dyn MetadataSource = &received;
                receiver.send_metadata(source)?;
                sender.add_connection_metadata(source);
                sender.send_metadata(source);
                drop(received);
                let upstream_deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    if let Some(upstream) = sender.capture_metadata(Duration::from_millis(100))? {
                        if upstream.data() == metadata.data() {
                            assert_eq!(upstream.timecode(), metadata.timecode());
                            break;
                        }
                    }
                    assert!(
                        Instant::now() < upstream_deadline,
                        "borrowed metadata did not reach its source"
                    );
                }
                sender.clear_connection_metadata();
                check_borrowed_sender_metadata(&sender, &receiver)?;
                return Ok(());
            }
        }
        assert!(
            Instant::now() < deadline,
            "metadata did not reach loopback receiver"
        );
    }
}

/// Borrow and release native metadata only after this same connection has
/// completed the original owned roundtrip above.
fn check_borrowed_sender_metadata(sender: &Sender, receiver: &Receiver) -> grafton_ndi::Result<()> {
    let second = MetadataFrame::new("<payload>borrowed second</payload>")?
        .with_timecode(Timecode::from_ticks(123_457));
    let third = MetadataFrame::new("<payload>after borrowed free</payload>")?
        .with_timecode(Timecode::from_ticks(123_458));
    receiver.send_metadata(&second)?;
    let borrowed = capture_expected_metadata(sender, &second, "second borrowed acquisition")?;
    receiver.send_metadata(&third)?;
    std::thread::scope(|scope| -> grafton_ndi::Result<()> {
        let (release, released) = std::sync::mpsc::channel();
        let worker = scope.spawn(move || {
            released.recv().unwrap();
            assert_eq!(borrowed.data(), "<payload>borrowed second</payload>");
            assert_eq!(borrowed.timecode(), Timecode::from_ticks(123_457));
            drop(borrowed);
        });
        let started = Instant::now();
        assert!(sender
            .capture_metadata_ref(Duration::from_millis(30))?
            .is_none());
        assert!(started.elapsed() >= Duration::from_millis(20));
        release.send(()).unwrap();
        worker.join().unwrap();
        let resumed = capture_expected_metadata(sender, &third, "third capture after native free")?;
        assert_eq!(resumed.data(), third.data());
        assert_eq!(resumed.timecode(), third.timecode());
        Ok(())
    })
}

fn capture_expected_metadata<'sender>(
    sender: &'sender Sender,
    expected: &MetadataFrame,
    phase: &str,
) -> grafton_ndi::Result<MetadataFrameRef<'sender>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut frames = 0;
    let mut unexpected = Vec::new();
    loop {
        if let Some(frame) = sender.capture_metadata_ref(Duration::from_millis(100))? {
            frames += 1;
            if frame.data() == expected.data() {
                assert_eq!(frame.timecode(), expected.timecode());
                return Ok(frame);
            }
            if unexpected.len() < 4 {
                unexpected.push((
                    frame.timecode(),
                    frame.data().len(),
                    frame.data().chars().take(160).collect::<String>(),
                ));
            }
        }
        assert!(
            Instant::now() < deadline,
            "{phase}: expected metadata did not arrive; frames: {frames}, \
             samples (timecode, bytes, text): {unexpected:?}; sender connections: {:?}",
            sender.connection_count(),
        );
    }
}

#[test]
fn receiver_connection_metadata_survives_its_input_and_reconnect() -> Result<(), grafton_ndi::Error>
{
    let Some(ndi) = common::ndi() else {
        return Ok(());
    };
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("receiver connection metadata")).clock_video(false),
    )?;
    let receiver = Receiver::new(&ndi, &ReceiverOptions::disconnected())?;
    let text = "<receiver-registration/>";
    receiver.add_connection_metadata(
        &MetadataFrame::new(text)?.with_timecode(Timecode::from_ticks(21)),
    )?;
    let target = common::loopback_source(&ndi, &sender)?;
    receiver.connect_to(&target);
    for connection in 0..2 {
        let timecode = Timecode::from_ticks(1000 + connection);
        let video = VideoFrame::builder()
            .resolution(16, 16)
            .timecode(timecode)
            .build()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut received_video = false;
        let mut received_registration = false;
        while Instant::now() < deadline {
            sender.send_video(&video)?;
            received_video |= receiver
                .capture_video_ref(Duration::from_millis(10))?
                .is_some_and(|frame| frame.timecode() == timecode);
            received_registration = sender
                .capture_metadata(Duration::from_millis(100))?
                .is_some_and(|metadata| metadata.data() == text);
            if received_registration {
                break;
            }
        }
        if !received_registration {
            let info = ndi.info();
            let sending = sender.connection_count()?;
            let receiving = receiver.connection_stats()?.connections();
            assert!(
                has_known_registration_loss(info) && received_video && sending > 0 && receiving > 0,
                "receiver registration missing on connection {connection}; fresh video={received_video}, \
                 connections sender={sending} receiver={receiving}; runtime={info:?}"
            );
            let explicit =
                MetadataFrame::new(format!("<receiver-explicit stage='{connection}'/>"))?
                    .with_timecode(timecode);
            receiver.send_metadata(&explicit)?;
            drop(capture_expected_metadata(
                &sender,
                &explicit,
                "explicit upstream delivery after #551",
            )?);

            // Write directly to stderr so the advisory is visible in passing CI logs.
            let prefix = if std::env::var_os("GITHUB_ACTIONS").is_some() {
                "::warning title=NDI SDK known issue #551::"
            } else {
                ""
            };
            writeln!(
                std::io::stderr().lock(),
                "{prefix}Automatic receiver metadata missing on connection {connection} with {}. \
                 Fresh video and explicit upstream delivery verified. \
                 https://github.com/GrantSparks/grafton-ndi/issues/551",
                info.raw_version(),
            )
            .expect("write the SDK compatibility advisory");
        }
        if connection == 0 {
            receiver.disconnect();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let sending = sender.connection_count()?;
                let receiving = receiver.connection_stats()?.connections();
                if sending == 0 && receiving == 0 {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "disconnect did not settle; connections sender={sending} receiver={receiving}"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            while sender.capture_metadata(Duration::ZERO)?.is_some() {}
            receiver.reconnect();
        }
    }
    receiver.clear_connection_metadata()?;
    Ok(())
}

fn has_known_registration_loss(info: &RuntimeInfo) -> bool {
    // Scope #551 to the native Windows builds that reproduced the failure.
    cfg!(all(target_os = "windows", target_arch = "x86_64"))
        && info.edition() == SdkEdition::Standard
        && info.linkage() == SdkLinkage::Shared
        && matches!(
            info.raw_version(),
            "NDI SDK WIN64 16:38:09 Apr 14 2026 6.3.2.0"
                | "NDI SDK WIN64 13:07:15 Nov  6 2026 6.4.0.2"
                | "NDI SDK WIN64 16:09:44 Nov 17 2026 6.4.0.2"
        )
}
