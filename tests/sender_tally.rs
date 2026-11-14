//! A sender's tally query reports the state a receiver published, every time.
#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(runtime, name, loopback);

use grafton_ndi::{
    Error, FrameRate, Receiver, ReceiverOptions, Sender, SenderOptions, Tally, VideoFrame,
};
use std::time::{Duration, Instant};

/// Keep control connections active while changing tally, with a guard so a
/// failed assertion stops the producer before scoped threads are joined.
fn with_connected_sender(label: &str, run: impl FnOnce(&Sender, &Receiver)) {
    use std::{
        sync::atomic::{AtomicBool, Ordering},
        thread,
    };
    let Some(ndi) = common::ndi() else { return };
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name(label))
            .clock_video(false)
            .clock_audio(false),
    )
    .unwrap();
    let receiver = Receiver::new(
        &ndi,
        &ReceiverOptions::new(common::loopback_source(&ndi, &sender).unwrap()),
    )
    .unwrap();
    let video = VideoFrame::builder().resolution(64, 32).build().unwrap();
    let stopped = AtomicBool::new(false);
    struct Stop<'a>(&'a AtomicBool);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    thread::scope(|scope| {
        let producer = scope.spawn(|| {
            while !stopped.load(Ordering::Acquire) {
                sender.send_video(&video).unwrap();
                thread::sleep(Duration::from_millis(10));
            }
        });
        let stop = Stop(&stopped);
        receiver.set_tally(Tally::new(true, false));
        let end = Instant::now() + Duration::from_secs(15);
        while sender
            .wait_for_tally_change(Duration::from_millis(100))
            .unwrap()
            != Some(Tally::new(true, false))
        {
            assert!(
                Instant::now() < end,
                "the control receiver never published its initial tally"
            );
        }
        run(&sender, &receiver);
        drop(stop);
        producer.join().unwrap();
    });
}

#[test]
fn polling_a_tally_change_preserves_it_for_the_next_wait() {
    with_connected_sender("poll then wait tally", |sender, receiver| {
        for next in [Tally::new(false, true), Tally::new(true, false)]
            .into_iter()
            .cycle()
            .take(6)
        {
            receiver.set_tally(next);
            let end = Instant::now() + Duration::from_secs(3);
            while sender.tally() != next {
                assert!(Instant::now() < end, "polling did not observe {next:?}");
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(
                sender
                    .wait_for_tally_change(Duration::from_millis(300))
                    .unwrap(),
                Some(next)
            );
            assert!(sender
                .wait_for_tally_change(Duration::from_millis(20))
                .unwrap()
                .is_none());
        }
    });
}

#[test]
fn concurrent_tally_pollers_cannot_take_a_waiters_change() {
    use std::{
        sync::atomic::{AtomicBool, Ordering},
        thread,
    };
    with_connected_sender("concurrent tally", |sender, receiver| {
        for next in [Tally::new(false, true), Tally::new(true, false)]
            .into_iter()
            .cycle()
            .take(6)
        {
            let finished = AtomicBool::new(false);
            thread::scope(|scope| {
                let waiter = scope.spawn(|| {
                    let result = sender.wait_for_tally_change(Duration::from_secs(2));
                    finished.store(true, Ordering::Release);
                    result.unwrap()
                });
                thread::sleep(Duration::from_millis(40));
                let poller = scope.spawn(|| {
                    while !finished.load(Ordering::Acquire) {
                        let _ = sender.tally();
                        thread::sleep(Duration::from_millis(1));
                    }
                });
                // Start the poller and publish together to exercise the narrow
                // native wakeup window, as well as polling between waits.
                receiver.set_tally(next);
                assert_eq!(waiter.join().unwrap(), Some(next));
                poller.join().unwrap();
            });
        }
    });
}

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
