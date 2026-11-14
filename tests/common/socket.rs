//! Diagnostics for a receiver that could not connect to its sender.

use grafton_ndi::{Source, SourceAddress};

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
