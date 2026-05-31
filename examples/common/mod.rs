//! Shared discovery helpers for the examples.
//!
//! Every receiver/finder example needs the same two things: build a [`Finder`]
//! that sees local sources plus any IP hints passed on the command line, and
//! block until the first source shows up. Hand-rolling those in each example
//! let them drift — most notably whether `show_local_sources(true)` was set —
//! so they live here once instead.
//!
//! Pulled into an example with:
//!
//! ```ignore
//! #[path = "common/mod.rs"]
//! mod common;
//! ```
//!
//! Each example is compiled as its own crate and uses only the helper it
//! needs, so the unused one would otherwise trip `dead_code`.
#![allow(dead_code)]

use std::time::Duration;

use grafton_ndi::{Finder, FinderOptions, Result, Source, NDI};

/// Build a [`Finder`] that discovers local sources plus any extra IPs/subnets
/// supplied on the command line.
///
/// `extra_ips` are the positional, non-flag arguments each example collects
/// from `std::env::args` (e.g. `192.168.0.110` or `10.0.0.0/24`). Local
/// sources are always included so loopback senders are visible during testing.
pub fn finder_with_extra_ips(ndi: &NDI, extra_ips: &[&str]) -> Result<Finder> {
    let mut builder = FinderOptions::builder().show_local_sources(true);

    if !extra_ips.is_empty() {
        println!("Searching additional IPs/subnets:");
        for ip in extra_ips {
            println!("  - {ip}");
            builder = builder.extra_ips(*ip);
        }
        println!();
    }

    Finder::new(ndi, &builder.build())
}

/// Block until at least one source is discovered, re-checking every second.
///
/// `should_stop` is polled between waits so callers can bail out on a Ctrl-C
/// flag; when it returns `true` this returns `Ok(None)`. Pass `|| false` to
/// wait indefinitely.
///
/// Unlike [`Finder::find_sources`], which enumerates the *complete* set over a
/// fixed window, this returns the moment the *first* source appears — what the
/// examples want when they only need something to connect to.
pub fn wait_for_first_source(
    finder: &Finder,
    should_stop: impl Fn() -> bool,
) -> Result<Option<Vec<Source>>> {
    loop {
        if should_stop() {
            return Ok(None);
        }
        finder.wait_for_sources(Duration::from_secs(1))?;
        let sources = finder.current_sources()?;
        if !sources.is_empty() {
            return Ok(Some(sources));
        }
    }
}
