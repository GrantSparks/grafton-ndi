//! Example: remote control of a source that offers KVM.
//!
//! This example demonstrates:
//! - Starting a [`KvmSession`](grafton_ndi::advanced::KvmSession) on a receiver
//! - Capturing video through the session while a drag stays held
//! - Releasing everything that was pressed
//!
//! Needs the NDI Advanced SDK at build time and an Advanced runtime.
//!
//! Run with: `cargo run --features advanced --example advanced_kvm`
//!
//! Optional arguments:
//! - IP address to search: `cargo run --features advanced --example advanced_kvm -- 192.168.0.110`

use std::{env, thread, time::Duration};

use grafton_ndi::{advanced::MouseButton, Error, Ndi, Receiver, ReceiverOptions};

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
    println!("Connecting to {source}");

    let mut receiver = Receiver::new(&ndi, &ReceiverOptions::new(source).name("KVM Example"))?;
    // Give the connection a moment to report what the source supports.
    thread::sleep(Duration::from_secs(2));

    let mut session = receiver.kvm()?;
    if !session.is_supported() {
        println!("This source does not offer remote control");
        return Ok(());
    }

    // Drag from the left of the source's screen to the right. The button
    // stays down across the frames captured through the session's receiver.
    session.move_pointer(0.25, 0.5)?;
    session.mouse_down(MouseButton::Left)?;
    for step in 1..=5u8 {
        // The frame stays borrowed from the receiver while input is sent.
        let frame = session
            .receiver()
            .video()
            .capture_ref(Duration::from_secs(1))?;
        if let Some(frame) = &frame {
            println!("Frame {}x{} while dragging", frame.width(), frame.height());
        }
        session.move_pointer(0.25 + 0.1 * f32::from(step), 0.5)?;
    }
    session.mouse_up(MouseButton::Left)?;

    match session.release_all() {
        Ok(()) => println!("Dragged, and nothing is left pressed"),
        Err(error) => println!("Something may still be pressed: {error}"),
    }
    Ok(())
}
