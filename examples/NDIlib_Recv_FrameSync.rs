//! Example: Receiving NDI video and audio using FrameSync for clock-corrected capture.
//!
//! This example demonstrates using the FrameSync API to receive video and audio
//! with automatic time-base correction. FrameSync is ideal for:
//!
//! - Video playback synced to GPU v-sync
//! - Audio playback synced to sound card clock
//! - Multi-source mixing with a common output clock
//!
//! This is based on the NDIlib_Recv_FrameSync example from the NDI SDK.
//!
//! Key differences from raw Receiver capture:
//!
//! 1. **No network-frame wait** - the SDK uses available frames or audio silence
//! 2. **Video uses time-base correction** - handles clock drift between sender/receiver
//! 3. **Audio uses dynamic resampling** - matches your output sample rate automatically
//! 4. **Video may repeat or skip frames** - according to the output sampling cadence
//!
//! This example uses a software timer, not a hardware audio or display clock.
//! The requested audio block duration sets the output tick; real playback should
//! request samples according to the audio device clock. Capture and resampling
//! still take processing time, and wrapper synchronization may briefly block.
//!
//! Run with: `cargo run --example NDIlib_Recv_FrameSync`
//!
//! Optional arguments:
//! - IP address to search: `cargo run --example NDIlib_Recv_FrameSync -- 192.168.0.100`
//! - Frame count: `cargo run --example NDIlib_Recv_FrameSync -- --frames 100`
//! - Audio sample rate: `cargo run --example NDIlib_Recv_FrameSync -- --audio-rate 44100`

use grafton_ndi::{
    Error, FrameSync, FrameSyncAudioRequest, Receiver, ReceiverColorFormat, ReceiverOptions,
    ScanType, NDI,
};

#[path = "common/mod.rs"]
mod common;

use std::{
    env, thread,
    time::{Duration, Instant},
};

fn main() -> Result<(), Error> {
    let args: Vec<String> = env::args().collect();
    let mut extra_ips = Vec::new();
    let mut frame_count = 30; // Default to 30 frames
    let mut audio_sample_rate = 48000;
    let mut audio_channels = 2;
    let mut audio_samples = 1024;

    let mut i = 1;
    while i < args.len() {
        if args[i] == "--frames" && i + 1 < args.len() {
            frame_count = args[i + 1].parse().unwrap_or(30);
            i += 2;
        } else if args[i] == "--audio-rate" && i + 1 < args.len() {
            audio_sample_rate = args[i + 1].parse().unwrap_or(48000);
            i += 2;
        } else if args[i] == "--audio-channels" && i + 1 < args.len() {
            audio_channels = args[i + 1].parse().unwrap_or(2);
            i += 2;
        } else if args[i] == "--audio-samples" && i + 1 < args.len() {
            audio_samples = args[i + 1].parse().unwrap_or(1024);
            i += 2;
        } else if !args[i].starts_with("--") {
            extra_ips.push(args[i].as_str());
            i += 1;
        } else {
            eprintln!("Unknown argument: {arg}", arg = args[i]);
            i += 1;
        }
    }

    println!("NDI FrameSync Example");
    println!("=====================\n");

    let ndi = NDI::new()?;
    println!("NDI initialized successfully");

    println!("Configuration:");
    println!("  Target frames: {frame_count}");
    println!("  Audio sample rate: {audio_sample_rate} Hz");
    println!("  Audio channels: {audio_channels}");
    println!("  Audio samples per capture: {audio_samples}");
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

    println!("Creating FrameSync for clock-corrected capture...");
    let framesync = FrameSync::new(receiver)?;
    let audio_request = FrameSyncAudioRequest::capture_with(
        Some(audio_sample_rate),
        audio_channels,
        audio_samples,
    )?;

    println!("FrameSync created successfully");
    println!("\nCapturing {frame_count} frames with FrameSync...\n");

    let start_time = Instant::now();
    let mut video_frames = 0;
    let mut audio_samples_total = 0;
    let mut last_video_timecode: i64 = 0;
    let mut repeated_timecodes = 0;

    // One audio block defines each software output tick. Video is sampled at
    // this same cadence and may repeat or skip source frames as needed.
    let frame_interval = Duration::from_secs_f64(audio_samples as f64 / audio_sample_rate as f64)
        .max(Duration::from_nanos(1));
    println!("Software output interval: {frame_interval:?}");

    while video_frames < frame_count {
        let loop_start = Instant::now();

        // Sample available video without waiting for the next network frame.
        if let Some(video) = framesync.capture_video(ScanType::Progressive)? {
            // A repeated timecode can indicate repeated video; senders may also
            // assign identical timecodes to different frames.
            if video.timecode() == last_video_timecode && video_frames > 0 {
                repeated_timecodes += 1;
            }
            last_video_timecode = video.timecode();

            if video_frames == 0 {
                println!("First video frame received:");
                println!("  Resolution: {}x{}", video.width(), video.height());
                println!("  Format: {:?}", video.pixel_format());
                println!(
                    "  Frame rate: {}/{}",
                    video.frame_rate_n(),
                    video.frame_rate_d()
                );
                println!(
                    "  Data size: {} bytes",
                    video
                        .planes()
                        .map(|p| p.active_row_bytes() * p.row_count())
                        .sum::<usize>()
                );
                println!();
            }

            video_frames += 1;
        } else if video_frames == 0 {
            // Still waiting for first frame
            print!(".");
            use std::io::Write;
            std::io::stdout().flush().ok();
        }

        // Request one output block; the SDK supplies silence if needed.
        let audio = framesync.capture_audio(audio_request)?;
        audio_samples_total += audio.num_samples() as usize;

        if video_frames == 1 {
            // Print audio info on first video frame
            if !audio.is_empty() {
                println!("Audio stream info:");
                println!("  Sample rate: {} Hz", audio.sample_rate());
                println!("  Channels: {}", audio.num_channels());
                println!("  Samples per frame: {}", audio.num_samples());
                println!("  Format: {:?}", audio.format());
                println!();
            } else {
                println!("No audio stream detected yet\n");
            }
        }

        // Print progress every 10 frames
        if video_frames > 0 && video_frames % 10 == 0 {
            let queue_depth = framesync.audio_queue_depth();
            println!(
                "Progress: {video_frames}/{frame_count} frames, audio queue: {queue_depth} samples"
            );
        }

        // Approximate the output clock; processing delays may make ticks late.
        let elapsed = loop_start.elapsed();
        if elapsed < frame_interval {
            thread::sleep(frame_interval - elapsed);
        }
    }

    let total_time = start_time.elapsed();
    let actual_fps = video_frames as f64 / total_time.as_secs_f64();

    println!("\n--- Capture Complete ---\n");
    println!("Results:");
    println!("  Total time: {total_time:.2?}");
    println!("  Video frames: {video_frames}");
    println!("  Repeated video timecodes: {repeated_timecodes}");
    println!("  Video sampling rate: {actual_fps:.2} frames/s");
    println!("  Total audio samples: {audio_samples_total}");

    // Calculate audio time
    let audio_duration_secs = audio_samples_total as f64 / audio_sample_rate as f64;
    println!("  Audio duration: {audio_duration_secs:.2}s at {audio_sample_rate} Hz");

    println!("\nWhy FrameSync?");
    println!("  - Video is selected for the requested output cadence");
    println!("  - Audio clock drift is handled by dynamic resampling");
    println!("  - SDK capture does not wait for a new network frame");
    if repeated_timecodes > 0 {
        println!("  - Observed {repeated_timecodes} consecutive repeated video timecodes");
    }

    println!("\nExample completed successfully!");

    Ok(())
}
