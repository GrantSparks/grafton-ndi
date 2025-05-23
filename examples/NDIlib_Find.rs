//! Example: Discovering NDI sources on the network.
//!
//! This example demonstrates how to use the Find API to discover NDI sources.
//! It waits for source changes and displays all available sources for 1 minute.
//!
//! Run with: `cargo run --example NDIlib_Find`

use grafton_ndi::{Error, Finder, FinderOptions, NDI};
use std::time::{Duration, Instant};

// Optional: Configure for specific environments
fn create_finder_options() -> FinderOptions {
    // For testing in specific network environments, you can customize:
    // - show_local_sources(false) to hide sources on this machine
    // - extra_ips("192.168.0.110") to search specific subnets
    FinderOptions::builder().build()
}

fn main() -> Result<(), Error> {
    // Initialize the NDI library
    let ndi = NDI::new()?;

    // Create the finder instance
    let finder_options = create_finder_options();
    let finder = Finder::new(&ndi, &finder_options)?;

    // Run for one minute
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(60) {
        // Wait up to 5 seconds for sources to be added or removed
        if !finder.wait_for_sources(5000) {
            println!("No change to the sources found.");
            continue;
        }

        // Get the updated list of sources
        let sources = finder.get_sources(0)?;

        // Display all the sources
        println!("Network sources ({} found).", sources.len());
        for (i, source) in sources.iter().enumerate() {
            println!("{}. {}", i + 1, source.name);
        }
    }

    Ok(())
}
