use grafton_ndi::{
    MetadataFrame, Receiver, ReceiverOptions, Sender, SenderOptions, SourceAddress, VideoFrame, NDI,
};
use std::time::{Duration, Instant};

#[test]
#[ignore = "Requires NDI runtime and loopback networking"]
fn reused_metadata_reaches_receiver_with_its_timecode() -> Result<(), grafton_ndi::Error> {
    let ndi = NDI::new()?;
    let sender = Sender::new(
        &ndi,
        &SenderOptions::builder("metadata roundtrip regression")
            .clock_video(false)
            .build(),
    )?;
    let connection = MetadataFrame::with_data("<connection/>", 17)?;
    sender.add_connection_metadata(&connection);

    let mut source = sender.source()?;
    let port = match &source.address {
        SourceAddress::Ip(address) => address.rsplit(':').next().unwrap_or("5961"),
        _ => "5961",
    };
    source.address = SourceAddress::Ip(format!("127.0.0.1:{port}"));
    let receiver = Receiver::new(&ndi, &ReceiverOptions::builder(source).build())?;
    let video = VideoFrame::builder().resolution(16, 16).build()?;
    let metadata = MetadataFrame::with_data("<payload>é</payload>", 123_456)?;
    let pointer = metadata.data().as_ptr();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        sender.send_video(&video);
        sender.send_metadata(&metadata);
        assert_eq!(metadata.data().as_ptr(), pointer);
        if let Some(received) = receiver
            .metadata()
            .try_capture(Duration::from_millis(100))?
        {
            if received.data() == metadata.data() {
                assert_eq!(received.timecode(), metadata.timecode());
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
