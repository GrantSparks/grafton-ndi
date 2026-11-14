//! Example: sending video without waiting for the previous frame.
//!
//! This example demonstrates:
//! - Queuing frames with a [`QueuedSender`]
//! - Reusing each frame once the SDK has finished sending it
//! - Getting a frame back when the queue is full
//! - Sending audio on a separate scoped thread while video waits for completions
//!
//! Needs the NDI Advanced SDK at build time and an Advanced runtime.
//!
//! Run with: `cargo run --features advanced --example advanced_queued_send`

use std::{
    thread,
    time::{Duration, Instant},
};

use grafton_ndi::{
    advanced::{QueueOptions, QueuedSender, QueuedVideo, QueuedVideoFrame},
    AudioFrame, Error, Ndi, RuntimeOptions, SdkEdition, SenderOptions, VideoFrame,
};

fn main() -> Result<(), Error> {
    let ndi = Ndi::with_options(&RuntimeOptions::new().edition(SdkEdition::Advanced))?;
    let mut sender = QueuedSender::new(
        &ndi,
        // Completion-based sends are paced by the application below.
        &SenderOptions::new("Queued Sender Example").clock_video(false),
        QueueOptions::default().max_frames(4),
    )?;

    {
        let (mut video, audio) = sender.split();
        thread::scope(|scope| -> Result<(), Error> {
            let worker = scope.spawn(move || -> Result<(), Error> {
                let frame = AudioFrame::builder()
                    .sample_rate(48_000)
                    .num_channels(2)
                    .num_samples(480)
                    .build()?;
                // Audio's native clock paces these 10 ms blocks independently.
                for _ in 0..500 {
                    audio.send_audio(&frame);
                }
                Ok(())
            });
            let video_result = send_video(&mut video);
            worker.join().expect("audio worker panicked")?;
            video_result
        })?;
    }
    // Drop waits for native completion before freeing any retained frame.
    drop(sender);
    Ok(())
}

fn send_video(sender: &mut QueuedVideo<'_>) -> Result<(), Error> {
    // Frames the SDK has finished with, ready to be filled and sent again.
    let mut spare: Vec<QueuedVideoFrame> = Vec::new();
    let mut next_tick = Instant::now();
    let interval = Duration::from_secs_f64(1.0 / 60.0);
    for number in 0..300 {
        while let Some(done) = sender.try_take_completed() {
            spare.push(done.into_frame());
        }
        let frame = match spare.pop() {
            Some(frame) => frame,
            None => VideoFrame::builder().resolution(1920, 1080).build()?.into(),
        };
        match sender.try_submit(frame) {
            Ok(id) => println!("Frame {number} queued as submission {}", id.get()),
            Err(rejected) if matches!(rejected.error(), Error::QueueFull(_)) => {
                // The queue is full: the frame comes back, and is kept for later.
                println!("Frame {number} not queued: {rejected}");
                spare.push(rejected.into_inner());
                if let Some(done) = sender.wait_completed(Duration::from_millis(100))? {
                    spare.push(done.into_frame());
                }
            }
            // Other rejections, including a frame larger than max_bytes,
            // would not succeed by waiting.
            Err(rejected) => return Err(rejected.into_error()),
        }
        next_tick += interval;
        thread::sleep(next_tick.saturating_duration_since(Instant::now()));
        // Do not burst through missed ticks after queue backpressure.
        next_tick = next_tick.max(Instant::now());
    }
    println!("{} frames still queued at exit", sender.retained_frames());
    Ok(())
}
