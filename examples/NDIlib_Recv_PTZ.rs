use std::{
    ffi::CString,
    time::{Duration, Instant},
};

use grafton_ndi::*;

fn main() {
    if let Ok(ndi) = NDI::new() {
        println!("NDI initialized successfully.");

        // Create a CString for the IP address and convert to &str
        let ip_address = CString::new("192.168.0.110").expect("CString::new failed");
        let ip_str = ip_address
            .to_str()
            .expect("CString to str conversion failed");

        // Create an NDI finder to locate sources on the network
        let finder = Finder::new(false, None, Some(ip_str));
        let ndi_find = Find::new(&ndi, finder).expect("Failed to create NDI find instance");

        // We wait until there is at least one source on the network
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
        // We tell it that we prefer YCbCr video since it is more efficient for us. If the source has an alpha channel
        // it will still be provided in BGRA
        let source_to_connect_to = sources[0].clone();
        let receiver = Receiver::new(
            source_to_connect_to,
            RecvColorFormat::UYVY_BGRA,
            RecvBandwidth::Highest,
            true,
            Some("Example PTZ Receiver".to_string()),
        );

        let ndi_recv = Recv::new(&ndi, receiver).expect("Failed to create NDI recv instance");

        // Run for 15 seconds
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(15) {
            println!("Waiting for a frame...");
            // Receive something
            match ndi_recv.capture(1000) {
                Ok(FrameType::None) => {
                    println!("Received nothing");
                }
                Ok(FrameType::Video(_)) => {
                    println!("Received a video frame");
                    // Handle video frame
                }
                Ok(FrameType::Audio(_)) => {
                    println!("Received an audio frame");
                    // Handle audio frame
                }
                Ok(FrameType::Metadata(_)) => {
                    println!("Received a metadata frame");
                    // Handle metadata frame
                }
                Ok(FrameType::StatusChange) => {
                    println!("Received a status change frame");
                    // Handle status change frame
                }
                Err(_) => {
                    if ndi_recv.ptz_is_supported() {
                        println!("This source supports PTZ functionality. Moving to preset #3.");
                        ndi_recv.ptz_recall_preset(3, 1.0);
                    }
                }
            }
        }

        // The NDI receiver will be destroyed automatically when it goes out of scope
    } else {
        println!("Cannot run NDI. Most likely because the CPU is not sufficient (see SDK documentation).");
    }

    // The Drop trait for NDI will take care of calling NDIlib_destroy()
}
