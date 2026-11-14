//! Example: fetching the audio that belongs to each video frame.
//!
//! This example demonstrates:
//! - Handing a receiver to an [`AvSync`]
//! - Capturing video through it, in place in the SDK's buffer
//! - Asking for the audio matching each frame's timestamp, without copying
//!   the frame
//! - Polling the receiver's events and publishing tally while it is attached
//!
//! Needs the NDI Advanced SDK at build time and an Advanced runtime.
//!
//! Run with: `cargo run --features advanced --example advanced_avsync`
//!
//! Optional arguments:
//! - IP address to search: `cargo run --features advanced --example advanced_avsync -- 192.168.0.110`

use std::{env, time::Duration};

use grafton_ndi::{
    advanced::{AvSync, AvSyncOutcome, AvSyncRequest},
    Error, Ndi, Receiver, ReceiverEvent, ReceiverOptions, RuntimeOptions, SdkEdition, Tally,
};

#[path = "common/finder.rs"]
mod finder;

use finder::finder_with_extra_ips;

fn main() -> Result<(), Error> {
    let args: Vec<String> = env::args().collect();
    let extra_ips: Vec<&str> = args[1..].iter().map(|s| s.as_str()).collect();

    let ndi = Ndi::with_options(&RuntimeOptions::new().edition(SdkEdition::Advanced))?;
    let finder = finder_with_extra_ips(&ndi, &extra_ips)?;
    let Some(source) = finder
        .find_sources(Duration::from_secs(5))?
        .into_iter()
        .next()
    else {
        println!("No NDI sources found!");
        return Ok(());
    };
    println!("Synchronizing {source}");

    let receiver = Receiver::new(&ndi, &ReceiverOptions::new(source).name("AVSync Example"))?;
    // On failure the receiver comes back inside the error; `?` drops it here.
    let sync = AvSync::new(receiver)?;
    // Every receiver control stays available while the receiver is attached.
    sync.receiver().set_tally(Tally::new(true, false));

    for _ in 0..120 {
        if let Some(ReceiverEvent::StatusChanged(status)) =
            sync.receiver().poll_event(Duration::ZERO)?
        {
            println!("{} connections", status.connections());
        }
        // The frame borrows the SDK's buffer, and the synchronizer reads it
        // there: no pixels are copied.
        let Some(video) = sync
            .receiver()
            .video()
            .capture_ref(Duration::from_secs(1))?
        else {
            println!("No video yet...");
            continue;
        };
        match sync.synchronize(Some(&video), AvSyncRequest::automatic())? {
            AvSyncOutcome::Audio { frame, .. } => println!(
                "{}x{} video with {} audio samples at {} Hz",
                video.width(),
                video.height(),
                frame.num_samples(),
                frame.sample_rate()
            ),
            AvSyncOutcome::NoAudioStream => println!("The source sends no audio"),
            other => println!("No audio for this frame: {other:?}"),
        }
    }

    // The receiver is ordinary again once synchronization stops.
    let receiver = sync.into_receiver();
    println!("Still connected: {}", receiver.is_connected());
    Ok(())
}
