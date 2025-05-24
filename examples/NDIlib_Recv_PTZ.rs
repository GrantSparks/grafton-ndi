//! Example: Controlling PTZ (Pan-Tilt-Zoom) cameras via NDI.
//!
//! This example demonstrates detecting PTZ support and recalling presets
//! when the receiver status changes.
//!
//! Run with: `cargo run --example NDIlib_Recv_PTZ`

use grafton_ndi::{Error, Finder, FinderOptions, ReceiverOptions, NDI};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

// Optional: Configure for specific test environments
fn create_finder_options() -> FinderOptions {
    // Uncomment to customize:
    // FinderOptions::builder()
    //     .show_local_sources(false)
    //     .extra_ips("192.168.0.110")
    //     .build()
    FinderOptions::builder().build()
}

fn main() -> Result<(), Error> {
    // Set up signal handler for graceful shutdown
    let exit_loop = Arc::new(AtomicBool::new(false));
    let exit_loop_clone = exit_loop.clone();
    ctrlc::set_handler(move || {
        exit_loop_clone.store(true, Ordering::Relaxed);
    })
    .expect("Error setting Ctrl-C handler");

    // Initialize NDI
    let ndi = NDI::new()?;

    // Create finder
    let finder_options = create_finder_options();
    let finder = Finder::new(&ndi, &finder_options)?;

    // Wait until there is at least one source
    let sources = loop {
        if exit_loop.load(Ordering::Relaxed) {
            return Ok(());
        }
        finder.wait_for_sources(1000);
        let sources = finder.get_sources(0)?;
        if !sources.is_empty() {
            break sources;
        }
    };

    // Create a receiver for the first source
    let receiver = ReceiverOptions::builder(sources[0].clone())
        .name("Example PTZ Receiver")
        .build(&ndi)?;

    // Run for 30 seconds
    let start = Instant::now();
    while !exit_loop.load(Ordering::Relaxed) && start.elapsed() < Duration::from_secs(30) {
        // Use poll_status_change to check for status changes
        if let Some(_status) = receiver.poll_status_change(1000) {
            // Check PTZ support on status change
            if receiver.ptz_is_supported() {
                println!("This source supports PTZ functionality. Moving to preset #3.");
                receiver.ptz_recall_preset(3, 1.0)?;
            }
        }
    }

    Ok(())
}
