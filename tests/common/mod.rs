//! Helpers shared by the integration tests.
//!
//! Each test binary uses a different subset, so unused helpers are expected.
#![allow(dead_code, unused_imports)]

#[cfg(feature = "file-io")]
use grafton_ndi::file;
use grafton_ndi::{capabilities, Error, Finder, FinderOptions, Ndi, Sender, Source, SourceAddress};
use std::{
    net::Ipv4Addr,
    time::{Duration, Instant},
};

#[cfg(all(target_os = "linux", not(ndi_link_static)))]
pub mod discovery_server;

#[path = "../../src/test_support.rs"]
mod test_support;
#[cfg(feature = "file-io")]
pub use test_support::file_api;
pub use test_support::{
    advanced_ndi, assert_expected_identity, hx_decoder, is_hx_placeholder, ndi, skip, unique_name,
    unsupported, Requirement,
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

/// The port of an endpoint that names an IP address and port, such as those
/// [`loopback_endpoint`] returns.
pub fn port_of(source: &Source) -> Option<u16> {
    match source.address()? {
        SourceAddress::Ip(address) => address.rsplit(':').next()?.parse().ok(),
        _ => None,
    }
}

/// What the operating system's TCP table holds for `port`, for the message of
/// a test whose receiver did not connect to a sender on it: `None` except on
/// Windows.
///
/// On Windows it is the rows of `netstat -ano -p TCP` whose local or remote
/// port is `port` or which this process owns, then `Get-NetTCPConnection`'s
/// rows for the port with the time each socket was created. Together they show
/// how many sockets listen on the port and whether the receiver has a
/// connection to it, and in what state. It takes a second or two, so a test
/// collects it only when it is about to fail or is diagnosing a failure.
pub fn socket_table(port: u16) -> Option<String> {
    #[cfg(windows)]
    {
        Some(windows_socket_table(port))
    }
    #[cfg(not(windows))]
    {
        let _ = port;
        None
    }
}

#[cfg(windows)]
fn windows_socket_table(port: u16) -> String {
    use std::{fmt::Write, process::Command};

    let pid = std::process::id().to_string();
    let suffix = format!(":{port}");
    let mut table = format!("netstat -ano -p TCP, rows for port {port} or process {pid}:\n");
    match Command::new("netstat").args(["-ano", "-p", "TCP"]).output() {
        Ok(output) => {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                // Proto, local address, foreign address, state, process id.
                if let [protocol, local, remote, _, owner] =
                    line.split_whitespace().collect::<Vec<_>>()[..]
                {
                    if protocol == "TCP"
                        && (local.ends_with(&suffix) || remote.ends_with(&suffix) || owner == pid)
                    {
                        let _ = writeln!(table, "  {}", line.trim());
                    }
                }
            }
            if !output.status.success() {
                let _ = writeln!(
                    table,
                    "  netstat exited with {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
        }
        Err(error) => {
            let _ = writeln!(table, "  netstat could not be run: {error}");
        }
    }
    let script = format!(
        "Get-NetTCPConnection | Where-Object {{ $_.LocalPort -eq {port} -or $_.RemotePort -eq \
         {port} }} | Sort-Object CreationTime | Format-Table -AutoSize LocalAddress, LocalPort, \
         RemoteAddress, RemotePort, State, OwningProcess, CreationTime | Out-String -Width 200"
    );
    let _ = writeln!(table, "Get-NetTCPConnection, rows for port {port}:");
    match Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
    {
        Ok(output) => {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                if !line.trim().is_empty() {
                    let _ = writeln!(table, "  {}", line.trim_end());
                }
            }
            if !output.status.success() {
                let _ = writeln!(
                    table,
                    "  powershell exited with {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
        }
        Err(error) => {
            let _ = writeln!(table, "  powershell could not be run: {error}");
        }
    }
    table
}
