use std::time::{Duration, Instant};

use grafton_ndi::{NDIlib, NDIlibFindInstance};

fn main() {
    if !NDIlib::initialize() {
        eprintln!("Failed to initialize NDI library.");
        return;
    }
    println!("NDI library initialized successfully.");

    // Generate the IP addresses within the additional range to include in our search
    let extra_ips: Vec<String> = (107..=111).map(|i| format!("192.168.0.{}", i)).collect();
    let extra_ips_cstr = extra_ips.join(",");

    println!("Creating NDI find instance...");
    let ndi_find = NDIlibFindInstance::new(false, None, Some(&extra_ips_cstr));
    if ndi_find.is_initialized() {
        println!("NDI find instance created successfully.");
    } else {
        eprintln!("Failed to create NDI find instance.");
        return;
    }

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(60) {
        println!("Waiting for sources (timeout 5000 ms)...");
        if !ndi_find.wait_for_sources(5000) {
            println!("No change to the sources found.");
            continue;
        }
        println!("Sources have changed.");

        let sources = ndi_find.get_sources(5000);
        println!("Network sources ({} found).", sources.len());

        for (i, source) in sources.iter().enumerate() {
            println!("{}. {}", i + 1, source.name);
        }
    }

    println!("Destroying NDI library...");
    NDIlib::destroy();
    println!("NDI library destroyed. Program finished.");
}
