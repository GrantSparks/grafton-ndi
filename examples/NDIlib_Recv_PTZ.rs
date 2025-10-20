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

use grafton_ndi::{Error, Finder, FinderOptions, Receiver, ReceiverOptions, NDI};

/// Configure finder options for specific test environments
fn create_finder_options(extra_ips: Vec<&str>) -> FinderOptions {
    let mut builder = FinderOptions::builder();

    if !extra_ips.is_empty() {
        println!("Searching additional IPs/subnets:");
        for ip in &extra_ips {
            println!("  - {}", ip);
            builder = builder.extra_ips(*ip);
        }
        println!();
    }

    builder.build()
}

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
    let ndi = NDI::new()?;

    // Create finder
    let finder_options = create_finder_options(extra_ips);
    let finder = Finder::new(&ndi, &finder_options)?;

    // Wait until there is at least one source
    let sources = loop {
        if exit_loop.load(Ordering::Relaxed) {
            return Ok(());
        }
        finder.wait_for_sources(Duration::from_secs(1))?;
        let sources = finder.sources(Duration::ZERO)?;
        if !sources.is_empty() {
            break sources;
        }
    };

    // Create a receiver for the first source
    let options = ReceiverOptions::builder(sources[0].clone())
        .name("Example PTZ Receiver")
        .build();
    let receiver = Receiver::new(&ndi, &options)?;

    // Run for 30 seconds
    let start = Instant::now();
    while !exit_loop.load(Ordering::Relaxed) && start.elapsed() < Duration::from_secs(30) {
        // Use poll_status_change to check for status changes
        if let Some(_status) = receiver.poll_status_change(Duration::from_secs(1))? {
            // Check PTZ support on status change
            if receiver.ptz_is_supported() {
                println!("This source supports PTZ functionality. Moving to preset #3.");
                receiver.ptz_recall_preset(3, 1.0)?;
            }
        }
    }

    Ok(())
}
