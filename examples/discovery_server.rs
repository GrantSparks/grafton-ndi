//! Example: advertising to, and listing, an NDI Discovery Server.
//!
//! A Discovery Server keeps a registry of senders and receivers for monitoring
//! and control. This example registers a sender of its own with the server,
//! then prints the senders and receivers the server reports as they change.
//!
//! It needs a Discovery Server (the SDK ships one as `ndi-discovery-server`)
//! and an NDI 6.4.0 or later runtime. The server does not have to be running
//! yet: advertisers and listeners connect in the background, a few seconds
//! after it appears.
//!
//! Run with: `cargo run --example discovery_server -- 127.0.0.1:5959`
//!
//! Without an argument the Discovery Server from the NDI configuration is
//! used, and the example fails if that names none.

use grafton_ndi::{
    discovery::{
        AdvertiserOptions, ListenerOptions, RecvListener, SendAdvertisement, SendAdvertiser,
        SendListener,
    },
    Error, Sender, SenderOptions, NDI,
};

use std::{
    env,
    time::{Duration, Instant},
};

/// How long the example watches the registry.
const WATCH: Duration = Duration::from_secs(30);

fn main() -> Result<(), Error> {
    let server = env::args().nth(1);
    let mut advertiser_options = AdvertiserOptions::default();
    let mut listener_options = ListenerOptions::default();
    if let Some(server) = &server {
        advertiser_options = advertiser_options.servers(server.as_str());
        listener_options = listener_options.server(server.as_str())?;
    }

    let ndi = NDI::new()?;

    // Register a sender. The advertiser keeps it registered, and alive, until
    // the registration is removed or the advertiser is dropped.
    let sender = Sender::new(
        &ndi,
        &SenderOptions::builder("Discovery Server Example").build(),
    )?;
    let mut advertiser = SendAdvertiser::new(&ndi, &advertiser_options)?;
    let registration =
        advertiser.register(&sender, SendAdvertisement::default().allow_monitoring(true))?;
    println!("Registered {}", sender.source()?);

    let mut senders = SendListener::new(&ndi, &listener_options)?;
    let mut receivers = RecvListener::new(&ndi, &listener_options)?;
    println!(
        "Listening on {}",
        senders
            .server_url()
            .unwrap_or_else(|| "the configured Discovery Server".into())
    );

    // Creating the listeners did not wait for the server. They connect in the
    // background, and their snapshots are empty until they do.
    let mut connected = false;
    println!("Waiting for the Discovery Server...");

    let deadline = Instant::now() + WATCH;
    while Instant::now() < deadline {
        // Print the registry whenever either side of it changes.
        let senders_changed = senders.wait_for_changes(Duration::from_secs(1))?;
        let receivers_changed = receivers.wait_for_changes(Duration::ZERO)?;
        if senders.is_connected() != connected {
            connected = !connected;
            println!(
                "{}",
                if connected {
                    "Connected."
                } else {
                    "Connection lost."
                }
            );
        }
        if !senders_changed && !receivers_changed {
            continue;
        }
        println!("\nSenders:");
        for sender in senders.snapshot()? {
            println!(
                "  {} at {}:{}",
                sender.name().unwrap_or("(unnamed)"),
                sender.address().unwrap_or("?"),
                sender.port().map_or("?".into(), |port| port.to_string()),
            );
        }
        println!("Receivers:");
        for receiver in receivers.snapshot()? {
            println!(
                "  {} at {}, accepting {:?}",
                receiver.name().unwrap_or("(unnamed)"),
                receiver.address().unwrap_or("?"),
                receiver.commands(),
            );
        }
    }

    // Only the advertiser that issued a registration can end it.
    advertiser.unregister(registration)?;
    Ok(())
}
