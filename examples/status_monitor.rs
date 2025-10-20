use grafton_ndi::{Finder, FinderOptions, Receiver, ReceiverBandwidth, ReceiverOptions, NDI};

use std::{
    env,
    io::{self, Write},
    time::Duration,
};

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
    let mut builder = FinderOptions::builder().show_local_sources(true);

    if !extra_ips.is_empty() {
        println!("\nSearching additional IPs/subnets:");
        for ip in &extra_ips {
            println!("  - {}", ip);
            builder = builder.extra_ips(*ip);
        }
    }

    let finder_options = builder.build();
    let finder = Finder::new(&ndi, &finder_options)?;

    println!("Looking for NDI sources...");
    finder.wait_for_sources(Duration::from_secs(5))?;
    let sources = finder.sources(Duration::ZERO)?;

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
            print!("[Status Change] ");

            if let Some(tally) = status.tally {
                print!("Tally: ");
                if tally.on_program {
                    print!("ON-AIR ");
                }
                if tally.on_preview {
                    print!("PREVIEW ");
                }
                if !tally.on_program && !tally.on_preview {
                    print!("OFF ");
                }
            }

            if let Some(connections) = status.connections {
                print!("| Connections: {connections} ");
            }

            if status.other {
                print!("| Other changes detected");
            }

            println!();
        } else {
            // Timeout - could show a heartbeat here
            print!(".");
            io::stdout().flush()?;
        }
    }
}
