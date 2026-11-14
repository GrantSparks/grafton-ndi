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
//! - Audio channels: `cargo run --example NDIlib_Recv_FrameSync -- --audio-channels 1`
//! - Samples per audio block: `cargo run --example NDIlib_Recv_FrameSync -- --audio-samples 512`

use grafton_ndi::{
    Error, FrameSync, FrameSyncAudioRequest, Ndi, Receiver, ReceiverColorFormat, ReceiverOptions,
    ScanType,
};

#[path = "common/arguments.rs"]
mod arguments;
#[path = "common/finder.rs"]
mod finder;
#[path = "common/source_wait.rs"]
mod source_wait;

use arguments::option_value;
use finder::finder_with_extra_ips;
use source_wait::wait_for_first_source;

use std::{
    env, thread,
    time::{Duration, Instant},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut extra_ips = Vec::new();
    let mut frame_count: u32 = 30;
    let mut audio_sample_rate = 48000;
    let mut audio_channels = 2;
    let mut audio_samples = 1024;
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--frames" => frame_count = option_value(&mut arguments, &argument)?,
            "--audio-rate" => audio_sample_rate = option_value(&mut arguments, &argument)?,
            "--audio-channels" => audio_channels = option_value(&mut arguments, &argument)?,
            "--audio-samples" => audio_samples = option_value(&mut arguments, &argument)?,
            flag if flag.starts_with('-') => {
                return Err(Error::InvalidInput(format!("unknown option: {flag}")).into());
            }
            _ => extra_ips.push(argument),
        }
    }
    if frame_count == 0 {
        return Err(Error::InvalidInput("--frames must be positive".into()).into());
    }
    // Validate the requested shape before waiting for a source.
    let audio_request =
        FrameSyncAudioRequest::capture_at(audio_sample_rate, audio_channels, audio_samples)?;
    let extra_ips: Vec<&str> = extra_ips.iter().map(String::as_str).collect();

    println!("NDI FrameSync Example");
    println!("=====================\n");

    let ndi = Ndi::new()?;
    println!("NDI initialized successfully");

    println!("Configuration:");
    println!("  Target frames: {frame_count}");
    println!("  Audio sample rate: {audio_sample_rate} Hz");
    println!("  Audio channels: {audio_channels}");
    println!("  Audio samples per capture: {audio_samples}");
    println!();

    let finder = finder_with_extra_ips(&ndi, &extra_ips)?;

    println!("Looking for sources ...");
    let sources = wait_for_first_source(&finder, || false)?
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

    println!("Creating FrameSync for clock-corrected capture...");
    let framesync = FrameSync::new(receiver)?;
    // The attached receiver exposes controls, metadata, and status.
    // Raw video becomes available again after framesync.into_receiver().
    println!(
        "PTZ supported: {}",
        framesync.receiver().ptz().is_supported()
    );
    println!("FrameSync created successfully");
    println!("\nCapturing {frame_count} frames with FrameSync...\n");

    let start_time = Instant::now();
    let mut video_frames = 0;
    let mut audio_samples_total = 0;
    let mut last_video_timecode = None;
    let mut repeated_timecodes = 0;
    let mut last_video = Instant::now();

    // One audio block defines each software output tick. Video is sampled at
    // this same cadence and may repeat or skip source frames as needed.
    let frame_interval = Duration::from_secs_f64(audio_samples as f64 / audio_sample_rate as f64)
        .max(Duration::from_nanos(1));
    println!("Software output interval: {frame_interval:?}");

    while video_frames < frame_count {
        let loop_start = Instant::now();

        // Sample available video without waiting for the next network frame.
        if let Some(video) = framesync.capture_video_ref(ScanType::Progressive)? {
            // A repeated timecode can indicate repeated video; senders may also
            // assign identical timecodes to different frames.
            if last_video_timecode == Some(video.timecode()) {
                repeated_timecodes += 1;
            }
            last_video_timecode = Some(video.timecode());

            if video_frames == 0 {
                println!("First video frame received:");
                println!("  Resolution: {}x{}", video.width(), video.height());
                println!("  Format: {:?}", video.pixel_format());
                let rate = video.frame_rate();
                println!("  Frame rate: {}/{}", rate.numerator(), rate.denominator());
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
            last_video = Instant::now();
        } else if video_frames == 0 {
            // Still waiting for first frame
            print!(".");
            use std::io::Write;
            std::io::stdout().flush().ok();
        }

        if last_video.elapsed() >= Duration::from_secs(10) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "no video available for 10 seconds",
            )
            .into());
        }

        // Request one output block; the SDK supplies silence if needed.
        let audio = framesync.capture_audio_ref(audio_request)?;
        audio_samples_total += audio.num_samples() as usize;

        if video_frames == 1 {
            // On the first video frame, report the source's audio format and
            // the blocks requested from it.
            match framesync.audio_input_format()? {
                Some(input) => {
                    println!("Audio stream info:");
                    println!(
                        "  Source: {} channels at {} Hz",
                        input.num_channels(),
                        input.sample_rate()
                    );
                    println!(
                        "  Requested: {} channels at {} Hz",
                        audio.num_channels(),
                        audio.sample_rate()
                    );
                    println!("  Samples per block: {}", audio.num_samples());
                    println!("  Format: {:?}", audio.format());
                    println!();
                }
                None => println!("No audio stream detected yet; capturing silence\n"),
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
