//! Attached receivers observe capability changes through their shared queries.
#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
#[cfg(feature = "advanced")]
common_helpers!(runtime, name, loopback, advanced);
#[cfg(not(feature = "advanced"))]
common_helpers!(runtime, name, loopback);

use grafton_ndi::{
    Error, FrameSync, MetadataFrame, Ndi, Receiver, ReceiverMode, ReceiverOptions, Sender,
    SenderOptions, Timecode,
};
use std::time::{Duration, Instant};

fn make_receiver(ndi: &Ndi, name: &str) -> Result<(Sender, Receiver), Error> {
    let sender = Sender::new(ndi, &SenderOptions::new(common::unique_name(name)))?;
    let source = common::loopback_source(ndi, &sender)?;
    let receiver = Receiver::new(ndi, &ReceiverOptions::new(source))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !receiver.is_connected() {
        assert!(Instant::now() < deadline, "receiver did not connect");
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok((sender, receiver))
}

fn query_change<M: ReceiverMode>(sender: &Sender, receiver: &Receiver<M>) -> Result<(), Error> {
    assert!(!receiver.ptz().is_supported());
    sender.clear_connection_metadata();
    sender.add_connection_metadata(
        &MetadataFrame::new(
            "<ndi_capabilities ntk_ptz=\"true\" web_control=\"http://127.0.0.1/attached\"/>",
        )?
        .with_timecode(Timecode::from_ticks(0)),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !receiver.ptz().is_supported()
        || receiver.web_control_url()?.as_deref() != Some("http://127.0.0.1/attached")
    {
        assert!(
            Instant::now() < deadline,
            "attached receiver did not observe capabilities"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(receiver.is_connected());
    assert!(receiver.connection_stats().connections() > 0);
    Ok(())
}

#[test]
fn framesync_receiver_queries_reflect_capability_changes() -> Result<(), Error> {
    let Some(ndi) = common::ndi() else {
        return Ok(());
    };
    let (sender, receiver) = make_receiver(&ndi, "FrameSync capabilities")?;
    let sync = FrameSync::new(receiver)?;
    query_change(&sender, sync.receiver())
}

#[cfg(feature = "advanced")]
#[test]
fn avsync_receiver_queries_reflect_capability_changes() -> Result<(), Error> {
    let Some(ndi) = common::advanced_ndi() else {
        return Ok(());
    };
    let (sender, receiver) = make_receiver(&ndi, "AV sync capabilities")?;
    let sync = grafton_ndi::advanced::AvSync::new(receiver)?;
    query_change(&sender, sync.receiver())
}
