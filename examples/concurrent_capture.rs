use grafton_ndi::{Receiver, ReceiverBandwidth, ReceiverColorFormat, ReceiverOptions, NDI};

use std::{env, sync::Arc, thread, time::Duration};

#[path = "common/mod.rs"]
mod common;

fn main() -> Result<(), grafton_ndi::Error> {
    // Parse command line arguments for extra IPs
    let args: Vec<String> = env::args().collect();
    let extra_ips: Vec<&str> = args[1..].iter().map(|s| s.as_str()).collect();

    // Initialize NDI
    let ndi = NDI::new()?;
    let version = NDI::version()?;
    println!("NDI version: {version}");

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
    let options = ReceiverOptions::builder(source)
        .color(ReceiverColorFormat::BGRX_BGRA)
        .bandwidth(ReceiverBandwidth::Highest)
        .allow_video_fields(true)
        .name("Concurrent Capture Example")
        .build();
    let receiver = Receiver::new(&ndi, &options)?;

    // Wrap receiver in Arc for sharing between threads
    let receiver = Arc::new(receiver);

    // Use scoped threads to ensure proper lifetimes
    thread::scope(|s| {
        // Spawn video capture thread
        let recv_video = Arc::clone(&receiver);
        let video_handle = s.spawn(move || {
            println!("Video thread started");
            let mut frame_count = 0;

            for _ in 0..10 {
                match recv_video.video().try_capture(Duration::from_secs(5)) {
                    Ok(Some(frame)) => {
                        frame_count += 1;
                        let width = frame.width();
                        let height = frame.height();
                        let frame_rate_n = frame.frame_rate_n();
                        let frame_rate_d = frame.frame_rate_d();
                        println!(
                            "[VIDEO] Frame {frame_count}: {width}x{height} @ {frame_rate_n}/{frame_rate_d} fps"
                        );
                    }
                    Ok(None) => {
                        println!("[VIDEO] Timeout waiting for frame");
                    }
                    Err(e) => {
                        eprintln!("[VIDEO] Error capturing frame: {e}");
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(100));
            }

            println!("Video thread finished - captured {frame_count} frames");
        });

        // Spawn audio capture thread
        let recv_audio = Arc::clone(&receiver);
        let audio_handle = s.spawn(move || {
            println!("Audio thread started");
            let mut sample_count = 0;

            for _ in 0..10 {
                match recv_audio.audio().try_capture(Duration::from_secs(5)) {
                    Ok(Some(frame)) => {
                        sample_count += frame.num_samples();
                        let num_samples = frame.num_samples();
                        let sample_rate = frame.sample_rate();
                        let num_channels = frame.num_channels();
                        println!(
                            "[AUDIO] {num_samples} samples @ {sample_rate} Hz, {num_channels} channels"
                        );
                    }
                    Ok(None) => {
                        println!("[AUDIO] Timeout waiting for frame");
                    }
                    Err(e) => {
                        eprintln!("[AUDIO] Error capturing frame: {e}");
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(100));
            }

            println!("Audio thread finished - captured {sample_count} samples");
        });

        // Spawn metadata capture thread
        let recv_metadata = Arc::clone(&receiver);
        let metadata_handle = s.spawn(move || {
            println!("Metadata thread started");
            let mut metadata_count = 0;

            for _ in 0..20 {
                match recv_metadata
                    .metadata()
                    .try_capture(Duration::from_millis(2500))
                {
                    Ok(Some(frame)) => {
                        metadata_count += 1;
                        let data = frame.data();
                        let data_len = data.len();
                        let preview = if data.len() > 50 {
                            let preview_data = &data[..50];
                            format!("{preview_data}...")
                        } else {
                            data.to_owned()
                        };
                        println!("[METADATA] Received {data_len} bytes: {preview}");
                    }
                    Ok(None) => {
                        // Metadata is less frequent, timeouts are normal
                    }
                    Err(e) => {
                        eprintln!("[METADATA] Error capturing frame: {e}");
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(50));
            }

            println!("Metadata thread finished - captured {metadata_count} frames");
        });

        // Wait for all threads to complete
        video_handle.join().unwrap();
        audio_handle.join().unwrap();
        metadata_handle.join().unwrap();
    });

    println!("\nConcurrent capture example completed successfully!");
    Ok(())
}
