//! Example: Controlling PTZ (Pan-Tilt-Zoom) cameras via NDI.
//!
//! This example demonstrates detecting PTZ support and recalling presets
//! when the receiver status changes.
//!
//! Run with: `cargo run --example NDIlib_Recv_PTZ`

use grafton_ndi::{Error, Find, Finder, Receiver, NDI};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

// Optional: Configure for specific test environments
fn create_finder() -> Finder {
    // Uncomment to customize:
    // Finder::builder()
    //     .show_local_sources(false)
    //     .extra_ips("192.168.0.110")
    //     .build()
    Finder::builder().build()
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
    let finder = create_finder();
    let ndi_find = Find::new(&ndi, &finder)?;

    // Wait until there is at least one source
    let sources = loop {
        if exit_loop.load(Ordering::Relaxed) {
            return Ok(());
        }
        ndi_find.wait_for_sources(1000);
        let sources = ndi_find.get_sources(0)?;
        if !sources.is_empty() {
            break sources;
        }
    };

    // Create a receiver for the first source
    let ndi_recv = Receiver::builder(sources[0].clone())
        .name("Example PTZ Receiver")
        .build(&ndi)?;

    // Run for 30 seconds
    let start = Instant::now();
    while !exit_loop.load(Ordering::Relaxed) && start.elapsed() < Duration::from_secs(30) {
        // Use poll_status_change to check for status changes
        if let Some(_status) = ndi_recv.poll_status_change(1000) {
            // Check PTZ support on status change
            if ndi_recv.ptz_is_supported() {
                println!("This source supports PTZ functionality. Moving to preset #3.");
                ndi_recv.ptz_recall_preset(3, 1.0)?;
            }
        }
    }

    Ok(())
}
