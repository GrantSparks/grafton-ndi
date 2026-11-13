//! Example: Receiving NDI video and saving a frame as PNG.
//!
//! This example demonstrates receiving a video frame from the first
//! available NDI source and saving it as a PNG file.
//!
//! This is based on the NDIlib_Recv_PNG example from the NDI SDK.
//!
//! IMPORTANT: This example demonstrates:
//!
//! 1. Using `receiver.video().capture_ref()` for reliable frame capture with
//!    automatic retry logic (handles NDI SDK timeout quirks internally)
//! 2. `ReceiverVideoFrameRef::encode_png()` for crate-provided image conversion, which
//!    handles RGBX/BGRX padding bytes and padded rows
//!
//! Run with: `cargo run --features image-encoding --example NDIlib_Recv_PNG`
//!
//! The `image-encoding` feature is off by default and this example needs it.
//!
//! Optional arguments:
//! - IP address to search: `cargo run --features image-encoding --example NDIlib_Recv_PNG -- 192.168.0.100`
//! - Multiple IPs: `cargo run --features image-encoding --example NDIlib_Recv_PNG -- 192.168.0.100 10.0.0.0/24`
//! - Custom output file: `cargo run --features image-encoding --example NDIlib_Recv_PNG -- --output MyImage.png`
//! - Both: `cargo run --features image-encoding --example NDIlib_Recv_PNG -- 192.168.0.100 --output MyImage.png`

use grafton_ndi::{Ndi, Receiver, ReceiverColorFormat, ReceiverOptions};

use std::{
    env, fs,
    time::{Duration, Instant},
};

#[path = "common/mod.rs"]
mod common;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut extra_ips = Vec::new();
    let mut output_file = String::from("CoolNDIImage.png");
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--output" => output_file = common::option_value(&mut arguments, &argument)?,
            flag if flag.starts_with('-') => return Err(format!("unknown option: {flag}").into()),
            _ => extra_ips.push(argument),
        }
    }
    let extra_ips: Vec<&str> = extra_ips.iter().map(String::as_str).collect();
    println!("NDI Video Capture to PNG Example");
    println!("=================================\n");

    let ndi = Ndi::new()?;
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
    let options =
        ReceiverOptions::new(sources[0].clone()).color_format(ReceiverColorFormat::RgbxRgba);
    let receiver = Receiver::new(&ndi, &options)?;

    println!("Receiver created successfully");
    println!("Waiting for video frames...\n");

    let start_time = Instant::now();
    let Some(video_frame) = receiver.video().capture_ref(Duration::from_secs(60))? else {
        return Err("no video frame arrived within 60 seconds".into());
    };

    let elapsed = start_time.elapsed();
    println!("Frame received after {elapsed:?}");

    println!("Frame details:");
    let width = video_frame.width();
    let height = video_frame.height();
    println!("  Resolution: {width}x{height}");
    let fourcc = video_frame.pixel_format();
    println!("  Format: {fourcc:?}");
    let stride = video_frame.line_stride_bytes();
    println!("  Line stride: {stride} bytes");
    let data_size: usize = video_frame
        .planes()
        .map(|plane| plane.active_row_bytes() * plane.row_count())
        .sum();
    println!("  Data size: {data_size} bytes");
    let frame_rate = video_frame.frame_rate();
    println!(
        "  Frame rate: {}/{}",
        frame_rate.numerator(),
        frame_rate.denominator()
    );
    let timecode = video_frame.timecode().ticks();
    println!("  Timecode: {timecode:016x}");

    println!("\nSaving frame as PNG...");
    let png_bytes = video_frame.encode_png()?;
    fs::write(&output_file, png_bytes)?;

    println!("✓ Saved frame as {output_file}");
    println!("\nExample completed successfully!");

    Ok(())
}
