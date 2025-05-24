//! Example: Receiving NDI audio with 32-bit float format.
//!
//! This example demonstrates:
//! - Finding an NDI source
//! - Setting up an audio receiver
//! - Capturing 32-bit float audio frames
//! - Accessing audio data by channel
//!
//! Run with: `cargo run --example NDIlib_Recv_Audio`

use grafton_ndi::{Error, Finder, FinderOptions, ReceiverOptions, ReceiverBandwidth, NDI};
use std::thread;
use std::time::Duration;

fn main() -> Result<(), Error> {
    println!("NDI Audio Receiver Example (32-bit float)");
    println!("=========================================\n");

    // Initialize the NDI runtime
    let ndi = NDI::new()?;
    println!("NDI initialized successfully\n");

    // Configure the finder
    let finder_options = FinderOptions::builder().show_local_sources(true).build();

    let finder = Finder::new(&ndi, &finder_options)?;

    // Wait for sources to appear
    println!("Searching for NDI sources...\n");
    finder.wait_for_sources(5000);
    let sources = finder.get_sources(5000)?;

    if sources.is_empty() {
        println!("No NDI sources found!");
        return Ok(());
    }

    // Display available sources
    println!("Available sources:");
    for (i, source) in sources.iter().enumerate() {
        println!("  {}. {}", i + 1, source);
    }
    println!();

    // Use the first source
    let source = sources[0].clone();
    println!("Connecting to: {}\n", source);

    // Create a receiver for audio
    let receiver = ReceiverOptions::builder(source)
        .bandwidth(ReceiverBandwidth::AudioOnly)
        .name("Audio Capture Example")
        .build(&ndi)?;

    println!("Receiver created successfully");
    println!("Waiting for audio frames...\n");

    // Capture a few audio frames
    for i in 0..5 {
        match receiver.capture_audio(5000)? {
            Some(audio_frame) => {
                println!("Frame {}: ", i + 1);
                println!("  Sample rate: {} Hz", audio_frame.sample_rate);
                println!("  Channels: {}", audio_frame.num_channels);
                println!("  Samples: {}", audio_frame.num_samples);
                println!("  Timestamp: {}", audio_frame.timestamp);
                println!("  Format: {:?}", audio_frame.fourcc);

                // Get the audio data as f32
                let audio_data = audio_frame.data();
                println!("  Total samples: {}", audio_data.len());

                // Calculate RMS level for first 100 samples
                let sample_count = audio_data.len().min(100);
                if sample_count > 0 {
                    let sum_squares: f32 = audio_data[..sample_count].iter().map(|&x| x * x).sum();
                    let rms = (sum_squares / sample_count as f32).sqrt();
                    println!("  RMS level (first {} samples): {:.4}", sample_count, rms);
                }

                // Show per-channel data if stereo or more
                if audio_frame.num_channels > 1 {
                    for ch in 0..audio_frame.num_channels.min(2) as usize {
                        if let Some(channel_data) = audio_frame.channel_data(ch) {
                            let ch_sample_count = channel_data.len().min(10);
                            print!("  Channel {}: [", ch);
                            for (idx, &sample) in channel_data[..ch_sample_count].iter().enumerate()
                            {
                                if idx > 0 {
                                    print!(", ");
                                }
                                print!("{:.3}", sample);
                            }
                            if channel_data.len() > ch_sample_count {
                                print!(", ...");
                            }
                            println!("]");
                        }
                    }
                }

                println!();
            }
            None => {
                println!("No audio frame available");
            }
        }

        // Small delay between captures
        thread::sleep(Duration::from_millis(100));
    }

    println!("Audio capture complete!");

    Ok(())
}
