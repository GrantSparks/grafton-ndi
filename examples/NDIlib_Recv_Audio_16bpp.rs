//! Example: Receiving NDI audio and converting to 16-bit format.
//!
//! This example demonstrates capturing audio from an NDI source and
//! converting it to 16-bit signed integer format, similar to the C++ example.
//!
//! Run with: `cargo run --example NDIlib_Recv_Audio_16bpp`
//!
//! Optional arguments:
//! - IP address to search: `cargo run --example NDIlib_Recv_Audio_16bpp -- 192.168.0.110`

use grafton_ndi::{
    audio_conversion::{AudioConverter, AudioReferenceLevel},
    Error, Ndi, Receiver, ReceiverBandwidth, ReceiverOptions,
};

use std::{
    env,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

#[path = "common/mod.rs"]
mod common;

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

    let ndi = Ndi::new()?;
    let converter = AudioConverter::new(&ndi)?;
    let mut audio_16bit = Vec::new();

    let finder = common::finder_with_extra_ips(&ndi, &extra_ips)?;

    // Wait until there is at least one source (or Ctrl-C)
    let Some(sources) =
        common::wait_for_first_source(&finder, || exit_loop.load(Ordering::Relaxed))?
    else {
        return Ok(());
    };

    // Create a receiver for the first source
    let options = ReceiverOptions::new(sources[0].clone())
        .bandwidth(ReceiverBandwidth::AudioOnly)
        .name("Example Audio Converter Receiver");
    let receiver = Receiver::new(&ndi, &options)?;

    // Run for one minute
    let start = Instant::now();
    while !exit_loop.load(Ordering::Relaxed) && start.elapsed() < Duration::from_secs(60) {
        if let Some(audio_frame) = receiver.audio().capture_ref(Duration::from_millis(250))? {
            println!(
                "Audio data received ({num_samples} samples).",
                num_samples = audio_frame.num_samples()
            );

            // Convert the borrowed capture directly, including a padded channel stride.
            // Use the SDK's current v3 converter with explicit receive headroom.
            // Keep the output allocation across captures, resizing if the source
            // changes its channel count or number of samples.
            audio_16bit.resize(
                audio_frame.num_channels() as usize * audio_frame.num_samples() as usize,
                0,
            );
            let written = converter.to_interleaved_i16(
                &audio_frame,
                &mut audio_16bit,
                AudioReferenceLevel::RECEIVE_HEADROOM,
            )?;

            // Here you would process the 16-bit audio data
            println!(
                "  Converted to 16-bit: {samples} samples",
                samples = written / audio_frame.num_channels() as usize
            );
        }
    }

    Ok(())
}
