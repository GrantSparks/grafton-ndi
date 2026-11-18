//! Example: pacing a sender from another source's clock.
//!
//! This example demonstrates:
//! - Locking a [`Genlock`] to a reference source
//! - Waiting on its video clock before each frame
//! - Constructing a reusable audio clock request for audio pacing
//! - Seeing when the system clock stands in for a missing reference
//!
//! Needs the NDI Advanced SDK at build time and an Advanced runtime.
//!
//! Run with: `cargo run --features advanced --example advanced_genlock`
//!
//! Optional arguments:
//! - IP address to search: `cargo run --features advanced --example advanced_genlock -- 192.168.0.110`

use std::{env, time::Duration};

use grafton_ndi::{
    advanced::{AudioClockRequest, ClockSource, Genlock, GenlockOptions},
    FrameRate, Ndi, RuntimeOptions, ScanType, SdkEdition, Sender, SenderOptions, VideoFrame,
};

#[path = "common/finder.rs"]
mod finder;

use finder::finder_with_extra_ips;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let extra_ips: Vec<&str> = args[1..].iter().map(|s| s.as_str()).collect();

    let ndi = Ndi::with_options(&RuntimeOptions::new().edition(SdkEdition::Advanced))?;
    let finder = finder_with_extra_ips(&ndi, &extra_ips)?;
    let reference = finder
        .find_sources(Duration::from_secs(5))?
        .into_iter()
        .next();
    match &reference {
        Some(source) => println!("Locking to {source}"),
        None => println!("No reference source found: running on the system clock"),
    }

    let mut options = GenlockOptions::new();
    if let Some(source) = reference {
        options = options.source(source);
    }
    let mut genlock = Genlock::new(&ndi, &options)?;
    // The genlock paces the frames, so the sender's own clock is switched off.
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new("Genlocked Sender Example").clock_video(false),
    )?;
    let frame_rate = FrameRate::new(60_000, 1001)?;
    let frame = VideoFrame::builder()
        .resolution(1920, 1080)
        .frame_rate(frame_rate)
        .build()?;

    // An audio producer can reuse this request with audio_clock.wait(request)
    // on its own scoped thread while this thread waits for video.
    let audio_request = AudioClockRequest::new(48_000, 800)?;
    println!("Audio pacing request: {audio_request:?}");
    let (mut video_clock, _audio_clock) = genlock.clocks();
    let mut paced_by = None;
    for _ in 0..600 {
        let clock = video_clock.wait(frame_rate, ScanType::Progressive);
        if paced_by != Some(clock) {
            match clock {
                ClockSource::ReferenceSignal => println!("Paced by the reference source"),
                _ => println!("Paced by the system clock"),
            }
            paced_by = Some(clock);
        }
        sender.send_video(&frame)?;
    }
    Ok(())
}
