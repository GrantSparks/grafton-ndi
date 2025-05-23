//! Example demonstrating zero-copy async sending with tokens

use grafton_ndi::{
    AudioFrameBorrowed, FourCCVideoType, MetadataFrameBorrowed, SendOptions, VideoFrameBorrowed,
    NDI,
};
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

fn main() -> Result<(), grafton_ndi::Error> {
    // Initialize NDI
    let ndi = NDI::new()?;

    // Create sender
    let send_options = SendOptions::builder("AsyncExample")
        .clock_video(true)
        .clock_audio(true)
        .build()?;
    let send = grafton_ndi::SendInstance::new(&ndi, &send_options)?;

    println!("Created NDI sender: AsyncExample");
    println!("Demonstrating zero-copy async sending with tokens...\n");

    // Track when buffers are released
    let video_released = Arc::new(AtomicBool::new(false));
    let audio_released = Arc::new(AtomicBool::new(false));
    let metadata_released = Arc::new(AtomicBool::new(false));

    // Set up callbacks
    let video_released_clone = video_released.clone();
    send.on_async_video_done(move |buf| {
        println!("Video buffer released: {} bytes", buf.len());
        video_released_clone.store(true, Ordering::Release);
    });

    let audio_released_clone = audio_released.clone();
    send.on_async_audio_done(move |buf| {
        println!("Audio buffer released: {} bytes", buf.len());
        audio_released_clone.store(true, Ordering::Release);
    });

    let metadata_released_clone = metadata_released.clone();
    send.on_async_metadata_done(move |buf| {
        println!("Metadata buffer released: {} bytes", buf.len());
        metadata_released_clone.store(true, Ordering::Release);
    });

    // Example 1: Video async send
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

        let frame = VideoFrameBorrowed::from_buffer(
            &video_buffer,
            1920,
            1080,
            FourCCVideoType::BGRA,
            30,
            1,
        );

        println!("Sending video frame...");
        let start = Instant::now();
        let _token = send.send_video_async(&frame);
        println!("Send completed in {:?}", start.elapsed());

        // Buffer is now in use by NDI
        println!("Token held - buffer cannot be modified");
        thread::sleep(Duration::from_millis(100));

        // Drop token to release buffer
        drop(_token);
        println!("Token dropped");

        // Wait for callback
        while !video_released.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(10));
        }
        println!("Buffer can now be reused\n");
    }

    // Example 2: Audio async send
    println!("=== Audio Async Send ===");
    {
        let sample_rate = 48000;
        let channels = 2;
        let duration_secs = 0.1;
        let samples = (sample_rate as f32 * duration_secs) as i32;
        let mut audio_buffer = vec![0u8; (samples * channels * 4) as usize];

        // Generate sine wave
        let frequency = 440.0; // A4
        let samples_f32 = unsafe {
            std::slice::from_raw_parts_mut(
                audio_buffer.as_mut_ptr() as *mut f32,
                (samples * channels) as usize,
            )
        };

        for i in 0..samples {
            let t = i as f32 / sample_rate as f32;
            let value = (2.0 * std::f32::consts::PI * frequency * t).sin() * 0.5;
            samples_f32[(i * channels) as usize] = value;
            samples_f32[(i * channels + 1) as usize] = value;
        }

        let frame = AudioFrameBorrowed::from_buffer(&audio_buffer, sample_rate, channels, samples);

        println!("Sending audio frame ({} samples)...", samples);
        let _token = send.send_audio_async(&frame);

        println!("Token held - buffer cannot be modified");
        thread::sleep(Duration::from_millis(50));

        drop(_token);
        println!("Token dropped");

        while !audio_released.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(10));
        }
        println!("Buffer can now be reused\n");
    }

    // Example 3: Metadata async send
    println!("=== Metadata Async Send ===");
    {
        let metadata = CString::new(
            r#"<ndi_metadata>
                <app_name>AsyncExample</app_name>
                <timestamp>12345</timestamp>
                <custom_data>Test metadata</custom_data>
            </ndi_metadata>"#,
        )?;

        let frame = MetadataFrameBorrowed::new(&metadata);

        println!("Sending metadata frame...");
        let _token = send.send_metadata_async(&frame);

        println!("Token held - metadata cannot be modified");
        thread::sleep(Duration::from_millis(50));

        drop(_token);
        println!("Token dropped");

        while !metadata_released.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(10));
        }
        println!("Buffer can now be reused\n");
    }

    // Example 4: Sequential sends of different types
    println!("=== Sequential Async Sends ===");
    {
        // Send video
        let buffer = vec![128u8; 1920 * 1080 * 4];
        let frame =
            VideoFrameBorrowed::from_buffer(&buffer, 1920, 1080, FourCCVideoType::BGRA, 60, 1);
        let _token = send.send_video_async(&frame);
        println!("Video sent");
        drop(_token);

        // Send audio
        let buffer = vec![0u8; 48000 * 2 * 4];
        let frame = AudioFrameBorrowed::from_buffer(&buffer, 48000, 2, 48000);
        let _token = send.send_audio_async(&frame);
        println!("Audio sent");
        drop(_token);

        // Send metadata
        let metadata = CString::new("<xml>sequential test</xml>")?;
        let frame = MetadataFrameBorrowed::new(&metadata);
        let _token = send.send_metadata_async(&frame);
        println!("Metadata sent");
        drop(_token);

        println!("All sequential sends completed\n");
    }

    println!("Example completed successfully!");
    Ok(())
}
