use std::time::{Duration, Instant};

use grafton_ndi::{Find, Finder, FrameType, Receiver, Recv, RecvBandwidth, RecvColorFormat, NDI};

fn main() {
    if let Ok(ndi) = NDI::new() {
        // Create an NDI finder to locate sources on the network
        let finder = Finder::new(false, None, Some("192.168.0.110"));
        let ndi_find = Find::new(&ndi, finder).expect("Failed to create NDI find instance");

        // Wait until there is at least one source on the network
        let mut sources = vec![];
        while sources.is_empty() {
            if ndi_find.wait_for_sources(1000) {
                sources = ndi_find.get_sources(1000).expect("Failed to get sources");
            }
        }

        // We need at least one source
        if sources.is_empty() {
            println!("No sources found.");
            return;
        }

        // We now have at least one source, so we create a receiver to look at it.
        let source_to_connect_to = sources[0].clone();
        let receiver = Receiver::new(
            source_to_connect_to,
            RecvColorFormat::UYVY_BGRA,
            RecvBandwidth::Highest,
            true,
            Some("Example PTZ Receiver".to_string()),
        );

        let mut ndi_recv = Recv::new(&ndi, receiver).expect("Failed to create NDI recv instance");

        // Run for 30 seconds
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(30) {
            if let Ok(FrameType::StatusChange) = ndi_recv.capture(1000) {
                if ndi_recv.ptz_is_supported() {
                    println!("This source supports PTZ functionality. Moving to preset #3.");
                    ndi_recv.ptz_recall_preset(3, 1.0);
                }
            }
        }

        // The NDI receiver and finder will be destroyed automatically when they go out of scope
    } else {
        println!("Cannot run NDI. Most likely because the CPU is not sufficient (see SDK documentation).");
    }
    // The Drop trait for NDI will take care of calling NDIlib_destroy()
}
