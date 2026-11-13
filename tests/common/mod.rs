//! Helpers shared by the integration tests.
//!
//! Each test binary uses a different subset, so unused helpers are expected.
#![allow(dead_code, unused_imports)]

use grafton_ndi::{capabilities, Error, Finder, FinderOptions, Ndi, Sender, Source, SourceAddress};
use std::time::{Duration, Instant};

#[cfg(all(target_os = "linux", not(ndi_link_static)))]
pub mod discovery_server;

#[path = "../../src/test_support.rs"]
mod test_support;
pub use test_support::{
    advanced_ndi, assert_expected_identity, hx_decoder, is_hx_placeholder, ndi, skip, unique_name,
    Requirement,
};

/// Discover the sender's actual advertised endpoint. The send API can return a
/// name without an address, and the first media port is not always 5961 (e.g.
/// a previous test's sockets may still occupy it). Query the local discovery
/// service explicitly, then retain its port while routing media over loopback.
pub fn loopback_source(ndi: &Ndi, sender: &Sender) -> grafton_ndi::Result<Source> {
    loopback_endpoint(ndi, sender.source()?.name())
}

/// The loopback endpoint of the source this process advertises as `name`; see
/// [`loopback_source`]. Some senders expose only their source, not themselves.
pub fn loopback_endpoint(ndi: &Ndi, name: &str) -> grafton_ndi::Result<Source> {
    let finder = Finder::new(ndi, &FinderOptions::new().extra_ips("127.0.0.1"))?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        for source in finder.current_sources()? {
            if source.name() == name {
                if let Some(SourceAddress::Ip(address)) = source.address() {
                    let port = address.rsplit(':').next().expect("SDK address port");
                    let loopback = SourceAddress::Ip(format!("127.0.0.1:{port}"));
                    return Ok(source.with_address(loopback));
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "local sender endpoint not discovered: {name}"
        );
        finder.wait_for_sources(Duration::from_millis(100))?;
    }
}
