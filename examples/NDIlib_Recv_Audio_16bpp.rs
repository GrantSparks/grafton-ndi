//! Example: Receiving NDI audio and converting to 16-bit format.
//!
//! This example demonstrates capturing audio from an NDI source and
//! converting it to 16-bit signed integer format, similar to the C++ example.
//!
//! Run with: `cargo run --example NDIlib_Recv_Audio_16bpp`
//!
//! Optional arguments:
//! - IP address to search: `cargo run --example NDIlib_Recv_Audio_16bpp -- 192.168.0.110`

use grafton_ndi::{Error, Receiver, ReceiverOptions, NDI};

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

    let ndi = NDI::new()?;

    let finder = common::finder_with_extra_ips(&ndi, &extra_ips)?;

    // Wait until there is at least one source (or Ctrl-C)
    let Some(sources) =
        common::wait_for_first_source(&finder, || exit_loop.load(Ordering::Relaxed))?
    else {
        return Ok(());
    };

    // Create a receiver for the first source
    let options = ReceiverOptions::builder(sources[0].clone())
        .name("Example Audio Converter Receiver")
        .build();
    let receiver = Receiver::new(&ndi, &options)?;

    // Run for one minute
    let start = Instant::now();
    while !exit_loop.load(Ordering::Relaxed) && start.elapsed() < Duration::from_secs(60) {
        if let Some(video_frame) = receiver.video().try_capture(Duration::ZERO)? {
            println!(
                "Video data received ({width}x{height}).",
                width = video_frame.width(),
                height = video_frame.height()
            );
        }

        if let Some(audio_frame) = receiver.audio().try_capture(Duration::ZERO)? {
            println!(
                "Audio data received ({num_samples} samples).",
                num_samples = audio_frame.num_samples()
            );

            let audio_16bit = convert_to_16bit_interleaved(&audio_frame, 20); // 20dB headroom

            // Here you would process the 16-bit audio data
            println!(
                "  Converted to 16-bit: {samples} samples",
                samples = audio_16bit.len() / audio_frame.num_channels() as usize
            );
        }

        if let Some(_metadata) = receiver.metadata().try_capture(Duration::ZERO)? {
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
    let samples_per_channel = audio_frame.num_samples() as usize;
    let channels = audio_frame.num_channels() as usize;
    let mut output = Vec::with_capacity(audio_frame.data().len());

    // NDI samples are planar; PCM output orders channels within each sample.
    let scale = 10.0_f32.powf(-reference_level_db as f32 / 20.0) * 32767.0;
    for sample in 0..samples_per_channel {
        for channel in 0..channels {
            let value = audio_frame
                .channel_data(channel)
                .expect("validated channel")[sample];
            output.push((value * scale).clamp(-32768.0, 32767.0) as i16);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafton_ndi::AudioFrame;

    #[test]
    fn converts_planar_stereo_into_scaled_interleaved_pcm() {
        let frame = AudioFrame::builder()
            .channels(2)
            .samples(2)
            .data(vec![0.25, -0.25, 0.5, -0.5])
            .build()
            .unwrap();
        assert_eq!(
            convert_to_16bit_interleaved(&frame, 0),
            vec![8191, 16383, -8191, -16383]
        );
        assert_eq!(
            convert_to_16bit_interleaved(&frame, 20),
            vec![819, 1638, -819, -1638]
        );
    }

    #[test]
    fn clamps_pcm_to_signed_16_bit_limits() {
        let frame = AudioFrame::builder()
            .channels(1)
            .samples(2)
            .data(vec![-2.0, 2.0])
            .build()
            .unwrap();
        assert_eq!(convert_to_16bit_interleaved(&frame, 0), vec![-32768, 32767]);
    }
}
