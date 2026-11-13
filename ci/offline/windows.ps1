# Cargo test runner for Windows: block all non-loopback traffic of the test
# executable, in both directions, for as long as it runs.
#
#   cargo test --config ci/offline.toml ...
#
# Windows Defender Firewall never filters loopback, so program-scoped block
# rules leave the executable with loopback networking only. Needs an elevated
# shell (GitHub-hosted runners are).
#
# Name resolution and the temporary directory are left alone. The rules do not
# change how the machine's own name resolves, tests address their senders as
# 127.0.0.1:<port> in any case, and the name-lock files that the Linux runner
# isolates have only been observed on Linux.
#
# The SDK is given a configuration, through NDI_CONFIG_DIR, that points it at
# a loopback discovery address instead of multicast DNS, as macos.sh does:
# with multicast DNS blocked, a Windows receiver may never connect, even to an
# explicit loopback address.
#
# Under the rules, a receiver of a source that a receiver of the same process
# connected to before stalls about half the time, and with the firewall off
# none does. To choose rules that do not cause it, GRAFTON_NDI_CI_FIREWALL
# selects them for tests/peer_recreation.rs: `outbound` or `inbound` alone,
# `none` (the firewall on, with no rule for the program) or `off` (the firewall
# off); and the drops the firewall logs are printed when the executable fails.
#
# The configuration also turns on the SDK's own log, which names the port each
# sender listens on, the address each receiver connects to and whether it
# connected, and the transport it receives over. It is printed when the
# executable fails.
$ErrorActionPreference = 'Stop'

# `$program, $arguments = $args` would make $arguments a scalar string when exactly
# one argument follows the program, and `@arguments` then splats a string one
# character at a time (python.exe: can't open file '...\c'). Selecting into an
# array keeps zero, one and many arguments a list.
$program = $args[0]
$arguments = @($args | Select-Object -Skip 1)

# The rules name the program by its long path: New-NetFirewallRule refuses a
# path with an 8.3 short component ("The parameter is incorrect"), and rustdoc
# starts each doctest as rust_out.exe in a directory under %TEMP%, which
# GitHub's hosted Windows images set to the short form
# C:\Users\RUNNER~1\AppData\Local\Temp. Resolve-Path keeps short names;
# GetLongPathNameW replaces each short component with its long name, and
# returns a path that has none unchanged.
$program = (Resolve-Path -LiteralPath $program).Path
# Only a path with a short component needs expanding: compiling the helper
# costs every test executable's start, and those under target\ have none.
if ($program.Contains('~')) {
    Add-Type -TypeDefinition @'
using System.IO;
using System.Runtime.InteropServices;
using System.Text;

public static class GraftonNdiLongPath
{
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern uint GetLongPathNameW(string shortPath, StringBuilder longPath, uint length);

    public static string Of(string path)
    {
        // Room for the longest path the Unicode file functions accept.
        var buffer = new StringBuilder(32768);
        uint length = GetLongPathNameW(path, buffer, (uint)buffer.Capacity);
        if (length == 0)
        {
            throw new IOException("GetLongPathNameW failed for " + path + " with Win32 error " + Marshal.GetLastWin32Error());
        }
        if (length >= buffer.Capacity)
        {
            throw new PathTooLongException("the long form of " + path + " needs " + length + " characters");
        }
        return buffer.ToString();
    }
}
'@
    $program = [GraftonNdiLongPath]::Of($program)
}
# Where the firewall logs the packets it drops, once told to.
$drops = Join-Path $env:SystemRoot 'System32\LogFiles\Firewall\pfirewall.log'

