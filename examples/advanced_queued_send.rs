//! Example: sending video without waiting for the previous frame.
//!
//! This example demonstrates:
//! - Queuing frames with a [`QueuedSender`]
//! - Reusing each frame once the SDK has finished sending it
//! - Getting a frame back when the queue is full
//!
//! Needs the NDI Advanced SDK at build time and an Advanced runtime.
//!
//! Run with: `cargo run --features advanced --example advanced_queued_send`

use std::{thread, time::Duration};

use grafton_ndi::{
    advanced::{QueueOptions, QueuedSender, QueuedVideoFrame},
    Error, Ndi, SenderOptions, VideoFrame,
};

fn main() -> Result<(), Error> {
    let ndi = Ndi::new()?;
    let mut sender = QueuedSender::new(
        &ndi,
        &SenderOptions::new("Queued Sender Example"),
        QueueOptions::default().max_frames(4),
    )?;

    // Frames the SDK has finished with, ready to be filled and sent again.
    let mut spare: Vec<QueuedVideoFrame> = Vec::new();
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
            // Any other rejection would not succeed by waiting.
            Err(rejected) => return Err(rejected.into_error()),
        }
        thread::sleep(Duration::from_millis(16));
    }
    println!("{} frames still queued at exit", sender.retained_frames());
    Ok(())
}
