//! Example: Receiving NDI audio and converting to 16-bit format.
//!
//! This example demonstrates capturing audio from an NDI source and
//! converting it to 16-bit signed integer format, similar to the C++ example.
//!
//! Run with: `cargo run --example NDIlib_Recv_Audio_16bpp`

use grafton_ndi::{Error, Finder, FinderOptions, ReceiverOptions, NDI};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn main() -> Result<(), Error> {
    // Set up signal handler for graceful shutdown
    let exit_loop = Arc::new(AtomicBool::new(false));
    let exit_loop_clone = exit_loop.clone();
    ctrlc::set_handler(move || {
        exit_loop_clone.store(true, Ordering::Relaxed);
    })
    .expect("Error setting Ctrl-C handler");

    // Initialize NDI
    let ndi = NDI::new()?;

    // Create finder
    let finder_options = FinderOptions::builder().build();
    let finder = Finder::new(&ndi, &finder_options)?;

    // Wait until there is at least one source
    let sources = loop {
        if exit_loop.load(Ordering::Relaxed) {
            return Ok(());
        }
        finder.wait_for_sources(1000);
        let sources = finder.get_sources(0)?;
        if !sources.is_empty() {
            break sources;
        }
    };

    // Create a receiver for the first source
    let receiver = ReceiverOptions::builder(sources[0].clone())
        .name("Example Audio Converter Receiver")
        .build(&ndi)?;

    // Run for one minute
    let start = Instant::now();
    while !exit_loop.load(Ordering::Relaxed) && start.elapsed() < Duration::from_secs(60) {
        // Check for video frames
        if let Some(video_frame) = receiver.capture_video(0)? {
            println!(
                "Video data received ({}x{}).",
                video_frame.width, video_frame.height
            );
        }

        // Check for audio frames
        if let Some(audio_frame) = receiver.capture_audio(0)? {
            println!("Audio data received ({} samples).", audio_frame.num_samples);

            // Convert to 16-bit interleaved format
            let audio_16bit = convert_to_16bit_interleaved(&audio_frame, 20); // 20dB headroom

            // Here you would process the 16-bit audio data
            println!(
                "  Converted to 16-bit: {} samples",
                audio_16bit.len() / audio_frame.num_channels as usize
            );
        }

        // Check for metadata
        if let Some(_metadata) = receiver.capture_metadata(0)? {
            println!("Meta data received.");
        }

        // Check for status changes
        if let Some(_status) = receiver.poll_status_change(0) {
            println!("Receiver connection status changed.");
        }

        // Small delay to avoid busy-waiting
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    Ok(())
}

fn convert_to_16bit_interleaved(
    audio_frame: &grafton_ndi::AudioFrame,
    reference_level_db: i32,
) -> Vec<i16> {
    let num_samples = (audio_frame.num_samples * audio_frame.num_channels) as usize;
    let mut output = vec![0i16; num_samples];

    // Calculate scaling factor based on reference level
    let scale = 10.0_f32.powf(-reference_level_db as f32 / 20.0) * 32767.0;

    // Get the float audio data
    let float_data = audio_frame.data();

    // Convert and clip
    for (i, &sample) in float_data.iter().enumerate() {
        let scaled = sample * scale;
        output[i] = if scaled > 32767.0 {
            32767
        } else if scaled < -32768.0 {
            -32768
        } else {
            scaled as i16
        };
    }

    output
}
