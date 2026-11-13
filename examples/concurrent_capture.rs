//! Example: capturing video, audio and metadata from one receiver on three threads.
//!
//! Connects to the first source found and shares the receiver between a
//! video, an audio and a metadata thread, each of which captures a few frames
//! and reports what it received.
//!
//! Run with: `cargo run --example concurrent_capture`
//!
//! Optional arguments:
//! - IP addresses to search: `cargo run --example concurrent_capture -- 192.168.0.100`

use grafton_ndi::{Ndi, Receiver, ReceiverBandwidth, ReceiverColorFormat, ReceiverOptions};

use std::{env, thread, time::Duration};

#[path = "common/mod.rs"]
mod common;

fn main() -> Result<(), grafton_ndi::Error> {
    // Parse command line arguments for extra IPs
    let args: Vec<String> = env::args().collect();
    let extra_ips: Vec<&str> = args[1..].iter().map(|s| s.as_str()).collect();

    // Initialize NDI
    let ndi = Ndi::new()?;
    println!("NDI version: {}", ndi.info().raw_version());

    // Create a finder to discover sources
    let finder = common::finder_with_extra_ips(&ndi, &extra_ips)?;

    // Discover sources across the full 5s window so late responders on
    // staggered/unicast networks are not missed.
    println!("Looking for NDI sources...");
    let sources = finder.find_sources(Duration::from_secs(5))?;
    if sources.is_empty() {
        println!("No NDI sources found on the network");
        return Ok(());
    }

    let sources_len = sources.len();
    println!("Found {sources_len} sources:");
    for (i, source) in sources.iter().enumerate() {
        println!("  [{i}] {source}");
    }

    // Connect to the first source
    let source = sources[0].clone();
    println!("\nConnecting to: {source}");

    // Create receiver
    let options = ReceiverOptions::new(source)
        .color_format(ReceiverColorFormat::BgrxBgra)
        .bandwidth(ReceiverBandwidth::Highest)
        .allow_video_fields(true)
        .name("Concurrent Capture Example");
    let receiver = Receiver::new(&ndi, &options)?;

    // Use scoped threads to ensure proper lifetimes
    thread::scope(|s| -> grafton_ndi::Result<()> {
        // Spawn video capture thread
        let recv_video = &receiver;
        let video_handle = s.spawn(move || -> grafton_ndi::Result<()> {
            println!("Video thread started");
            let mut frame_count = 0;

            for _ in 0..10 {
                match recv_video.video().capture_ref(Duration::from_secs(5))? {
                    Some(frame) => {
                        frame_count += 1;
                        let width = frame.width();
                        let height = frame.height();
                        let frame_rate_n = frame.frame_rate().numerator();
                        let frame_rate_d = frame.frame_rate().denominator();
                        println!(
                            "[VIDEO] Frame {frame_count}: {width}x{height} @ {frame_rate_n}/{frame_rate_d} fps"
                        );
                    }
                    None => {
                        println!("[VIDEO] Timeout waiting for frame");
                    }
                }
            }

            println!("Video thread finished - captured {frame_count} frames");
            Ok(())
        });

        // Spawn audio capture thread
        let recv_audio = &receiver;
        let audio_handle = s.spawn(move || -> grafton_ndi::Result<()> {
            println!("Audio thread started");
            let mut sample_count = 0;

            for _ in 0..10 {
                match recv_audio.audio().capture_ref(Duration::from_secs(5))? {
                    Some(frame) => {
                        sample_count += frame.num_samples();
                        let num_samples = frame.num_samples();
                        let sample_rate = frame.sample_rate();
                        let num_channels = frame.num_channels();
                        println!(
                            "[AUDIO] {num_samples} samples @ {sample_rate} Hz, {num_channels} channels"
                        );
                    }
                    None => {
                        println!("[AUDIO] Timeout waiting for frame");
                    }
                }
            }

            println!("Audio thread finished - captured {sample_count} samples");
            Ok(())
        });

        // Spawn metadata capture thread
        let recv_metadata = &receiver;
        let metadata_handle = s.spawn(move || -> grafton_ndi::Result<()> {
            println!("Metadata thread started");
            let mut metadata_count = 0;

            for _ in 0..20 {
                match recv_metadata
                    .metadata()
                    .capture_ref(Duration::from_millis(2500))?
                {
                    Some(frame) => {
                        metadata_count += 1;
                        let data = frame.data();
                        let data_len = data.len();
                        let mut characters = data.chars();
                        let mut preview: String = characters.by_ref().take(50).collect();
                        if characters.next().is_some() {
                            preview.push_str("...");
                        }
                        println!("[METADATA] Received {data_len} bytes: {preview}");
                    }
                    None => {
                        // Metadata is less frequent, timeouts are normal
                    }
                }
            }

            println!("Metadata thread finished - captured {metadata_count} frames");
            Ok(())
        });

        // Wait for all threads to complete
        video_handle.join().expect("video worker panicked")?;
        audio_handle.join().expect("audio worker panicked")?;
        metadata_handle.join().expect("metadata worker panicked")?;
        Ok(())
    })?;

    println!("\nConcurrent capture workers finished.");
    Ok(())
}
