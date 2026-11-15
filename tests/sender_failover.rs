//! A receiver follows a failover whose caller-owned Source was dropped.

#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(runtime, name, loopback);

use grafton_ndi::{
    Error, PixelFormat, Receiver, ReceiverOptions, Sender, SenderOptions, Timecode, VideoFrame,
};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

fn marked_frame(value: u8) -> VideoFrame {
    let mut frame = VideoFrame::builder()
        .resolution(64, 32)
        .pixel_format(PixelFormat::Bgra)
        .timecode(Timecode::from_ticks(i64::from(value)))
        .build()
        .unwrap();
    frame.data_mut().fill(value);
    frame
}

/// End the background sender even when a delivery assertion unwinds.
struct StopOnDrop<'a>(&'a AtomicBool);
impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn receive_marker(receiver: &Receiver, marker: u8, deadline: Instant, mut send: impl FnMut()) {
    loop {
        send();
        match receiver.video().capture_ref(Duration::from_millis(100)) {
            Ok(Some(frame)) if frame.timecode() == Timecode::from_ticks(i64::from(marker)) => {
                return
            }
            Ok(_) | Err(Error::ConnectionLost) => {}
            Err(error) => panic!("capture failed: {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "receiver never received marker {marker}"
        );
    }
}

#[test]
fn a_receiver_follows_failover_after_its_input_source_and_primary_are_dropped() {
    let Some(ndi) = common::ndi() else {
        return;
    };
    let backup = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("retained failover backup")).clock_video(false),
    )
    .unwrap();
    let primary = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("retained failover primary")).clock_video(false),
    )
    .unwrap();
    {
        let target = common::loopback_source(&ndi, &backup).unwrap();
        primary.set_failover(&target).unwrap();
    }
    let receiver = Receiver::new(
        &ndi,
        &ReceiverOptions::new(common::loopback_source(&ndi, &primary).unwrap()),
    )
    .unwrap();
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let _stop_on_unwind = StopOnDrop(&stop);
        scope.spawn(|| {
            let frame = marked_frame(119);
            while !stop.load(Ordering::Acquire) {
                backup.send_video(&frame).unwrap();
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let frame = marked_frame(17);
        receive_marker(
            &receiver,
            17,
            Instant::now() + Duration::from_secs(15),
            || {
                primary.send_video(&frame).unwrap();
            },
        );
        drop(primary);
        receive_marker(
            &receiver,
            119,
            Instant::now() + Duration::from_secs(20),
            || {},
        );
    });
}
