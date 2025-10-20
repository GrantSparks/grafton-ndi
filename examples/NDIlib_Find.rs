//! Example: Discovering NDI sources on the network.
//!
//! This example demonstrates how to use the Find API to discover NDI sources.
//! It waits for source changes and displays all available sources for 1 minute.
//!
//! This is based on the NDIlib_Find example from the NDI SDK.
//!
//! Run with: `cargo run --example NDIlib_Find`
//!
//! Optional arguments:
//! - IP address or subnet to search: `cargo run --example NDIlib_Find -- 192.168.0.100`
//! - Multiple IPs: `cargo run --example NDIlib_Find -- 192.168.0.100 10.0.0.0/24`
//!
//! Troubleshooting:
//! - If no sources are found, try adding specific IP addresses with extra_ips()
//! - Ensure NDI sources are running on your network
//! - Check firewall settings (NDI uses TCP port 5353 for discovery)

use std::{
    env,
    time::{Duration, Instant},
};

use grafton_ndi::{Error, Finder, FinderOptions, NDI};

fn main() -> Result<(), Error> {
    // Parse command line arguments for extra IPs
    let args: Vec<String> = env::args().collect();
    let extra_ips: Vec<&str> = args[1..].iter().map(|s| s.as_str()).collect();
    println!("NDI Source Discovery Example");
    println!("============================\n");

    // Initialize the NDI library
    let ndi = NDI::new()?;
    println!("NDI initialized successfully\n");

    // Create finder options
    let mut builder = FinderOptions::builder().show_local_sources(true); // Include sources on this machine

    // Add any command line IPs
    if !extra_ips.is_empty() {
        println!("Searching additional IPs/subnets:");
        for ip in &extra_ips {
            println!("  - {}", ip);
            builder = builder.extra_ips(*ip);
        }
        println!();
    }

    let finder_options = builder.build();

    // Create the finder instance
    let finder = Finder::new(&ndi, &finder_options)?;
    println!("Searching for NDI sources...");
    println!("(Will run for 60 seconds)\n");

    // Check for initial sources immediately
    let initial_sources = finder.sources(Duration::ZERO)?;
    if !initial_sources.is_empty() {
        println!("Initial sources found ({}):", initial_sources.len());
        for (i, source) in initial_sources.iter().enumerate() {
            println!("  {}. {}", i + 1, source);
        }
        println!();
    }

    // Run for one minute
    let start = Instant::now();
    let mut last_count = initial_sources.len();

    while start.elapsed() < Duration::from_secs(60) {
        // Wait up to 5 seconds for sources to be added or removed
        if !finder.wait_for_sources(Duration::from_secs(5))? {
            // No changes detected
            let elapsed = start.elapsed().as_secs();
            if elapsed % 10 == 0 {
                // Print status every 10 seconds
                println!(
                    "[{:02}s] No change to sources (still {} found)",
                    elapsed, last_count
                );
            }
            continue;
        }

        // Get the updated list of sources
        let sources = finder.sources(Duration::ZERO)?;
        let elapsed = start.elapsed().as_secs();

        // Display changes
        if sources.len() != last_count {
            println!("\n[{:02}s] Source list changed!", elapsed);
        }

        println!("Network sources ({} found):", sources.len());
        for (i, source) in sources.iter().enumerate() {
            println!("  {}. {}", i + 1, source);
        }
        println!();

        last_count = sources.len();
    }

    println!("Discovery complete after 60 seconds.");
    println!("Final source count: {}", last_count);

    Ok(())
}
