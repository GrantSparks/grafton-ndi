//! Example: Controlling PTZ (Pan-Tilt-Zoom) cameras via NDI.
//!
//! This example demonstrates:
//! - Discovering NDI sources with PTZ support
//! - Connecting to a PTZ-enabled source
//! - Sending PTZ control commands
//! - Monitoring PTZ status
//!
//! PTZ support requires an NDI-enabled camera that supports PTZ commands.
//!
//! Run with: `cargo run --example NDIlib_Recv_PTZ`

use grafton_ndi::{Find, Finder, Receiver, RecvBandwidth, RecvColorFormat, NDI};
use std::time::{Duration, Instant};

fn main() {
    println!("NDI PTZ Camera Control Example");
    println!("==============================\n");

    // Initialize NDI
    let ndi = match NDI::new() {
        Ok(ndi) => ndi,
        Err(e) => {
            eprintln!("Failed to initialize NDI: {}", e);
            return;
        }
    };

    // Configure source discovery
    let finder = Finder::builder()
        .show_local_sources(false)
        .extra_ips("192.168.0.110")
        .build();

    let ndi_find = Find::new(&ndi, finder).expect("Failed to create NDI find instance");

    println!("Searching for NDI sources...");

    // Wait for sources to appear
    let mut sources = vec![];
    let mut attempts = 0;

    while sources.is_empty() {
        attempts += 1;
        if attempts > 1 {
            print!(".");
            use std::io::{stdout, Write};
            stdout().flush().ok();
        }

        if ndi_find.wait_for_sources(1000) {
            sources = ndi_find.get_sources(0).expect("Failed to get sources");
        }

        if attempts > 10 {
            println!("\nNo sources found after 10 seconds.");
            return;
        }
    }

    println!("\n\nFound {} source(s):", sources.len());
    for (i, source) in sources.iter().enumerate() {
        println!("  {}. {}", i + 1, source);
    }

    // Connect to the first source
    let source = sources[0].clone();
    println!("\nConnecting to: {}\n", source);

    let ndi_recv = Receiver::builder(source)
        .color(RecvColorFormat::UYVY_BGRA)
        .bandwidth(RecvBandwidth::Highest)
        .name("PTZ Control Example")
        .build(&ndi)
        .expect("Failed to create receiver");

    // Check if the source supports PTZ
    println!("Checking PTZ support...");
    if ndi_recv.ptz_is_supported() {
        println!("✓ PTZ is supported!");
    } else {
        println!("✗ PTZ is NOT supported by this source.");
        println!("  Note: PTZ requires an NDI-enabled camera with PTZ capabilities.");
        return;
    }

    println!("\nDemonstrating PTZ control for 30 seconds...\n");

    // Run PTZ demonstrations for 30 seconds
    let start = Instant::now();

    while start.elapsed() < Duration::from_secs(30) {
        match ndi_recv.capture_metadata(1000) {
            Ok(_) => {
                if ndi_recv.ptz_is_supported() {
                    println!("This source supports PTZ functionality. Moving to preset #3.");
                    if let Err(e) = ndi_recv.ptz_recall_preset(3, 1.0) {
                        eprintln!("Failed to recall PTZ preset: {}", e);
                    }
                }
            }
            Err(e) => eprintln!("Error during capture: {}", e),
        }
    }

    // The NDI receiver and finder will be destroyed automatically when they go out of scope
    // The Drop trait for NDI will take care of calling NDIlib_destroy()
}
