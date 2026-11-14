//! Example: switching what receivers see with an NDI routing source.
//!
//! A router is an NDI source with no media of its own. Receivers connect to it
//! by name and are redirected to whichever source it currently routes to, so
//! this process changes what they show without touching a frame.
//!
//! The example advertises a router and routes it to each discovered source in
//! turn, a few seconds apiece, then clears the route.
//!
//! Run with: `cargo run --example routing`
//!
//! Optional arguments:
//! - The router's name: `cargo run --example routing -- --name Program`
//! - IP addresses or subnets to search: `cargo run --example routing -- 192.168.0.100`

use grafton_ndi::{
    routing::{Router, RouterOptions},
    Error, Ndi,
};

use std::{env, thread, time::Duration};

#[path = "common/finder.rs"]
mod finder;

use finder::finder_with_extra_ips;

/// How long each source stays routed.
const DWELL: Duration = Duration::from_secs(5);

fn main() -> Result<(), Error> {
    let mut name = String::from("Routing Example");
    let mut extra_ips = Vec::new();
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        if argument == "--name" {
            name = arguments
                .next()
                .ok_or_else(|| Error::InvalidInput("--name needs a value".into()))?;
        } else {
            extra_ips.push(argument);
        }
    }
    let extra_ips: Vec<&str> = extra_ips.iter().map(String::as_str).collect();

    let ndi = Ndi::new()?;
    let mut router = Router::new(&ndi, &RouterOptions::new(name))?;
    println!("Advertising router: {}", router.source()?);

    let finder = finder_with_extra_ips(&ndi, &extra_ips)?;
    let own = router.source()?;
    let sources: Vec<_> = finder
        .find_sources(Duration::from_secs(5))?
        .into_iter()
        .filter(|source| source.name() != own.name())
        .collect();
    if sources.is_empty() {
        println!("No sources found to route to.");
        return Ok(());
    }

    for source in &sources {
        router.change(source)?;
        println!(
            "Routing to {source} ({} receiver(s) connected to the router)",
            router.connection_count(Duration::ZERO)?
        );
        thread::sleep(DWELL);
    }

    router.clear()?;
    println!("Route cleared.");
    Ok(())
}
