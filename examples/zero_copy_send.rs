use grafton_ndi::{FourCCVideoType, SendOptions, VideoFrameBorrowed, NDI};
use std::time::{Duration, Instant};

fn main() -> Result<(), grafton_ndi::Error> {
    // Initialize NDI
    let ndi = NDI::new()?;
    println!("NDI version: {}", NDI::version()?);

    // Create sender
    let send_options = SendOptions::builder("Zero-Copy Sender Example")
        .clock_video(true)
        .clock_audio(true)
        .build()?;
    let send = grafton_ndi::SendInstance::new(&ndi, send_options)?;

    println!("Created NDI sender: {}", send.get_source_name());

    // Pre-allocate buffers for double-buffering
    let width = 1920;
    let height = 1080;
    let buffer_size = (width * height * 4) as usize; // BGRA format

    let mut buffer1 = vec![0u8; buffer_size];
    let mut buffer2 = vec![0u8; buffer_size];
    let mut current_buffer = 0;

    // Frame timing
    let frame_rate = 60.0;
    let frame_duration = Duration::from_secs_f64(1.0 / frame_rate);
    let mut next_frame_time = Instant::now();

    println!("Sending video at {}x{} @ {} fps", width, height, frame_rate);
    println!("Press Ctrl+C to stop...");

    let mut frame_count = 0;
    let start_time = Instant::now();

    loop {
        // Get the current buffer to work with
        let buffer = if current_buffer == 0 {
            &mut buffer1
        } else {
            &mut buffer2
        };

        // Simulate frame generation (in real app, this would be encoding/rendering)
        generate_test_pattern(buffer, width, height, frame_count);

        // Create a borrowed frame that references our buffer
        let borrowed_frame =
            VideoFrameBorrowed::from_buffer(buffer, width, height, FourCCVideoType::BGRA, 60, 1);

        // Send asynchronously - no copy happens here!
        let token = send.send_video_async(borrowed_frame);

        // While NDI is using our buffer, we can switch to the other buffer
        current_buffer = 1 - current_buffer;

        // Simulate some work that can happen while NDI is sending
        std::thread::sleep(Duration::from_millis(5));

        // Token is dropped here, indicating we're done with the previous buffer
        drop(token);

        frame_count += 1;

        // Print statistics every 60 frames
        if frame_count % 60 == 0 {
            let elapsed = start_time.elapsed();
            let actual_fps = frame_count as f64 / elapsed.as_secs_f64();
            println!(
                "Sent {} frames in {:.1}s - {:.1} fps (target: {} fps)",
                frame_count,
                elapsed.as_secs_f64(),
                actual_fps,
                frame_rate
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

    println!("\nFinished sending {} frames", frame_count);
    Ok(())
}

// Generate a simple test pattern
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
