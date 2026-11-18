//! Finder construction shared by the discovery and receiver examples.

use std::{io, net::IpAddr};

use grafton_ndi::{Finder, FinderOptions, Ndi, Result};

/// Build a [`Finder`] that discovers local sources plus any extra IP addresses
/// supplied on the command line.
///
/// `extra_ips` are the positional, non-flag arguments each example collects
/// from `std::env::args`, such as `192.168.0.110` or `10.0.0.8`.
/// Local sources are always included so loopback senders are visible during testing.
pub fn finder_with_extra_ips(ndi: &Ndi, extra_ips: &[&str]) -> Result<Finder> {
    let mut options = FinderOptions::new();

    if !extra_ips.is_empty() {
        for ip in extra_ips {
            ip.parse::<IpAddr>().map_err(|error| {
                io::Error::new(io::ErrorKind::InvalidInput, format!("{ip:?}: {error}"))
            })?;
        }
        println!("Searching additional IP addresses:");
        for ip in extra_ips {
            println!("  - {ip}");
        }
        options = options.extra_ips(extra_ips.iter().copied());
        println!();
    }

    Finder::new(ndi, &options)
}
