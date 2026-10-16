use grafton_ndi::{Receiver, ReceiverBandwidth, ReceiverOptions, NDI};

use std::{
    env,
    io::{self, Write},
    time::Duration,
};

#[path = "common/mod.rs"]
mod common;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse command line arguments
    let args: Vec<String> = env::args().collect();
    let mut source_name = None;
    let mut extra_ips = Vec::new();

    let mut i = 1;
    while i < args.len() {
        if args[i] == "--source" && i + 1 < args.len() {
            source_name = Some(args[i + 1].clone());
            i += 2;
        } else if !args[i].starts_with("--") {
            extra_ips.push(args[i].as_str());
            i += 1;
        } else {
            i += 1;
        }
    }

    // Initialize NDI
    let ndi = NDI::new()?;
    println!("NDI initialized successfully");

    // Find sources
    let finder = common::finder_with_extra_ips(&ndi, &extra_ips)?;

    println!("Looking for NDI sources...");
    let sources = finder.find_sources(Duration::from_secs(5))?;

    if sources.is_empty() {
        println!("No NDI sources found on the network");
        return Ok(());
    }

    // Select source
    let source = if let Some(name) = source_name {
        sources
            .into_iter()
            .find(|s| s.name.contains(&name))
            .ok_or_else(|| format!("Source '{name}' not found"))?
    } else {
        println!("\nAvailable sources:");
        for (i, source) in sources.iter().enumerate() {
            println!("  {i}: {source}");
        }
        println!("\nUsing first source: {}", sources[0]);
        sources[0].clone()
    };

    // Create receiver with metadata-only bandwidth to focus on status changes
    let options = ReceiverOptions::builder(source.clone())
        .bandwidth(ReceiverBandwidth::MetadataOnly)
        .build();
    let receiver = Receiver::new(&ndi, &options)?;

    println!("\nMonitoring status changes for: {source}");
    println!("Press Ctrl+C to exit\n");

    // Monitor status changes
    loop {
        if let Some(status) = receiver.poll_status_change(Duration::from_secs(1))? {
            println!(
                "[Status Change] Connections: {} (source capabilities may also have changed)",
                status.connections
            );
        } else {
            // Timeout - could show a heartbeat here
            print!(".");
            io::stdout().flush()?;
        }
    }
}
