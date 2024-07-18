use std::time::{Duration, Instant};

use grafton_ndi::{Find, Finder, NDI};

fn main() -> Result<(), &'static str> {
    // Initialize the NDI library and ensure it's properly cleaned up
    if let Ok(_ndi) = NDI::new() {
        // Create an NDI finder to locate sources on the network
        // let finder = Finder::default();
        let finder = Finder::new(false, None, Some("192.168.0.110"));
        let ndi_find = Find::new(finder)?;

        // Run for one minute
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(60) {
            // Wait up to 5 seconds to check for new sources to be added or removed
            if !ndi_find.wait_for_sources(5000) {
                println!("No change to the sources found.");
                continue;
            }

            // Get the updated list of sources
            let sources = ndi_find.get_sources(5000);

            // Display all the sources
            println!("Network sources ({} found).", sources.len());
            for (i, source) in sources.iter().enumerate() {
                println!("{}. {}", i + 1, source.name);
            }
        }

        // The NDI finder will be destroyed automatically when it goes out of scope
        // The NDI library will be destroyed automatically when `_ndi` goes out of scope
    } else {
        return Err("Failed to initialize NDI library");
    }

    Ok(())
}
