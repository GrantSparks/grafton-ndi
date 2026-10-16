//! Example: Sending video frames via NDI.
//!
//! This example sends black 1920x1080 frames at the builder's default 60 fps.
//! The NDI SDK clocks submission, and each batch reports its measured throughput.
//!
//! Run with: `cargo run --example NDIlib_Send_Video`

use std::time::Instant;

use grafton_ndi::{Error, PixelFormat, Sender, SenderOptions, VideoFrame, NDI};

fn main() -> Result<(), Error> {
    // Initialize NDI
    let ndi = NDI::new()?;

    // Create the NDI sender
    let send_options = SenderOptions::builder("My Video").build();
    let sender = Sender::new(&ndi, &send_options)?;

    // Build 1920x1080 frames at the default 60 fps.
    let xres = 1920i32;
    let yres = 1080i32;

    // Send full batches until at least 10 seconds have elapsed.
    let start = Instant::now();
    while start.elapsed().as_secs() < 10 {
        let batch_start = Instant::now();

        // Send 200 frames
        for _idx in 0..200 {
            // Create video frame with test pattern
            let video_frame = VideoFrame::builder()
                .resolution(xres, yres)
                .pixel_format(PixelFormat::BGRX)
                .build()?;

            // Zero-initialized BGRX pixels produce a black frame.

            // Send the frame (SDK handles timing)
            sender.send_video(&video_frame);
        }

        // Display FPS
        let elapsed = batch_start.elapsed().as_secs_f32();
        println!("200 frames sent, at {:.2}fps", 200.0 / elapsed);
    }

    Ok(())
}
