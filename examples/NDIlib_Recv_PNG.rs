//! Example: Receiving NDI video and saving a frame as PNG.
//!
//! This example demonstrates receiving a video frame from the first
//! available NDI source and saving it as a PNG file.
//!
//! This is based on the NDIlib_Recv_PNG example from the NDI SDK.
//!
//! IMPORTANT: This example includes critical error handling that is
//! necessary for reliable NDI reception:
//!
//! 1. Retry loop for frame capture - NDI SDK's capture_video doesn't
//!    actually block for the full timeout duration, so we need to retry
//! 2. Stride validation - Prevents corrupted images when stride != width * 4
//! 3. Format verification - Ensures we actually get RGBA/RGBX format
//! 4. Compressed format detection - Warns about unsupported formats
//!
//! Run with: `cargo run --example NDIlib_Recv_PNG`
//!
//! Optional arguments:
//! - IP address to search: `cargo run --example NDIlib_Recv_PNG -- 192.168.0.100`
//! - Multiple IPs: `cargo run --example NDIlib_Recv_PNG -- 192.168.0.100 10.0.0.0/24`
//! - Custom output file: `cargo run --example NDIlib_Recv_PNG -- --output MyImage.png`
//! - Both: `cargo run --example NDIlib_Recv_PNG -- 192.168.0.100 --output MyImage.png`

use grafton_ndi::{
    Error, Finder, FinderOptions, FourCCVideoType, ReceiverColorFormat, ReceiverOptions, NDI,
};
use std::env;
use std::fs::File;
use std::time::{Duration, Instant};

fn main() -> Result<(), Error> {
    // Parse command line arguments
    let args: Vec<String> = env::args().collect();
    let mut extra_ips = Vec::new();
    let mut output_file = "CoolNDIImage.png";

    let mut i = 1;
    while i < args.len() {
        if args[i] == "--output" && i + 1 < args.len() {
            output_file = &args[i + 1];
            i += 2;
        } else if !args[i].starts_with("--") {
            extra_ips.push(args[i].as_str());
            i += 1;
        } else {
            eprintln!("Unknown argument: {}", args[i]);
            i += 1;
        }
    }
    println!("NDI Video Capture to PNG Example");
    println!("=================================\n");

    // Initialize NDI
    let ndi = NDI::new()?;
    println!("NDI initialized successfully");

    if output_file != "CoolNDIImage.png" {
        println!("Output file: {}", output_file);
    }
    println!();

    // Create a finder
    let mut builder = FinderOptions::builder().show_local_sources(true);

    // Add any command line IPs
    if !extra_ips.is_empty() {
        println!("Searching additional IPs/subnets:");
        for ip in &extra_ips {
            println!("  - {}", ip);
            builder = builder.extra_ips(*ip);
        }
        println!();
    }

    let finder = Finder::new(&ndi, &builder.build())?;

    // Wait until there is one source
    println!("Looking for sources ...");
    let sources = loop {
        finder.wait_for_sources(1000);
        let sources = finder.get_sources(0)?;
        if !sources.is_empty() {
            println!("Found {} source(s):", sources.len());
            for (i, source) in sources.iter().enumerate() {
                println!("  {}. {}", i + 1, source);
            }
            break sources;
        }
    };

    // Create a receiver for the first source
    println!("\nCreating receiver for: {}", sources[0]);
    let receiver = ReceiverOptions::builder(sources[0].clone())
        .color(ReceiverColorFormat::RGBX_RGBA)
        .build(&ndi)?;

    println!("Receiver created successfully");
    println!("Waiting for video frames...\n");

    // IMPORTANT: NDI SDK's capture_video doesn't block for the full timeout!
    // We need to implement our own retry loop with proper timing
    let start_time = Instant::now();
    let timeout = Duration::from_secs(60);
    let mut attempts = 0;

    let video_frame = loop {
        attempts += 1;

        // Check if we've exceeded our total timeout
        if start_time.elapsed() > timeout {
            return Err(Error::InitializationFailed(
                "Timeout waiting for video frame after 60 seconds".to_string(),
            ));
        }

        // Try to capture a frame with a short timeout (100ms)
        // The NDI SDK may return immediately even with a longer timeout
        match receiver.capture_video(100)? {
            Some(frame) => {
                println!("Frame received after {} attempts", attempts);

                // Debug information about the frame
                println!("Frame details:");
                println!("  Resolution: {}x{}", frame.width, frame.height);
                println!("  Format: {:?}", frame.fourcc);
                println!("  Line stride: {} bytes", unsafe {
                    frame.line_stride_or_size.line_stride_in_bytes
                });
                println!("  Data size: {} bytes", frame.data.len());
                println!(
                    "  Frame rate: {}/{}",
                    frame.frame_rate_n, frame.frame_rate_d
                );
                println!("  Timecode: {:016x}", frame.timecode);

                // Verify we got the format we requested
                match frame.fourcc {
                    FourCCVideoType::RGBA | FourCCVideoType::RGBX => {
                        println!("  ✓ Got requested RGBA/RGBX format");
                    }
                    _ => {
                        eprintln!(
                            "  ⚠ Warning: Got unexpected format {:?}, PNG may fail",
                            frame.fourcc
                        );
                    }
                }

                // CRITICAL: Verify stride matches width to prevent corrupted images
                let expected_stride = frame.width * 4; // 4 bytes per pixel for RGBA
                let actual_stride = unsafe { frame.line_stride_or_size.line_stride_in_bytes };

                if actual_stride != expected_stride {
                    // This is a common issue with some NDI sources
                    // If stride != width * 4, we would need to handle row padding
                    return Err(Error::InitializationFailed(format!(
                        "Line stride ({}) doesn't match width * 4 ({}). \
                         This would require handling row padding which this example doesn't implement.",
                        actual_stride, expected_stride
                    )));
                }

                // Check data size to detect if we might have a compressed format
                let expected_uncompressed_size = (frame.width * frame.height * 4) as usize;
                if frame.data.len() < expected_uncompressed_size / 2 {
                    eprintln!(
                        "  ⚠ Warning: Frame data size ({} bytes) is much smaller than expected",
                        frame.data.len()
                    );
                    eprintln!(
                        "            uncompressed size ({} bytes). This might be a compressed",
                        expected_uncompressed_size
                    );
                    eprintln!("            format that needs decoding before saving as PNG.");
                }

                break frame;
            }
            None => {
                // No frame available yet, wait a bit before retrying
                if attempts % 10 == 0 {
                    println!("Still waiting for video frame... (attempt {})", attempts);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    };

    // Save as PNG
    println!("\nSaving frame as PNG...");
    let file = File::create(output_file)?;
    let mut encoder = png::Encoder::new(file, video_frame.width as u32, video_frame.height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);

    encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(&video_frame.data))
        .map_err(|e| Error::InitializationFailed(format!("PNG encoding failed: {}", e)))?;

    println!("✓ Saved frame as {}", output_file);
    println!("\nExample completed successfully!");

    Ok(())
}
