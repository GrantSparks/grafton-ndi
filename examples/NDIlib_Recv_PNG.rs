//! Example: Receiving NDI video and saving frames as PNG images.
//!
//! This example demonstrates:
//! - Finding a specific NDI source by name
//! - Setting up a receiver with specific color format
//! - Capturing video frames
//! - Saving frames as PNG files
//!
//! Run with: `cargo run --example NDIlib_Recv_PNG`

use grafton_ndi::{Error, Find, Finder, Receiver, RecvBandwidth, RecvColorFormat, VideoFrame, NDI};
use std::fs::File;

fn main() -> Result<(), Error> {
    println!("NDI Video Capture to PNG Example");
    println!("=================================\n");

    // Initialize the NDI runtime
    let ndi = NDI::new()?;
    println!("NDI initialized successfully\n");

    // Configure the finder to search for sources
    // We exclude local sources and add a specific IP to search
    let finder = Finder::builder()
        .show_local_sources(false)
        .extra_ips("192.168.0.110")
        .build();

    let ndi_find = Find::new(&ndi, &finder)?;

    // Search for a specific source by name
    let source_name = "CAMERA4";
    println!("Searching for source: {}\n", source_name);

    let mut found_source = None;
    let mut search_attempts = 0;

    while found_source.is_none() {
        search_attempts += 1;

        // Wait for sources to appear or change (timeout: 5 seconds)
        if search_attempts > 1 {
            println!("Still searching... (attempt {})", search_attempts);
        }

        ndi_find.wait_for_sources(5000);
        let sources = ndi_find.get_sources(5000)?;

        // Display all found sources
        if !sources.is_empty() {
            println!("Available sources:");
            for (i, source) in sources.iter().enumerate() {
                println!("  {}. {}", i + 1, source);

                // Check if this is the source we're looking for
                if source.name.contains(source_name) {
                    found_source = Some(source.clone());
                    println!("\n✓ Target source found!");
                    break;
                }
            }
            println!();
        } else {
            println!("No sources found yet...");
        }
    }

    let source = found_source.unwrap();

    // Create a receiver for the found source
    println!("Creating receiver for: {}\n", source);

    let ndi_recv = Receiver::builder(source)
        .color(RecvColorFormat::RGBX_RGBA) // Request RGBA format for PNG
        .bandwidth(RecvBandwidth::Highest) // Maximum quality
        .allow_video_fields(false) // Progressive frames only
        .name("PNG Capture Example") // Identify our receiver
        .build(&ndi)?;

    println!("Receiver created successfully");
    println!("Waiting for video frames...\n");

    // Wait until we have a video frame
    let video_frame = loop {
        // Sleep for 5 seconds
        std::thread::sleep(std::time::Duration::from_secs(5));

        println!("Waiting for video frame ...");
        match ndi_recv.capture_video(60000) {
            Ok(Some(frame)) => break frame,
            Ok(None) => println!("No video frame available yet."),
            Err(e) => eprintln!("Error capturing video frame: {}", e),
        }
    };

    // Save the frame as a PNG file
    if let Err(e) = save_frame_as_png(&video_frame) {
        eprintln!("Failed to save frame as PNG: {}", e);
    }

    // The NDI receiver will be destroyed automatically when it goes out of scope
    // The NDI library will be destroyed automatically when `ndi` goes out of scope

    Ok(())
}

fn save_frame_as_png(video_frame: &VideoFrame) -> Result<(), Error> {
    let path = "CoolNDIImage.png";

    let file = File::create(path)?;

    let mut encoder = png::Encoder::new(file, video_frame.xres as u32, video_frame.yres as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);

    // Debugging info
    println!(
        "Saving frame with resolution: {}x{}, data_size_in_bytes: {}",
        video_frame.xres,
        video_frame.yres,
        unsafe { video_frame.line_stride_or_size.data_size_in_bytes }
    );

    // Ensure the data is not empty
    if video_frame.data.is_empty() {
        return Err(Error::InitializationFailed("Frame data is empty".into()));
    }

    let mut writer = encoder
        .write_header()
        .map_err(|e| Error::InitializationFailed(format!("Failed to write PNG header: {}", e)))?;

    writer
        .write_image_data(&video_frame.data)
        .map_err(|e| Error::InitializationFailed(format!("Failed to write PNG data: {}", e)))?;

    Ok(())
}
