use grafton_ndi::{Error, Find, Finder, NDI};
use std::time::{Duration, Instant};

fn main() -> Result<(), Error> {
    // Initialize the NDI library and ensure it's properly cleaned up
    if let Ok(ndi) = NDI::new() {
        // The IP address as a string
        let ip_address = "192.168.0.110";

        // Create an NDI finder to locate sources on the network
        let finder = Finder::new(false, None, Some(ip_address));
        let ndi_find = Find::new(&ndi, finder)?;

        // Run for 15 seconds
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(15) {
            // Wait up to 5 seconds to check for new sources to be added or removed
            println!("Waiting for sources...");
            if !ndi_find.wait_for_sources(5000) {
                println!("No change to the sources found.");
                continue;
            }

            // Get the updated list of sources
            println!("Getting sources...");
            let sources = ndi_find.get_sources(5000)?;
            println!("Sources retrieved.");

            // Display all the sources
            println!("Network sources ({} found).", sources.len());
            for (i, source) in sources.iter().enumerate() {
                println!("{}. {}", i + 1, source);
            }
        }

        // The ndi_find will be destroyed automatically when it goes out of scope
        // The NDI library will be destroyed automatically when `ndi` goes out of scope
    } else {
        return Err(Error::InitializationFailed(
            "Failed to initialize NDI library".into(),
        ));
    }

    Ok(())
}
