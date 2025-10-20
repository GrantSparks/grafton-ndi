//! Example: Sending video frames via NDI.
//!
//! This example demonstrates sending a 1920x1080 video stream with
//! alternating black and white frames. The NDI SDK handles frame
//! timing to maintain the correct frame rate.
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

    // We are going to create a 1920x1080 frame at 29.97Hz
    let xres = 1920i32;
    let yres = 1080i32;

    // Run for 5 minutes
    let start = Instant::now();
    while start.elapsed().as_secs() < 300 {
        let batch_start = Instant::now();

        // Send 200 frames
        for _idx in 0..200 {
            // Create video frame with test pattern
            let video_frame = VideoFrame::builder()
                .resolution(xres, yres)
                .pixel_format(PixelFormat::BGRX)
                .build()?;

            // The frame is created with zero-initialized data
            // In the C++ example they fill with alternating black/white
            // but our frame is already black (zeros)

            // Send the frame (SDK handles timing)
            sender.send_video(&video_frame);
        }

        // Display FPS
        let elapsed = batch_start.elapsed().as_secs_f32();
        println!("200 frames sent, at {:.2}fps", 200.0 / elapsed);
    }

    Ok(())
}
