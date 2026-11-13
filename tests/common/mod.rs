//! Helpers shared by the integration tests.
//!
//! Each test binary uses a different subset, so unused helpers are expected.
#![allow(dead_code, unused_imports)]

use grafton_ndi::{capabilities, Error, Finder, FinderOptions, Ndi, Sender, Source, SourceAddress};
use std::{
    net::Ipv4Addr,
    time::{Duration, Instant},
};

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
///
/// Call it once for each receiver: on Windows every call returns a loopback
/// address of its own (see [`loopback_host`]).
pub fn loopback_source(ndi: &Ndi, sender: &Sender) -> grafton_ndi::Result<Source> {
    loopback_endpoint(ndi, sender.source()?.name())
}

/// The loopback endpoint of the source this process advertises as `name`; see
/// [`loopback_source`]. Some senders expose only their source, not themselves.
pub fn loopback_endpoint(ndi: &Ndi, name: &str) -> grafton_ndi::Result<Source> {
    let (source, port) = advertised_endpoint(ndi, name)?;
    let loopback = SourceAddress::Ip(format!("{}:{port}", loopback_host()));
    Ok(source.with_address(loopback))
}

/// The source this process advertises as `name`, as the SDK's discovery
/// reports it, and the port it is reached on. [`loopback_endpoint`] is what
/// a receiver is given; this is for a test that needs the port itself.
pub fn advertised_endpoint(ndi: &Ndi, name: &str) -> grafton_ndi::Result<(Source, u16)> {
    let finder = Finder::new(ndi, &FinderOptions::new().extra_ips("127.0.0.1"))?;
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

/// The loopback address the next endpoint from [`loopback_endpoint`] names.
///
/// On Windows it is a different one each time, 127.0.0.2, 127.0.0.3 and so
/// on, so that no receiver of a test process is given a destination that a
/// receiver before it used. Every address in 127.0.0.0/8 reaches a sender,
/// which listens on all interfaces, and the port stays the one the SDK chose.
/// Elsewhere it is always 127.0.0.1: Linux and macOS do not need it, and
/// macOS has no loopback address but 127.0.0.1.
///
/// On the hosted Windows runners (run 43342188690), a receiver given
/// `127.0.0.1:5961` sometimes never connected to the sender listening there,
/// for the whole 15 s a test waits, while a receiver opened 3 ms later to the
/// same address connected in 1 ms. 12 of 43 receivers opened within 150 ms of
/// another receiver's teardown never connected, against 1 of 15 whose SDK log
/// showed no receiver closing in the 300 ms before, although a sender had shut
/// down less than 150 ms before in both groups. The failures follow the
/// teardown of receivers more closely than the reuse of the port. A
/// destination that no connection has used leaves nothing the SDK or the
/// operating system keeps for a destination to get in the way; whether that
/// is enough is what `tests/peer_recreation.rs` measures, against a control
/// at 127.0.0.1. `ci/README.md` has the evidence.
///
/// When 127.0.0.2 does not reach a listener on this machine, the address stays
/// 127.0.0.1 and [`loopback_mitigation`] says why.
pub fn loopback_host() -> Ipv4Addr {
    match rotation() {
        Ok(next) => next(),
        Err(_) => Ipv4Addr::LOCALHOST,
    }
}

/// How [`loopback_host`] addresses senders in this process, for the reports
/// of tests that depend on it: `Ok` with what it does, or `Err` with why it
/// gives every endpoint 127.0.0.1.
pub fn loopback_mitigation() -> Result<&'static str, String> {
    match rotation() {
        Ok(_) => Ok("each endpoint has a loopback address of its own, from 127.0.0.2"),
        Err(why) => Err(why.clone()),
    }
}

/// The source of distinct loopback addresses, when this platform uses them.
#[cfg(windows)]
fn rotation() -> &'static Result<fn() -> Ipv4Addr, String> {
    use std::sync::{
        atomic::{AtomicU32, Ordering},
        OnceLock,
    };

    /// The low 24 bits of the next address in 127.0.0.0/8.
    static NEXT: AtomicU32 = AtomicU32::new(2);
    static ROTATION: OnceLock<Result<fn() -> Ipv4Addr, String>> = OnceLock::new();

    fn next() -> Ipv4Addr {
        loop {
            let host = NEXT.fetch_add(1, Ordering::Relaxed) & 0x00ff_ffff;
            // Skip the network and broadcast forms of each /24.
            if !matches!(host & 0xff, 0 | 0xff) {
                return Ipv4Addr::from(0x7f00_0000 | host);
            }
        }
    }

    ROTATION.get_or_init(|| {
        let reachable = reaches_listener(Ipv4Addr::new(127, 0, 0, 2));
        if let Err(why) = &reachable {
            // Written directly: the harness captures `eprintln!`.
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr(),
                "tests/common: every endpoint names 127.0.0.1, because {why}"
            );
        }
        reachable.map(|()| next as fn() -> Ipv4Addr)
    })
}

#[cfg(not(windows))]
fn rotation() -> &'static Result<fn() -> Ipv4Addr, String> {
    static UNNEEDED: std::sync::OnceLock<Result<fn() -> Ipv4Addr, String>> =
        std::sync::OnceLock::new();
    UNNEEDED.get_or_init(|| {
        Err(format!(
            "{} has not shown the failure the distinct addresses avoid",
            std::env::consts::OS
        ))
    })
}

/// Whether a connection to `host` reaches a listener on all interfaces, as
/// the SDK's senders listen.
#[cfg(windows)]
fn reaches_listener(host: Ipv4Addr) -> Result<(), String> {
    use std::net::{SocketAddr, TcpListener, TcpStream};

    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0))
        .map_err(|error| format!("a listener on 0.0.0.0 could not be opened: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("the listener has no address: {error}"))?
        .port();
    TcpStream::connect_timeout(&SocketAddr::from((host, port)), Duration::from_secs(2))
        .map(drop)
        .map_err(|error| format!("{host}:{port} did not reach a listener on 0.0.0.0: {error}"))
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
