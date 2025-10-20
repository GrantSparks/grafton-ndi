//! Example: Receiving NDI audio and converting to 16-bit format.
//!
//! This example demonstrates capturing audio from an NDI source and
//! converting it to 16-bit signed integer format, similar to the C++ example.
//!
//! Run with: `cargo run --example NDIlib_Recv_Audio_16bpp`
//!
//! Optional arguments:
//! - IP address to search: `cargo run --example NDIlib_Recv_Audio_16bpp -- 192.168.0.110`

use grafton_ndi::{Error, Finder, FinderOptions, Receiver, ReceiverOptions, NDI};

use std::{
    env,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

fn main() -> Result<(), Error> {
    // Parse command line arguments for extra IPs
    let args: Vec<String> = env::args().collect();
    let extra_ips: Vec<&str> = args[1..].iter().map(|s| s.as_str()).collect();

    // Set up signal handler for graceful shutdown
    let exit_loop = Arc::new(AtomicBool::new(false));
    let exit_loop_clone = exit_loop.clone();
    ctrlc::set_handler(move || {
        exit_loop_clone.store(true, Ordering::Relaxed);
    })
    .expect("Error setting Ctrl-C handler");

    let ndi = NDI::new()?;

    let mut builder = FinderOptions::builder();

    // Add any command line IPs
    if !extra_ips.is_empty() {
        println!("Searching additional IPs/subnets:");
        for ip in &extra_ips {
            println!("  - {}", ip);
            builder = builder.extra_ips(*ip);
        }
        println!();
    }

    let finder_options = builder.build();
    let finder = Finder::new(&ndi, &finder_options)?;

    // Wait until there is at least one source
    let sources = loop {
        if exit_loop.load(Ordering::Relaxed) {
            return Ok(());
        }
        finder.wait_for_sources(Duration::from_secs(1))?;
        let sources = finder.sources(Duration::ZERO)?;
        if !sources.is_empty() {
            break sources;
        }
    };

    // Create a receiver for the first source
    let options = ReceiverOptions::builder(sources[0].clone())
        .name("Example Audio Converter Receiver")
        .build();
    let receiver = Receiver::new(&ndi, &options)?;

    // Run for one minute
    let start = Instant::now();
    while !exit_loop.load(Ordering::Relaxed) && start.elapsed() < Duration::from_secs(60) {
        if let Some(video_frame) = receiver.capture_video_timeout(Duration::ZERO)? {
            println!(
                "Video data received ({width}x{height}).",
                width = video_frame.width,
                height = video_frame.height
            );
        }

        if let Some(audio_frame) = receiver.capture_audio_timeout(Duration::ZERO)? {
            println!(
                "Audio data received ({num_samples} samples).",
                num_samples = audio_frame.num_samples
            );

            let audio_16bit = convert_to_16bit_interleaved(&audio_frame, 20); // 20dB headroom

            // Here you would process the 16-bit audio data
            println!(
                "  Converted to 16-bit: {samples} samples",
                samples = audio_16bit.len() / audio_frame.num_channels as usize
            );
        }

        if let Some(_metadata) = receiver.capture_metadata_timeout(Duration::ZERO)? {
            println!("Meta data received.");
        }

        if let Some(_status) = receiver.poll_status_change(Duration::ZERO)? {
            println!("Receiver connection status changed.");
        }

        // Small delay to avoid busy-waiting
        std::thread::sleep(Duration::from_millis(10));
    }

    Ok(())
}

/// Convert audio frame from float to 16-bit signed integer format
///
/// # Arguments
/// * `audio_frame` - The input audio frame with float samples
/// * `reference_level_db` - The reference level in dB for scaling
fn convert_to_16bit_interleaved(
    audio_frame: &grafton_ndi::AudioFrame,
    reference_level_db: i32,
) -> Vec<i16> {
    let num_samples = (audio_frame.num_samples * audio_frame.num_channels) as usize;
    let mut output = vec![0i16; num_samples];

    // Calculate scaling factor based on reference level
    let scale = 10.0_f32.powf(-reference_level_db as f32 / 20.0) * 32767.0;

    let float_data = audio_frame.data();

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
