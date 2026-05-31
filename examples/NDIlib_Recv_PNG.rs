//! Example: Receiving NDI video and saving a frame as PNG.
//!
//! This example demonstrates receiving a video frame from the first
//! available NDI source and saving it as a PNG file.
//!
//! This is based on the NDIlib_Recv_PNG example from the NDI SDK.
//!
//! IMPORTANT: This example demonstrates:
//!
//! 1. Using `capture_video()` for reliable frame capture with
//!    automatic retry logic (handles NDI SDK timeout quirks internally)
//! 2. `VideoFrame::encode_png()` for crate-provided image conversion
//! 3. Correct handling for RGBX/BGRX padding bytes and padded rows
//!
//! Run with: `cargo run --example NDIlib_Recv_PNG`
//!
//! If default features are disabled, add `--features image-encoding`.
//!
//! Optional arguments:
//! - IP address to search: `cargo run --example NDIlib_Recv_PNG -- 192.168.0.100`
//! - Multiple IPs: `cargo run --example NDIlib_Recv_PNG -- 192.168.0.100 10.0.0.0/24`
//! - Custom output file: `cargo run --example NDIlib_Recv_PNG -- --output MyImage.png`
//! - Both: `cargo run --example NDIlib_Recv_PNG -- 192.168.0.100 --output MyImage.png`

use grafton_ndi::{Error, Receiver, ReceiverColorFormat, ReceiverOptions, NDI};

use std::{
    env, fs,
    time::{Duration, Instant},
};

#[path = "common/mod.rs"]
mod common;

fn main() -> Result<(), Error> {
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

    let ndi = NDI::new()?;
    println!("NDI initialized successfully");

    if output_file != "CoolNDIImage.png" {
        println!("Output file: {output_file}");
    }
    println!();

    let finder = common::finder_with_extra_ips(&ndi, &extra_ips)?;

    println!("Looking for sources ...");
    let sources = common::wait_for_first_source(&finder, || false)?
        .expect("wait_for_first_source returns None only when the stop closure fires");
    let count = sources.len();
    println!("Found {count} source(s):");
    for (i, source) in sources.iter().enumerate() {
        let num = i + 1;
        println!("  {num}. {source}");
    }

    let first_source = &sources[0];
    println!("\nCreating receiver for: {first_source}");
    let options = ReceiverOptions::builder(sources[0].clone())
        .color(ReceiverColorFormat::RGBX_RGBA)
        .build();
    let receiver = Receiver::new(&ndi, &options)?;

    println!("Receiver created successfully");
    println!("Waiting for video frames...\n");

    let start_time = Instant::now();
    let video_frame = receiver.capture_video(Duration::from_secs(60))?;

    let elapsed = start_time.elapsed();
    println!("Frame received after {elapsed:?}");

    println!("Frame details:");
    let width = video_frame.width();
    let height = video_frame.height();
    println!("  Resolution: {width}x{height}");
    let fourcc = video_frame.pixel_format();
    println!("  Format: {fourcc:?}");
    match video_frame.line_stride_or_size() {
        grafton_ndi::LineStrideOrSize::LineStrideBytes(stride) => {
            println!("  Line stride: {stride} bytes");
        }
        grafton_ndi::LineStrideOrSize::DataSizeBytes(size) => {
            println!("  Data size layout: {size} bytes");
        }
    }
    let data_size = video_frame.data().len();
    println!("  Data size: {data_size} bytes");
    let frame_rate_n = video_frame.frame_rate_n();
    let frame_rate_d = video_frame.frame_rate_d();
    println!("  Frame rate: {frame_rate_n}/{frame_rate_d}");
    let timecode = video_frame.timecode();
    println!("  Timecode: {timecode:016x}");

    println!("\nSaving frame as PNG...");
    let png_bytes = video_frame.encode_png()?;
    fs::write(output_file, png_bytes)?;

    println!("✓ Saved frame as {output_file}");
    println!("\nExample completed successfully!");

    Ok(())
}
