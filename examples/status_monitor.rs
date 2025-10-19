use std::{
    env,
    io::{self, Write},
};

use grafton_ndi::{Finder, FinderOptions, ReceiverBandwidth, ReceiverOptions, NDI};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse command line arguments
    let args: Vec<String> = env::args().collect();
    let source_name = if args.len() > 2 && args[1] == "--source" {
        Some(args[2].clone())
    } else {
        None
    };

    // Initialize NDI
    let ndi = NDI::new()?;
    println!("NDI initialized successfully");

    // Find sources
    let finder_options = FinderOptions::builder().show_local_sources(true).build();
    let finder = Finder::new(&ndi, &finder_options)?;

    println!("Looking for NDI sources...");
    finder.wait_for_sources(5000);
    let sources = finder.get_sources(0)?;

    if sources.is_empty() {
        println!("No NDI sources found on the network");
        return Ok(());
    }

    // Select source
    let source = if let Some(name) = source_name {
        sources
            .into_iter()
            .find(|s| s.name.contains(&name))
            .ok_or_else(|| format!("Source '{}' not found", name))?
    } else {
        println!("\nAvailable sources:");
        for (i, source) in sources.iter().enumerate() {
            println!("  {}: {}", i, source);
        }
        println!("\nUsing first source: {}", sources[0]);
        sources[0].clone()
    };

    // Create receiver with metadata-only bandwidth to focus on status changes
    let receiver = ReceiverOptions::builder(source.clone())
        .bandwidth(ReceiverBandwidth::MetadataOnly)
        .build(&ndi)?;

    println!("\nMonitoring status changes for: {}", source);
    println!("Press Ctrl+C to exit\n");

    // Monitor status changes
    loop {
        if let Some(status) = receiver.poll_status_change(1000) {
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
                print!("| Connections: {} ", connections);
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
