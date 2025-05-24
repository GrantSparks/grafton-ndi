//! Example: Sending audio via NDI.
//!
//! This example demonstrates sending 4-channel audio at 48kHz.
//! The audio is clocked to ensure accurate sample timing.
//!
//! Run with: `cargo run --example NDIlib_Send_Audio`

use grafton_ndi::{AudioFrame, Error, SendInstance, SendOptions, NDI};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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

    // Create an NDI source that is clocked to audio
    let send_options = SendOptions::builder("My Audio").clock_audio(true).build()?;

    let ndi_send = SendInstance::new(&ndi, &send_options)?;

    // Audio parameters
    let sample_rate = 48000;
    let no_channels = 4;
    let no_samples = 1920;

    // Create audio buffer (planar format)
    let mut audio_data = vec![0.0f32; (no_samples * no_channels) as usize];

    // Send 1000 frames
    for idx in 0..1000 {
        if exit_loop.load(Ordering::Relaxed) {
            break;
        }

        // Fill with silence (in real usage, you'd generate actual audio)
        audio_data.fill(0.0);

        // Create audio frame
        let audio_frame = AudioFrame::builder()
            .sample_rate(sample_rate)
            .channels(no_channels)
            .samples(no_samples)
            .data(audio_data.clone())
            .build()?;

        // Send the frame (clocked to 48kHz)
        ndi_send.send_audio(&audio_frame);

        // Display progress
        println!("Frame number {} sent.", idx);
    }

    Ok(())
}
