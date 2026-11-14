//! Discover a sender and route its advertised endpoint over loopback.

use grafton_ndi::{Finder, FinderOptions, Ndi, Sender, Source, SourceAddress};
use std::{
    net::Ipv4Addr,
    time::{Duration, Instant},
};

/// Discover the sender's actual advertised endpoint. The send API can return a
/// name without an address, and the first media port is not always 5961 (e.g.
/// a previous test's sockets may still occupy it). Query the local discovery
/// service explicitly, then retain its port while routing media over loopback.
pub fn loopback_source(ndi: &Ndi, sender: &Sender) -> grafton_ndi::Result<Source> {
    loopback_endpoint(ndi, sender.source()?.name())
}

/// The loopback endpoint, `127.0.0.1:<port>`, of the source this process
/// advertises as `name`; see [`loopback_source`]. Some senders expose only
/// their source, not themselves.
pub fn loopback_endpoint(ndi: &Ndi, name: &str) -> grafton_ndi::Result<Source> {
    let (source, port) = advertised_endpoint(ndi, name)?;
    let loopback = SourceAddress::Ip(format!("{}:{port}", Ipv4Addr::LOCALHOST));
    Ok(source.with_address(loopback))
}

/// The source this process advertises as `name`, as the SDK's discovery
/// reports it, and the port it is reached on. [`loopback_endpoint`] is what
/// a receiver is given; this is for a test that needs the port itself.
pub fn advertised_endpoint(ndi: &Ndi, name: &str) -> grafton_ndi::Result<(Source, u16)> {
    advertised_endpoint_by(&loopback_finder(ndi)?, name)
}

/// A finder that also asks this machine's loopback address for sources, as
/// [`advertised_endpoint`] looks senders up.
pub fn loopback_finder(ndi: &Ndi) -> grafton_ndi::Result<Finder> {
    Finder::new(ndi, &FinderOptions::new().extra_ips("127.0.0.1"))
}

/// [`advertised_endpoint`], through `finder`, which the caller keeps.
pub fn advertised_endpoint_by(finder: &Finder, name: &str) -> grafton_ndi::Result<(Source, u16)> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        for source in finder.current_sources()? {
            if source.name() == name {
                if let Some(SourceAddress::Ip(address)) = source.address() {
                    let port = address
                        .rsplit(':')
                        .next()
                        .and_then(|port| port.parse().ok())
                        .unwrap_or_else(|| panic!("the SDK advertised {name} at {address}"));
                    return Ok((source, port));
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
