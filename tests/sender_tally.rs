//! A sender's tally query reports the state a receiver published, every time.
#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(runtime, name, loopback);

use grafton_ndi::{
    Error, FrameRate, Receiver, ReceiverOptions, Sender, SenderOptions, Tally, VideoFrame,
};
use std::time::{Duration, Instant};

#[test]
fn current_tally_stays_on_program_across_polls_after_the_change_was_observed() {
    let Some(ndi) = common::ndi() else { return };
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("tally regression"))
            .clock_video(true)
            .clock_audio(false),
    )
    .unwrap();
    let source = common::loopback_source(&ndi, &sender).unwrap();
    let receiver = Receiver::new(&ndi, &ReceiverOptions::new(source)).unwrap();
    receiver.set_tally(Tally::new(true, false));
    let video = VideoFrame::builder()
        .resolution(64, 32)
        .frame_rate(FrameRate::new(30, 1).unwrap())
        .build()
        .unwrap();

    // The change is reported once; wait for the one that puts it on program.
    let deadline = Instant::now() + Duration::from_secs(15);
    let observed = loop {
        sender.send_video(&video).unwrap();
        if let Some(tally) = sender
            .wait_for_tally_change(Duration::from_millis(100))
            .unwrap()
        {
            if tally.on_program() {
                break tally;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the sender never saw the receiver's program tally"
        );
    };
    assert!(!observed.on_preview());

    // With no further change, the current state is still reported.
    for _ in 0..3 {
        let tally = sender.tally();
        assert!(tally.on_program() && !tally.on_preview());
    }
    // A wait with nothing new to report times out rather than repeating it.
    assert!(sender
        .wait_for_tally_change(Duration::from_millis(50))
        .unwrap()
        .is_none());
}

#[test]
fn waiting_for_a_tally_change_rejects_an_oversized_timeout() {
    let Some(ndi) = common::ndi() else { return };
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("tally timeout")),
    )
    .unwrap();
    assert!(matches!(
        sender.wait_for_tally_change(Duration::MAX),
        Err(Error::InvalidInput(_))
    ));
}
