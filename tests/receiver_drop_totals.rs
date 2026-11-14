//! Native receive-queue overflow establishes the meaning of received totals.
#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(runtime, name, loopback);

use grafton_ndi::{
    AudioFrame, Receiver, ReceiverBandwidth, ReceiverOptions, Sender, SenderOptions,
};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

#[test]
fn audio_queue_drops_are_part_of_received_total() {
    let Some(ndi) = common::ndi() else { return };
    eprintln!(
        "receiver drop regression: SDK {} {:?} on {} {}",
        ndi.info().raw_version(),
        ndi.info().edition(),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("receiver queue overflow")).clock_audio(false),
    )
    .unwrap();
    let source = common::loopback_source(&ndi, &sender).unwrap();
    let receiver = Receiver::new(
        &ndi,
        &ReceiverOptions::new(source).bandwidth(ReceiverBandwidth::AudioOnly),
    )
    .unwrap();
    let audio = AudioFrame::builder()
        .sample_rate(48_000)
        .num_channels(2)
        .num_samples(480)
        .build()
        .unwrap();
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        scope.spawn(|| {
            while !stop.load(Ordering::Relaxed) {
                sender.send_audio(&audio);
                thread::sleep(Duration::from_millis(10));
            }
        });
        struct Stop<'a>(&'a AtomicBool);
        impl Drop for Stop<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
        let _stop = Stop(&stop);
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut observed_progress = false;
        loop {
            let stats = receiver.connection_stats();
            let received = stats.audio_frames_received();
            let dropped = stats.audio_frames_dropped();
            if received > 0 {
                observed_progress = true;
            }
            if received >= dropped && dropped > received / 2 && dropped > 0 {
                // No raw capture or FrameSync consumes this queue. The public
                // total includes frames subsequently dropped from it.
                assert!(stats.audio_frames_queued() > 0, "{stats:?}");
                let expected = 100.0 * (dropped as f64 / received as f64);
                assert!((stats.audio_drop_percentage() - expected).abs() < 1e-10);
                assert!(stats.audio_drop_percentage() > 50.0);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "queue did not overflow after progress={observed_progress}: {stats:?}"
            );
            thread::sleep(Duration::from_millis(50));
        }
    });
}
