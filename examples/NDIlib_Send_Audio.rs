//! Example: Sending audio via NDI.
//!
//! This example sends 50 frames of 4-channel silence at 48kHz, 1920 samples
//! per frame, and then exits. The audio is clocked to ensure accurate sample
//! timing.
//!
//! Run with: `cargo run --example NDIlib_Send_Audio`

use grafton_ndi::{AudioFrame, Error, Ndi, Sender, SenderOptions};

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

fn main() -> Result<(), Error> {
    // Set up signal handler for graceful shutdown
    let exit_loop = Arc::new(AtomicBool::new(false));
    let exit_loop_clone = exit_loop.clone();
    ctrlc::set_handler(move || {
        exit_loop_clone.store(true, Ordering::Relaxed);
    })
    .expect("Error setting Ctrl-C handler");

    let ndi = Ndi::new()?;

    // Create an NDI source that is clocked to audio
    let send_options = SenderOptions::new("My Audio").clock_audio(true);

    let sender = Sender::new(&ndi, &send_options)?;

    // Audio parameters
    let sample_rate = 48000;
    let num_channels = 4;
    let num_samples = 1920;

    // Build the planar silence once. Synchronous sends borrow the samples only
    // for the call, so the same allocation can be reused for every frame.
    let audio_frame = AudioFrame::builder()
        .sample_rate(sample_rate)
        .num_channels(num_channels)
        .num_samples(num_samples)
        .build()?;

    for idx in 0..50 {
        if exit_loop.load(Ordering::Relaxed) {
            break;
        }

        // Send the frame (clocked to 48kHz)
        sender.send_audio(&audio_frame);

        println!("Frame number {idx} sent.");
    }

    Ok(())
}
