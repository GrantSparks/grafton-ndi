use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

use grafton_ndi::{BorrowedVideoFrame, FourCCVideoType, SenderOptions, NDI};

fn main() -> Result<(), grafton_ndi::Error> {
    // Initialize NDI
    let ndi = NDI::new()?;
    let version = NDI::version()?;
    println!("NDI version: {version}");

    // Create sender (must be mutable for async sending)
    let send_options = SenderOptions::builder("Zero-Copy Sender Example")
        .clock_video(true)
        .clock_audio(true)
        .build()?;
    let mut sender = grafton_ndi::Sender::new(&ndi, &send_options)?;

    let source_name = sender.get_source_name()?;
    println!("Created NDI sender: {source_name}");

    // Pre-allocate a single buffer (demonstrating single-buffer with callback)
    let width = 1920;
    let height = 1080;
    let buffer_size = (width * height * 4) as usize; // BGRA format

    let mut buffer = vec![0u8; buffer_size];

    // Channel to signal when buffer is available again
    let (tx, rx) = mpsc::channel();

    // Register completion callback
    sender.on_async_video_done(move |_len| {
        // Buffer is now available for reuse
        let _ = tx.send(());
    });

    // Frame timing
    let frame_rate = 60.0;
    let frame_duration = Duration::from_secs_f64(1.0 / frame_rate);
    let mut next_frame_time = Instant::now();

    println!("Sending video at {width}x{height} @ {frame_rate} fps");
    println!("Press Ctrl+C to stop...");

    let mut frame_count = 0;
    let start_time = Instant::now();
    let mut buffer_available = true;

    loop {
        // Wait for buffer to be available if needed
        if !buffer_available {
            if rx.recv_timeout(Duration::from_millis(100)).is_ok() {
                #[allow(unused_assignments)]
                {
                    buffer_available = true;
                }
            } else {
                println!("Warning: Buffer not released in time");
                continue;
            }
        }

        // Generate frame data
        generate_test_pattern(&mut buffer, width, height, frame_count);

        // Create a borrowed frame that references our buffer
        let borrowed_frame =
            BorrowedVideoFrame::from_buffer(&buffer, width, height, FourCCVideoType::BGRA, 60, 1);

        // Send asynchronously - no copy happens here!
        let _token = sender.send_video_async(&borrowed_frame);
        buffer_available = false;

        // The buffer is now owned by NDI until the callback fires

        frame_count += 1;

        // Print statistics every 60 frames
        if frame_count % 60 == 0 {
            let elapsed = start_time.elapsed();
            let actual_fps = frame_count as f64 / elapsed.as_secs_f64();
            let elapsed_secs = elapsed.as_secs_f64();
            println!(
                "Sent {frame_count} frames in {elapsed_secs:.1}s - {actual_fps:.1} fps (target: {frame_rate} fps)"
            );
        }

        // Wait for next frame time
        let now = Instant::now();
        if now < next_frame_time {
            std::thread::sleep(next_frame_time - now);
        }
        next_frame_time += frame_duration;

        // Stop after 300 frames (5 seconds at 60fps)
        if frame_count >= 300 {
            break;
        }
    }

    println!("\nFinished sending {frame_count} frames");

    // The sender will now automatically wait for all async operations to complete
    // when it's dropped, so no manual sleep is needed.
    //
    // Alternatively, you can explicitly wait with:
    // send.flush_async(Duration::from_secs(1))?;

    Ok(())
}

/// Generate a simple test pattern with moving gradients
///
/// # Arguments
/// * `buffer` - The buffer to fill with BGRA pixel data
/// * `width` - Frame width in pixels
/// * `height` - Frame height in pixels
/// * `frame_num` - Current frame number for animation
fn generate_test_pattern(buffer: &mut [u8], width: i32, height: i32, frame_num: u32) {
    let width = width as usize;
    let height = height as usize;

    for y in 0..height {
        for x in 0..width {
            let offset = (y * width + x) * 4;

            // Create a moving gradient pattern
            let r = ((x + frame_num as usize) % 256) as u8;
            let g = ((y + frame_num as usize) % 256) as u8;
            let b = ((x + y + frame_num as usize) % 256) as u8;

            buffer[offset] = b; // B
            buffer[offset + 1] = g; // G
            buffer[offset + 2] = r; // R
            buffer[offset + 3] = 255; // A
        }
    }
}
