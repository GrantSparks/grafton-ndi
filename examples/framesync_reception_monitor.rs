//! Monitor recent audio reception while FrameSync supplies fixed mono output.
//!
//! Run with `cargo run --example framesync_reception_monitor -- 'HOST (Audio)' 1500`.
//! The second argument is the application's positive grace period in ms;
//! choose it from packet cadence, jitter and startup/recovery needs.
#[path = "common/reception_monitor.rs"]
mod reception_monitor;

use grafton_ndi::{
    Error, FrameSync, FrameSyncAudioRequest, Ndi, Receiver, ReceiverBandwidth, ReceiverOptions,
    Source,
};
use reception_monitor::ReceptionMonitor;
use std::{
    env,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let source = args
        .next()
        .ok_or("provide a source name and grace period in ms")?;
    let grace_ms: u64 = args
        .next()
        .ok_or("provide a positive grace period in ms")?
        .parse()?;
    if grace_ms == 0 || args.next().is_some() {
        return Err("provide exactly a source name and positive grace period in ms".into());
    }
    let ndi = Ndi::new()?;
    let receiver = Receiver::new(
        &ndi,
        &ReceiverOptions::new(Source::new(source)).bandwidth(ReceiverBandwidth::AudioOnly),
    )?;
    let framesync = FrameSync::new(receiver).map_err(Error::from)?;
    let request = FrameSyncAudioRequest::capture_at(48_000, 1, 480)?;
    let stopped = AtomicBool::new(false);

    thread::scope(|scope| {
        scope.spawn(|| {
            let mut monitor = ReceptionMonitor::new(Duration::from_millis(grace_ms));
            let mut previous = None;
            while !stopped.load(Ordering::Relaxed) {
                if framesync
                    .receiver()
                    .poll_source_name_change(Duration::ZERO)
                    .unwrap()
                    .is_some()
                {
                    monitor.reset();
                }
                let stats = framesync.receiver().connection_stats();
                let state = monitor.observe(
                    stats.is_connected(),
                    stats.audio_frames_received(),
                    Instant::now(),
                );
                if previous != Some(state) {
                    println!("audio reception: {state:?}");
                    previous = Some(state);
                }
                // Drops and expected throughput are separate quality checks.
                thread::sleep(Duration::from_millis(100));
            }
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let tick = Instant::now();
            // Fixed mono output remains usable even when no input arrives.
            let audio = framesync.capture_audio_ref(request).unwrap();
            assert_eq!(
                (
                    audio.sample_rate(),
                    audio.num_channels(),
                    audio.num_samples()
                ),
                (48_000, 1, 480)
            );
            thread::sleep(Duration::from_millis(10).saturating_sub(tick.elapsed()));
        }
        stopped.store(true, Ordering::Relaxed);
    });
    Ok(())
}
