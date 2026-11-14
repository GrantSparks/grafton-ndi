//! Example: Receiving NDI audio with 32-bit float format.
//!
//! This example demonstrates:
//! - Finding an NDI source
//! - Setting up an audio receiver
//! - Capturing 32-bit float audio frames
//! - Accessing audio data by channel
//!
//! Run with: `cargo run --example NDIlib_Recv_Audio`
//!
//! Optional arguments:
//! - IP address to search: `cargo run --example NDIlib_Recv_Audio -- 192.168.0.110`

use std::{env, time::Duration};

use grafton_ndi::{Error, Ndi, Receiver, ReceiverBandwidth, ReceiverOptions};

#[path = "common/finder.rs"]
mod finder;

use finder::finder_with_extra_ips;

fn main() -> Result<(), Error> {
    // Parse command line arguments for extra IPs
    let args: Vec<String> = env::args().collect();
    let extra_ips: Vec<&str> = args[1..].iter().map(|s| s.as_str()).collect();

    println!("NDI Audio Receiver Example (32-bit float)");
    println!("=========================================\n");

    // Initialize the NDI runtime
    let ndi = Ndi::new()?;
    println!("NDI initialized successfully\n");

    // Configure the finder
    let finder = finder_with_extra_ips(&ndi, &extra_ips)?;

    // Wait for sources to appear
    println!("Searching for NDI sources...\n");
    let sources = finder.find_sources(Duration::from_secs(5))?;

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
    let options = ReceiverOptions::new(source)
        .bandwidth(ReceiverBandwidth::AudioOnly)
        .name("Audio Capture Example");
    let receiver = Receiver::new(&ndi, &options)?;

    println!("Receiver created successfully");
    println!("Waiting for audio frames...\n");

    // Capture a few audio frames
    for i in 0..5 {
        match receiver.audio().capture_ref(Duration::from_secs(5))? {
            Some(audio_frame) => {
                println!("Frame {}: ", i + 1);
                println!("  Sample rate: {} Hz", audio_frame.sample_rate());
                println!("  Channels: {}", audio_frame.num_channels());
                println!("  Samples: {}", audio_frame.num_samples());
                match audio_frame.timestamp() {
                    Some(timestamp) => println!("  Timestamp: {} ticks", timestamp.ticks()),
                    None => println!("  Timestamp: unknown"),
                }
                println!("  Format: {:?}", audio_frame.format());

                // Borrow channels directly; SDK channel padding is excluded.
                println!(
                    "  Total samples: {}",
                    audio_frame.channels().map(<[f32]>::len).sum::<usize>()
                );
                if let Some(channel) = audio_frame.channel_data(0) {
                    let samples = &channel[..channel.len().min(100)];
                    if !samples.is_empty() {
                        let sum_squares: f32 = samples.iter().map(|&x| x * x).sum();
                        let rms = (sum_squares / samples.len() as f32).sqrt();
                        println!(
                            "  RMS level (first channel, {} samples): {rms:.4}",
                            samples.len()
                        );
                    }
                }

                // Show per-channel data if stereo or more
                if audio_frame.num_channels() > 1 {
                    for ch in 0..audio_frame.num_channels().min(2) as usize {
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
    }

    println!("Audio capture complete!");

    Ok(())
}
