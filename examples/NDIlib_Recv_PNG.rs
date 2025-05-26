//! Example: Receiving NDI video and saving a frame as PNG.
//!
//! This example demonstrates receiving a video frame from the first
//! available NDI source and saving it as a PNG file.
//!
//! Note: Despite using NDIlib_recv_capture_v3 which should block up to the
//! timeout, in practice it returns immediately with None when no frames are
//! available. This example implements a retry loop to wait for frames.
//!
//! Run with: `cargo run --example NDIlib_Recv_PNG`

use grafton_ndi::{
    Error, Finder, FinderOptions, ReceiverBandwidth, ReceiverColorFormat, ReceiverOptions,
    VideoFrame, NDI,
};
use std::fs::File;

// Optional: Configure for specific test environments
fn create_finder_options() -> FinderOptions {
    // Uncomment and modify to customize for your environment:
    // FinderOptions::builder()
    //     .show_local_sources(true)
    //     .extra_ips("192.168.1.0/24")
    //     .build()

    FinderOptions::default()
}

fn main() -> Result<(), Error> {
    // Initialize NDI
    let ndi = NDI::new()?;

    // Create a finder
    let finder_options = create_finder_options();
    let finder = Finder::new(&ndi, &finder_options)?;

    // Wait until there is one source
    println!("Looking for sources ...");
    let sources = loop {
        finder.wait_for_sources(1000);
        let sources = finder.get_sources(0)?;
        if !sources.is_empty() {
            break sources;
        }
    };

    // Create a receiver for the first source
    // Force RGBX_RGBA format so we get data suitable for PNG encoding
    let receiver = ReceiverOptions::builder(sources[0].clone())
        .color(ReceiverColorFormat::RGBX_RGBA)
        .bandwidth(ReceiverBandwidth::Highest)
        .allow_video_fields(true)
        .build(&ndi)?;

    // Wait for up to 60 seconds to receive a video frame
    // Note: NDI SDK's capture_video doesn't block for the full timeout when no frames
    // are available, so we need to implement our own retry loop
    let start = std::time::Instant::now();
    let timeout_duration = std::time::Duration::from_secs(60);

    println!("Connected to source: {}", sources[0].name);
    println!("Waiting for video frames...");

    let video_frame = loop {
        match receiver.capture_video(100)? {
            // Short timeout for each attempt
            Some(frame) => {
                println!(
                    "Received frame: {}x{}, format: {:?}, timestamp: {}",
                    frame.width, frame.height, frame.fourcc, frame.timestamp
                );
                break Some(frame);
            }
            None => {
                if start.elapsed() >= timeout_duration {
                    break None;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    };

    if let Some(video_frame) = video_frame {
        // Debug information about the frame
        println!(
            "Received frame: {}x{}, fourcc: {:?}, data size: {} bytes",
            video_frame.width,
            video_frame.height,
            video_frame.fourcc,
            video_frame.data.len()
        );
        println!("Line stride: {:?}", video_frame.line_stride_or_size);

        // Check if this looks like a compressed format
        let expected_uncompressed_size = (video_frame.width * video_frame.height * 4) as usize;
        if video_frame.data.len() < expected_uncompressed_size / 2 {
            println!("WARNING: Frame data size ({} bytes) is much smaller than expected uncompressed size ({} bytes)", 
                     video_frame.data.len(), expected_uncompressed_size);
            println!("This might be a compressed video format that needs decoding.");
        }

        // Verify we got RGBA format as expected
        if !matches!(
            video_frame.fourcc,
            grafton_ndi::FourCCVideoType::RGBA | grafton_ndi::FourCCVideoType::RGBX
        ) {
            eprintln!(
                "Warning: Received format {:?} instead of RGBA/RGBX",
                video_frame.fourcc
            );
        }

        // Verify stride matches width (for RGBA it should be width * 4)
        let expected_stride = video_frame.width * 4;
        let actual_stride = unsafe { video_frame.line_stride_or_size.line_stride_in_bytes };
        assert_eq!(
            actual_stride, expected_stride,
            "Line stride ({}) doesn't match expected stride ({}) - would need to handle stride",
            actual_stride, expected_stride
        );

        // Save as PNG
        save_frame_as_png(&video_frame)?;
        println!("Saved frame as CoolNDIImage.png");
    }

    Ok(())
}

fn save_frame_as_png(frame: &VideoFrame) -> Result<(), Error> {
    let file = File::create("CoolNDIImage.png")?;
    let mut encoder = png::Encoder::new(file, frame.width as u32, frame.height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);

    encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(&frame.data))
        .map_err(|e| Error::InitializationFailed(format!("PNG encoding failed: {}", e)))?;

    Ok(())
}
