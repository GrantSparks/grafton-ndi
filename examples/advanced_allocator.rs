//! Example: receiving frames into memory the application supplies.
//!
//! This example demonstrates:
//! - Enabling custom allocation with an [`AllocatorService`]
//! - Supplying padded video lines from a policy closure
//! - Watching what the SDK holds
//!
//! Needs the NDI Advanced SDK at build time and an Advanced runtime.
//!
//! Run with: `cargo run --features advanced --example advanced_allocator`
//!
//! Optional arguments:
//! - IP address to search: `cargo run --features advanced --example advanced_allocator -- 192.168.0.110`

use std::{env, time::Duration};

use grafton_ndi::{
    advanced::{AllocationBuffer, AllocationRequest, AllocatorLimits, AllocatorService},
    Error, Ndi, Receiver, ReceiverOptions,
};

#[path = "common/mod.rs"]
mod common;

fn main() -> Result<(), Error> {
    let args: Vec<String> = env::args().collect();
    let extra_ips: Vec<&str> = args[1..].iter().map(|s| s.as_str()).collect();

    let ndi = Ndi::new()?;
    let finder = common::finder_with_extra_ips(&ndi, &extra_ips)?;
    let Some(source) = finder
        .find_sources(Duration::from_secs(5))?
        .into_iter()
        .next()
    else {
        println!("No NDI sources found!");
        return Ok(());
    };
    println!("Receiving {source}");

    let receiver = Receiver::new(
        &ndi,
        &ReceiverOptions::new(source).name("Allocator Example"),
    )?;
    // Custom allocation keeps the NDI runtime initialized until the process exits.
    let service = AllocatorService::new(ndi.process_lifetime(), AllocatorLimits::default())?;
    let mut receiver = service.attach(receiver)?;

    // Called on SDK threads: pad every video line to a multiple of 256 bytes.
    receiver.set_video_allocator(|request: AllocationRequest| {
        let stride = request.stride().next_multiple_of(256);
        AllocationBuffer::zeroed(request.with_stride(stride)?)
    })?;

    for _ in 0..120 {
        if let Some(frame) = receiver
            .receiver()
            .video()
            .capture(Duration::from_secs(1))?
        {
            let usage = service.usage();
            println!(
                "{}x{} frame; the SDK holds {} buffers, {} bytes",
                frame.width(),
                frame.height(),
                usage.retained_allocations(),
                usage.retained_bytes()
            );
        }
    }

    // Back to the SDK's own memory; the receiver is ordinary again.
    let receiver = receiver.into_receiver();
    println!("Still connected: {}", receiver.is_connected());
    Ok(())
}
