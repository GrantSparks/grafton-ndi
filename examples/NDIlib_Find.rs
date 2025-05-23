//! Example: Discovering NDI sources on the network.
//!
//! This example demonstrates how to use the Find API to discover NDI sources.
//! It continuously monitors for changes and displays all available sources.
//!
//! Run with: `cargo run --example NDIlib_Find`

use std::time::{Duration, Instant};
use grafton_ndi::{Error, Find, Finder, NDI};

fn main() -> Result<(), Error> {
    println!("NDI Source Discovery Example");
    println!("============================\n");
    
    // Initialize the NDI library
    let ndi = NDI::new()?;
    println!("NDI version: {}\n", NDI::version()?);
    
    // Configure the finder
    // - Don't show local sources (sources on this machine)
    // - Add a specific IP to search (useful for sources on different subnets)
    let finder = Finder::builder()
        .show_local_sources(false)
        .extra_ips("192.168.0.110")
        .build();
        
    // Create the finder instance
    let ndi_find = Find::new(&ndi, finder)?;

    // Monitor sources for 15 seconds
    let start = Instant::now();
    let run_duration = Duration::from_secs(15);
    
    println!("Monitoring for NDI sources for {} seconds...\n", run_duration.as_secs());
    
    while start.elapsed() < run_duration {
        // Wait for the source list to change (timeout: 5 seconds)
        // This is more efficient than polling as it only returns when
        // sources are added or removed
        if !ndi_find.wait_for_sources(5000) {
            println!("No changes detected ({}s remaining)", 
                (run_duration - start.elapsed()).as_secs());
            continue;
        }
        
        // Source list changed - get the updated list
        let sources = ndi_find.get_sources(0)?;
        
        // Display all discovered sources
        println!("\n📡 Network sources ({} found):", sources.len());
        println!("{:-<50}", "");
        
        if sources.is_empty() {
            println!("No sources found. Make sure NDI sources are running on the network.");
        } else {
            for (i, source) in sources.iter().enumerate() {
                println!("{}. {}", i + 1, source);
                
                // Show additional details about the source
                match &source.address {
                    grafton_ndi::SourceAddress::Url(url) => {
                        println!("   Type: NDI HX (URL: {})", url);
                    }
                    grafton_ndi::SourceAddress::Ip(ip) => {
                        println!("   Type: Standard NDI (IP: {})", ip);
                    }
                    grafton_ndi::SourceAddress::None => {
                        println!("   Type: Unknown");
                    }
                }
            }
        }
        println!("{:-<50}\n", "");
    }

    println!("\nDiscovery complete. Shutting down...");
    
    // The Find instance is automatically cleaned up when dropped
    // The NDI runtime is automatically cleaned up when the last NDI instance is dropped
    
    Ok(())
}
