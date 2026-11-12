//! Example: remote control of a source that offers KVM.
//!
//! This example demonstrates:
//! - Starting a [`KvmSession`](grafton_ndi::advanced::KvmSession) on a receiver
//! - Moving the pointer and clicking
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

    // Click once in the middle of the source's screen.
    session.move_pointer(0.5, 0.5)?;
    session.mouse_down(MouseButton::Left)?;
    session.mouse_up(MouseButton::Left)?;

    match session.release_all() {
        Ok(()) => println!("Clicked, and nothing is left pressed"),
        Err(error) => println!("Something may still be pressed: {error}"),
    }
    Ok(())
}
