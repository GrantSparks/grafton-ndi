mod common;
use grafton_ndi::{
    MetadataFrame, MetadataSource, Receiver, ReceiverOptions, Sender, SenderOptions, Timecode,
    VideoFrame,
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
        if let Some(received) = receiver
            .metadata()
            .capture_ref(Duration::from_millis(100))?
        {
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
                return Ok(());
            }
        }
        assert!(
            Instant::now() < deadline,
            "metadata did not reach loopback receiver"
        );
    }
}
