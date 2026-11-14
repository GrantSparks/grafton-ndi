//! Example: monitoring a receiver's connection status.
//!
//! Connects a metadata-only receiver to the first source found, or to the
//! first one whose name contains the `--source` text, and prints every status
//! change it reports: the number of connections and whether the source
//! supports PTZ. It runs until interrupted.
//!
//! Run with: `cargo run --example status_monitor`
//!
//! Optional arguments:
//! - IP addresses to search: `cargo run --example status_monitor -- 192.168.0.100`
//! - Source to monitor: `cargo run --example status_monitor -- --source "Camera 1"`

use grafton_ndi::{Ndi, Receiver, ReceiverBandwidth, ReceiverEvent, ReceiverOptions};

use std::{
    env,
    io::{self, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

#[path = "common/arguments.rs"]
mod arguments;
#[path = "common/finder.rs"]
mod finder;

use arguments::option_value;
use finder::finder_with_extra_ips;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut source_name: Option<String> = None;
    let mut extra_ips = Vec::new();
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--source" => source_name = Some(option_value(&mut arguments, &argument)?),
            flag if flag.starts_with('-') => return Err(format!("unknown option: {flag}").into()),
            _ => extra_ips.push(argument),
        }
    }
    let extra_ips: Vec<&str> = extra_ips.iter().map(String::as_str).collect();
    let stop = Arc::new(AtomicBool::new(false));
    let signal = stop.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;

    // Initialize NDI
    let ndi = Ndi::new()?;
    println!("NDI initialized successfully");

    // Find sources
    let finder = finder_with_extra_ips(&ndi, &extra_ips)?;

    println!("Looking for NDI sources...");
    let source = if let Some(name) = source_name {
        finder
            .find_source(Duration::from_secs(5), |source| {
                source.name().contains(&name)
            })?
            .ok_or_else(|| format!("Source '{name}' not found"))?
    } else {
        let sources = finder.find_sources(Duration::from_secs(5))?;
        let Some(first) = sources.first() else {
            println!("No NDI sources found on the network");
            return Ok(());
        };
        println!("\nAvailable sources:");
        for (i, source) in sources.iter().enumerate() {
            println!("  {i}: {source}");
        }
        println!("\nUsing first source: {first}");
        first.clone()
    };

    // Create receiver with metadata-only bandwidth to focus on status changes
    let options = ReceiverOptions::new(source.clone()).bandwidth(ReceiverBandwidth::MetadataOnly);
    let receiver = Receiver::new(&ndi, &options)?;

    println!("\nMonitoring status changes for: {source}");
    println!("Press Ctrl+C to exit\n");

    // Notifications consumed by concurrent media capture are retained here.
    // Multiple changes may coalesce; refresh capabilities on each notification.
    // Monitor status changes
    while !stop.load(Ordering::Relaxed) {
        match receiver.poll_event(Duration::from_secs(1))? {
            Some(ReceiverEvent::StatusChanged(status)) => {
                println!(
                    "[Status Change] Connections: {} (source capabilities may also have changed)",
                    status.connections()
                );
                println!("PTZ supported: {}", receiver.ptz().is_supported());
            }
            Some(_) => {}
            None => {
                // Timeout - could show a heartbeat here
                print!(".");
                io::stdout().flush()?;
            }
        }
    }
    Ok(())
}
