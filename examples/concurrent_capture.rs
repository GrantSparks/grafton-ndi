use std::sync::Arc;
use std::thread;
use std::time::Duration;

use grafton_ndi::{ReceiverBandwidth, ReceiverColorFormat, ReceiverOptions, NDI};

fn main() -> Result<(), grafton_ndi::Error> {
    // Initialize NDI
    let ndi = NDI::new()?;
    println!("NDI version: {}", NDI::version()?);

    // Create a finder to discover sources
    let finder_options = grafton_ndi::FinderOptions::default();
    let finder = grafton_ndi::Finder::new(&ndi, &finder_options)?;

    // Wait for sources
    println!("Looking for NDI sources...");
    if !finder.wait_for_sources(5000) {
        println!("No sources found after 5 seconds");
        return Ok(());
    }

    // Get available sources
    let sources = finder.get_sources(0)?;
    if sources.is_empty() {
        println!("No NDI sources found on the network");
        return Ok(());
    }

    println!("Found {} sources:", sources.len());
    for (i, source) in sources.iter().enumerate() {
        println!("  [{}] {}", i, source);
    }

    // Connect to the first source
    let source = sources[0].clone();
    println!("\nConnecting to: {}", source);

    // Create receiver
    let receiver = ReceiverOptions::builder(source)
        .color(ReceiverColorFormat::BGRX_BGRA)
        .bandwidth(ReceiverBandwidth::Highest)
        .allow_video_fields(true)
        .name("Concurrent Capture Example")
        .build(&ndi)?;

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
                match recv_video.capture_video(5000) {
                    Ok(Some(frame)) => {
                        frame_count += 1;
                        println!(
                            "[VIDEO] Frame {}: {}x{} @ {}/{} fps",
                            frame_count,
                            frame.width,
                            frame.height,
                            frame.frame_rate_n,
                            frame.frame_rate_d
                        );
                    }
                    Ok(None) => {
                        println!("[VIDEO] Timeout waiting for frame");
                    }
                    Err(e) => {
                        eprintln!("[VIDEO] Error capturing frame: {}", e);
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(100));
            }

            println!("Video thread finished - captured {} frames", frame_count);
        });

        // Spawn audio capture thread
        let recv_audio = Arc::clone(&receiver);
        let audio_handle = s.spawn(move || {
            println!("Audio thread started");
            let mut sample_count = 0;

            for _ in 0..10 {
                match recv_audio.capture_audio(5000) {
                    Ok(Some(frame)) => {
                        sample_count += frame.num_samples;
                        println!(
                            "[AUDIO] {} samples @ {} Hz, {} channels",
                            frame.num_samples, frame.sample_rate, frame.num_channels
                        );
                    }
                    Ok(None) => {
                        println!("[AUDIO] Timeout waiting for frame");
                    }
                    Err(e) => {
                        eprintln!("[AUDIO] Error capturing frame: {}", e);
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(100));
            }

            println!("Audio thread finished - captured {} samples", sample_count);
        });

        // Spawn metadata capture thread
        let recv_metadata = Arc::clone(&receiver);
        let metadata_handle = s.spawn(move || {
            println!("Metadata thread started");
            let mut metadata_count = 0;

            for _ in 0..20 {
                match recv_metadata.capture_metadata(2500) {
                    Ok(Some(frame)) => {
                        metadata_count += 1;
                        println!(
                            "[METADATA] Received {} bytes: {}",
                            frame.data.len(),
                            if frame.data.len() > 50 {
                                format!("{}...", &frame.data[..50])
                            } else {
                                frame.data.clone()
                            }
                        );
                    }
                    Ok(None) => {
                        // Metadata is less frequent, timeouts are normal
                    }
                    Err(e) => {
                        eprintln!("[METADATA] Error capturing frame: {}", e);
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(50));
            }

            println!(
                "Metadata thread finished - captured {} frames",
                metadata_count
            );
        });

        // Wait for all threads to complete
        video_handle.join().unwrap();
        audio_handle.join().unwrap();
        metadata_handle.join().unwrap();
    });

    println!("\nConcurrent capture example completed successfully!");
    Ok(())
}