# Print what the firewall dropped since its log was `$from` bytes long: the
# drops by protocol, source, destination, port, direction and process, most
# frequent first. The log's fields are: date time action protocol src-ip dst-ip
# src-port dst-port size tcpflags tcpsyn tcpack tcpwin icmptype icmpcode info
# path pid.
function Write-FirewallDrops([long] $from) {
    if (-not (Test-Path -LiteralPath $drops)) {
        Write-Output "Windows Firewall wrote no log at $drops."
        return
    }
    # The firewall writes its log in batches: wait until it stops growing.
    $size = -1
    for ($second = 0; $second -lt 15; $second++) {
        Start-Sleep -Seconds 1
        $now = (Get-Item -LiteralPath $drops).Length
        if ($now -eq $size -and $second -ge 3) { break }
        $size = $now
    }
    $stream = [System.IO.File]::Open($drops, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
    try {
        # A log that was rotated since is read from its start.
        if ($stream.Length -ge $from) { $stream.Position = $from }
        $text = [System.IO.StreamReader]::new($stream).ReadToEnd()
    }
    finally {
        $stream.Dispose()
    }
    $dropped = @($text -split "`r?`n" | Where-Object { $_ -match '^\S+ \S+ DROP ' })
    Write-Output "Windows Firewall dropped $($dropped.Count) packets while $program ran, here by protocol, source, destination, direction and process:"
    $dropped |
        ForEach-Object {
            $field = $_ -split ' '
            $process = if ($field.Count -gt 17) { $field[17] } else { '?' }
            '{0} {1}:{2} -> {3}:{4} {5} pid {6}' -f $field[3], $field[4], $field[6], $field[5], $field[7], $field[16], $process
        } |
        Group-Object |
        Sort-Object -Property Count -Descending |
        Select-Object -First 40 |
        ForEach-Object { Write-Output ('  {0,6} x {1}' -f $_.Count, $_.Name) }
    $dropped | Select-Object -First 3 | ForEach-Object { Write-Output "  first lines: $_" }
}

# The rules for this run: both directions, unless the experiment selects.
$confinement = if ($env:GRAFTON_NDI_CI_FIREWALL) { $env:GRAFTON_NDI_CI_FIREWALL } else { 'all' }
$rules = switch ($confinement) {
    'all' { @{ Direction = 'Outbound' }; @{ Direction = 'Inbound' } }
    'outbound' { @{ Direction = 'Outbound' } }
    'inbound' { @{ Direction = 'Inbound' } }
    'none' { }
    'off' { }
    default { throw "GRAFTON_NDI_CI_FIREWALL is '$confinement', not all, outbound, inbound, none or off" }
}

$rule = 'grafton-ndi-ci-' + [guid]::NewGuid().ToString('N')
$scratch = Join-Path ([System.IO.Path]::GetTempPath()) $rule
$config = Join-Path $scratch 'config'
$log = Join-Path $scratch 'log'
$result = 1
try {
    New-Item -ItemType Directory -Path $config, $log | Out-Null
    $ndi = @{
        # The experiment's runs also log what the SDK's sends cost and whom
        # they reach.
        log = @{ enable = $true; level = $(if ($env:GRAFTON_NDI_CI_FIREWALL) { 'debug' } else { 'info' }); dir = $log }
        networks = @{ discovery = '127.0.0.1' }
    }
    @{ ndi = $ndi } |
        ConvertTo-Json -Depth 5 |
        Set-Content -LiteralPath (Join-Path $config 'ndi-config.v1.json') -Encoding utf8NoBOM
    $env:NDI_CONFIG_DIR = $config
    if ($confinement -eq 'off') {
        Set-NetFirewallProfile -Profile Domain, Public, Private -Enabled False
    }
    else {
        Set-NetFirewallProfile -Profile Domain, Public, Private -Enabled True -LogBlocked True -LogMaxSizeKilobytes 32767 -LogFileName '%systemroot%\system32\LogFiles\Firewall\pfirewall.log'
    }
    foreach ($parameters in @($rules)) {
        try {
            New-NetFirewallRule -DisplayName $rule -Action Block -Program $program -Profile Any @parameters | Out-Null
        }
        catch {
            # The cmdlet's message does not name the program it refused.
            throw ('New-NetFirewallRule refused the program {0}: {1}' -f $program, $_)
        }
    }
    $dropsBefore = if (Test-Path -LiteralPath $drops) { (Get-Item -LiteralPath $drops).Length } else { 0 }
    $env:CARGO_NET_OFFLINE = 'true'
    & $program @arguments
    $result = $LASTEXITCODE
    if ($result -ne 0) {
        foreach ($file in Get-ChildItem -LiteralPath $log -File) {
            Write-Output "NDI SDK log $($file.Name), written while $program ran:"
            Get-Content -LiteralPath $file.FullName | Write-Output
        }
        Write-FirewallDrops $dropsBefore
    }
}
finally {
    Get-NetFirewallRule -DisplayName $rule -ErrorAction SilentlyContinue | Remove-NetFirewallRule
    Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
}
exit $result
