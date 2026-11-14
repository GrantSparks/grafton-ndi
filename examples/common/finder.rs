//! Finder construction shared by the discovery and receiver examples.

use grafton_ndi::{Finder, FinderOptions, Ndi, Result};

/// Build a [`Finder`] that discovers local sources plus any extra IP addresses
/// supplied on the command line.
///
/// `extra_ips` are the positional, non-flag arguments each example collects
/// from `std::env::args` (e.g. `192.168.0.110` or `10.0.0.0/24`). Local
/// sources are always included so loopback senders are visible during testing.
pub fn finder_with_extra_ips(ndi: &Ndi, extra_ips: &[&str]) -> Result<Finder> {
    let mut options = FinderOptions::new();

    if !extra_ips.is_empty() {
        println!("Searching additional IPs/subnets:");
        for ip in extra_ips {
            println!("  - {ip}");
        }
        options = options.extra_ips(extra_ips.join(","));
        println!();
    }

    Finder::new(ndi, &options)
}
