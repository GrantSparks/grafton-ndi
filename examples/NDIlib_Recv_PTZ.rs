//! Example: Controlling PTZ (Pan-Tilt-Zoom) cameras via NDI.
//!
//! This example demonstrates detecting PTZ support and recalling presets
//! when the receiver status changes.
//!
//! Run with: `cargo run --example NDIlib_Recv_PTZ`
//!
//! Optional arguments:
//! - IP address to search: `cargo run --example NDIlib_Recv_PTZ -- 192.168.0.110`

use std::{
    env,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use grafton_ndi::{Error, Ndi, Receiver, ReceiverEvent, ReceiverOptions};

#[path = "common/finder.rs"]
mod finder;
#[path = "common/source_wait.rs"]
mod source_wait;

use finder::finder_with_extra_ips;
use source_wait::wait_for_first_source;

fn main() -> Result<(), Error> {
    // Parse command line arguments for extra IPs
    let args: Vec<String> = env::args().collect();
    let extra_ips: Vec<&str> = args[1..].iter().map(|s| s.as_str()).collect();

    // Set up signal handler for graceful shutdown
    let exit_loop = Arc::new(AtomicBool::new(false));
    let exit_loop_clone = exit_loop.clone();
    ctrlc::set_handler(move || {
        exit_loop_clone.store(true, Ordering::Relaxed);
    })
    .expect("Error setting Ctrl-C handler");

    // Initialize NDI
    let ndi = Ndi::new()?;

    // Create finder
    let finder = finder_with_extra_ips(&ndi, &extra_ips)?;

    // Wait until there is at least one source (or Ctrl-C)
    let Some(sources) = wait_for_first_source(&finder, || exit_loop.load(Ordering::Relaxed))?
    else {
        return Ok(());
    };

    // Create a receiver for the first source
    let options = ReceiverOptions::new(sources[0].clone()).name("Example PTZ Receiver");
    let receiver = Receiver::new(&ndi, &options)?;

    // Run for 30 seconds
    let start = Instant::now();
    while !exit_loop.load(Ordering::Relaxed) && start.elapsed() < Duration::from_secs(30) {
        // Use poll_event to check for status changes
        if let Some(ReceiverEvent::StatusChanged(_status)) =
            receiver.poll_event(Duration::from_secs(1))?
        {
            // Check PTZ support on status change
            if receiver.ptz().is_supported() {
                println!("This source supports PTZ functionality. Moving to preset #3.");
                receiver.ptz().recall_preset(3, 1.0)?;
            }
        }
    }

    Ok(())
}
