//! Example: Receiving NDI video and saving a frame as PNG.
//!
//! This example demonstrates receiving a video frame from the first
//! available NDI source and saving it as a PNG file.
//!
//! This is based on the NDIlib_Recv_PNG example from the NDI SDK.
//!
//! IMPORTANT: This example demonstrates:
//!
//! 1. Using `capture_video_blocking()` for reliable frame capture with
//!    automatic retry logic (handles NDI SDK timeout quirks internally)
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

use std::{env, fs::File, time::Instant};

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
            eprintln!("Unknown argument: {arg}", arg = args[i]);
            i += 1;
        }
    }
    println!("NDI Video Capture to PNG Example");
    println!("=================================\n");

    // Initialize NDI
    let ndi = NDI::new()?;
    println!("NDI initialized successfully");

    if output_file != "CoolNDIImage.png" {
        println!("Output file: {output_file}");
    }
    println!();

    // Create a finder
    let mut builder = FinderOptions::builder().show_local_sources(true);

    // Add any command line IPs
    if !extra_ips.is_empty() {
        println!("Searching additional IPs/subnets:");
        for ip in &extra_ips {
            println!("  - {ip}");
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
            let count = sources.len();
            println!("Found {count} source(s):");
            for (i, source) in sources.iter().enumerate() {
                let num = i + 1;
                println!("  {num}. {source}");
            }
            break sources;
        }
    };

    // Create a receiver for the first source
    let first_source = &sources[0];
    println!("\nCreating receiver for: {first_source}");
    let receiver = ReceiverOptions::builder(sources[0].clone())
        .color(ReceiverColorFormat::RGBX_RGBA)
        .build(&ndi)?;

    println!("Receiver created successfully");
    println!("Waiting for video frames...\n");

    // Use the new blocking capture method that handles retry logic internally
    // This is much simpler than manually implementing the retry loop
    let start_time = Instant::now();
    let video_frame = receiver.capture_video_blocking(60_000)?;

    let elapsed = start_time.elapsed();
    println!("Frame received after {elapsed:?}");

    // Debug information about the frame
    println!("Frame details:");
    let width = video_frame.width;
    let height = video_frame.height;
    println!("  Resolution: {width}x{height}");
    let fourcc = video_frame.fourcc;
    println!("  Format: {fourcc:?}");
    let line_stride = match video_frame.line_stride_or_size {
        grafton_ndi::LineStrideOrSize::LineStrideBytes(stride) => stride,
        grafton_ndi::LineStrideOrSize::DataSizeBytes(_) => {
            eprintln!("ERROR: Expected line stride but got data size");
            return Err(Error::InvalidFrame(
                "Frame has data size instead of line stride".into(),
            ));
        }
    };
    println!("  Line stride: {line_stride} bytes");
    let data_size = video_frame.data.len();
    println!("  Data size: {data_size} bytes");
    let frame_rate_n = video_frame.frame_rate_n;
    let frame_rate_d = video_frame.frame_rate_d;
    println!("  Frame rate: {frame_rate_n}/{frame_rate_d}");
    let timecode = video_frame.timecode;
    println!("  Timecode: {timecode:016x}");

    // Verify we got the format we requested
    match video_frame.fourcc {
        FourCCVideoType::RGBA | FourCCVideoType::RGBX => {
            println!("  ✓ Got requested RGBA/RGBX format");
        }
        _ => {
            let format = video_frame.fourcc;
            eprintln!("  ⚠ Warning: Got unexpected format {format:?}, PNG may fail");
        }
    }

    // CRITICAL: Verify stride matches width to prevent corrupted images
    let expected_stride = video_frame.width * 4; // 4 bytes per pixel for RGBA
    let actual_stride = line_stride;

    if actual_stride != expected_stride {
        // This is a common issue with some NDI sources
        // If stride != width * 4, we would need to handle row padding
        return Err(Error::InitializationFailed(format!(
            "Line stride ({actual_stride}) doesn't match width * 4 ({expected_stride}). \
             This would require handling row padding which this example doesn't implement."
        )));
    }

    // Check data size to detect if we might have a compressed format
    let expected_uncompressed_size = (video_frame.width * video_frame.height * 4) as usize;
    if video_frame.data.len() < expected_uncompressed_size / 2 {
        let actual_size = video_frame.data.len();
        eprintln!(
            "  ⚠ Warning: Frame data size ({actual_size} bytes) is much smaller than expected"
        );
        eprintln!("            uncompressed size ({expected_uncompressed_size} bytes). This might be a compressed");
        eprintln!("            format that needs decoding before saving as PNG.");
    }

    // Save as PNG
    println!("\nSaving frame as PNG...");
    let file = File::create(output_file)?;
    let mut encoder = png::Encoder::new(file, video_frame.width as u32, video_frame.height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);

    encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(&video_frame.data))
        .map_err(|e| Error::InitializationFailed(format!("PNG encoding failed: {e}")))?;

    println!("✓ Saved frame as {output_file}");
    println!("\nExample completed successfully!");

    Ok(())
}
