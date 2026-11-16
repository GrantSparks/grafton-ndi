#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(runtime, name, loopback);
use grafton_ndi::{
    MetadataFrame, MetadataFrameRef, MetadataSource, Receiver, ReceiverOptions, Sender,
    SenderOptions, Timecode, VideoFrame,
};
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
    let receiver = Receiver::new(
        &ndi,
        &ReceiverOptions::new(grafton_ndi::Source::new("").unwrap()),
    )?;
    let text = "<receiver-registration/>";
    receiver.add_connection_metadata(
        &MetadataFrame::new(text)?.with_timecode(Timecode::from_ticks(21)),
    )?;
    let target = common::loopback_source(&ndi, &sender)?;
    receiver.connect_to(&target);
    let video = VideoFrame::builder().resolution(16, 16).build()?;
    for attempt in 0..2 {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            sender.send_video(&video)?;
            receiver.capture_video_ref(Duration::from_millis(10))?;
            if let Some(metadata) = sender.capture_metadata(Duration::from_millis(100))? {
                if metadata.data() == text {
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "receiver registration missing on connection {attempt}"
            );
        }
        if attempt == 0 {
            while sender.capture_metadata(Duration::ZERO)?.is_some() {}
            receiver.disconnect();
            receiver.reconnect();
        }
    }
    receiver.clear_connection_metadata()?;
    Ok(())
}
