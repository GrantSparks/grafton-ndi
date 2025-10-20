//! Example demonstrating zero-copy async video sending with tokens

use grafton_ndi::{BorrowedVideoFrame, PixelFormat, SenderOptions, NDI};

use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

fn main() -> Result<(), grafton_ndi::Error> {
    // Initialize NDI
    let ndi = NDI::new()?;

    // Create sender (must be mutable for async sending)
    let send_options = SenderOptions::builder("AsyncExample")
        .clock_video(true)
        .clock_audio(true)
        .build();
    let mut sender = grafton_ndi::Sender::new(&ndi, &send_options)?;

    println!("Created NDI sender: AsyncExample");
    println!("Demonstrating zero-copy async video sending with tokens...\n");

    // Channel to track when video buffer is released
    let (tx, rx) = mpsc::channel();

    // Set up callback for video completion
    sender.on_async_video_done(move |len| {
        println!("Video buffer released: {len} bytes");
        let _ = tx.send(len);
    });

    println!("=== Video Async Send ===");
    {
        let mut video_buffer = vec![0u8; 1920 * 1080 * 4];
        // Fill with test pattern
        for (i, pixel) in video_buffer.chunks_mut(4).enumerate() {
            pixel[0] = (i % 256) as u8; // B
            pixel[1] = ((i / 256) % 256) as u8; // G
            pixel[2] = ((i / 65536) % 256) as u8; // R
            pixel[3] = 255; // A
        }

        let frame =
            BorrowedVideoFrame::from_buffer(&video_buffer, 1920, 1080, PixelFormat::BGRA, 30, 1);

        println!("Sending video frame...");
        let start = Instant::now();
        let _token = sender.send_video_async(&frame);
        println!("Send completed in {:?}", start.elapsed());

        // Buffer is now in use by NDI
        println!("Token held - buffer in use by NDI");
        thread::sleep(Duration::from_millis(100));

        // Wait for SDK callback to indicate buffer is released
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(_) => println!("SDK callback fired - buffer can now be reused\n"),
            Err(_) => println!("Warning: No callback received within timeout\n"),
        }
    }

    println!("=== Multiple Video Frames ===");
    {
        let mut buffers = vec![vec![0u8; 1920 * 1080 * 4]; 3];

        // Fill each buffer with different colors
        for (idx, buffer) in buffers.iter_mut().enumerate() {
            let color = match idx {
                0 => [255, 0, 0, 255], // Red
                1 => [0, 255, 0, 255], // Green
                2 => [0, 0, 255, 255], // Blue
                _ => [0, 0, 0, 255],
            };

            for pixel in buffer.chunks_mut(4) {
                pixel.copy_from_slice(&color);
            }
        }

        println!("Sending 3 video frames sequentially...");
        for (idx, buffer) in buffers.iter().enumerate() {
            let frame =
                BorrowedVideoFrame::from_buffer(buffer, 1920, 1080, PixelFormat::BGRA, 30, 1);

            let frame_num = idx + 1;
            println!("Sending frame {frame_num}...");
            let _token = sender.send_video_async(&frame);

            // Simulate some processing time
            thread::sleep(Duration::from_millis(33)); // ~30fps

            // SDK will notify via callback when buffer is released
        }
        println!("All frames sent\n");
    }

    // Note: Audio and metadata sending is always synchronous in NDI SDK
    println!("Note: NDI SDK only supports async sending for video frames.");
    println!("Audio and metadata are always sent synchronously (with immediate copy).");

    println!("\nExample completed successfully!");
    Ok(())
}
