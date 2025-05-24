//! Example: Receiving NDI video and saving a frame as PNG.
//!
//! This example demonstrates receiving a video frame from the first
//! available NDI source and saving it as a PNG file.
//!
//! Run with: `cargo run --example NDIlib_Recv_PNG`

use grafton_ndi::{
    Error, Finder, FinderOptions, ReceiverColorFormat, ReceiverOptions, VideoFrame, NDI,
};
use std::fs::File;

// Optional: Configure for specific test environments
fn create_finder_options() -> FinderOptions {
    // Uncomment to customize for your environment:
    // FinderOptions::builder()
    //     .show_local_sources(false)
    //     .extra_ips("192.168.0.110")
    //     .build()
    FinderOptions::builder().build()
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
    let receiver = ReceiverOptions::builder(sources[0].clone())
        .color(ReceiverColorFormat::RGBX_RGBA)
        .build(&ndi)?;

    // Wait for up to a minute to receive a video frame
    if let Some(video_frame) = receiver.capture_video(60000)? {
        // Verify stride matches width (same check as C++ example)
        assert_eq!(
            unsafe { video_frame.line_stride_or_size.line_stride_in_bytes },
            video_frame.width * 4,
            "Line stride doesn't match width - would need to handle stride"
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
