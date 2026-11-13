# Cargo test runner for Windows: block the test executable's traffic to and
# from every address but the machine's own, in both directions, for as long as
# it runs.
#
#   cargo test --config ci/offline.toml ...
#
# Windows Defender Firewall never filters loopback, and Windows delivers
# traffic to one of the machine's own addresses as it delivers loopback, so
# nothing the executable sends leaves the machine and nothing from another
# machine reaches it. Needs an elevated shell (GitHub-hosted runners are).
#
# The rules leave the machine's own addresses alone because the SDK uses them:
# blocked as well, as every address but loopback was before, they stopped a
# receiver connecting to a sender that had already served another receiver,
# 11 to 18 of 25 times on every Windows row of run 43369298234, against none
# with the firewall off.
#
# Name resolution and the temporary directory are left alone. The rules do not
# change how the machine's own name resolves, tests address their senders as
# 127.0.0.1:<port> in any case, and the name-lock files that the Linux runner
# isolates have only been observed on Linux.
#
# The SDK is given a configuration, through NDI_CONFIG_DIR, that points it at
# a loopback discovery address instead of multicast DNS, as macos.sh does. The
# rules block multicast DNS, and without the address receivers stalled: in run
# 43366775596, under the rules, 43 to 54 of 100 receivers of
# tests/peer_recreation.rs never connected to a sender of their own process on
# 127.0.0.1 with loopback as the SDK's only adapter, and none with the address,
# or with the firewall off.
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

# The rules name the program by its long path. rustdoc starts each doctest as
# rust_out.exe in a directory under %TEMP%, which GitHub's hosted Windows
# images set to the 8.3 form C:\Users\RUNNER~1\AppData\Local\Temp. In run
# 43332050213 New-NetFirewallRule refused every doctest's program with "The
# parameter is incorrect", while the test executables under target\ and
# python.exe got their rules. The log prints none of those paths, so the short
# name is the probable cause rather than an observed one. Resolve-Path keeps
# short names; GetLongPathNameW replaces each short component with its long
# name, and returns a path that has none unchanged.
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
# An IPv4 address as a number, and back.
function ConvertTo-Number([string] $address) {
    $bytes = [System.Net.IPAddress]::Parse($address).GetAddressBytes()
    [Array]::Reverse($bytes)
    [uint64] [BitConverter]::ToUInt32($bytes, 0)
}
function ConvertTo-Address([uint64] $number) {
    $bytes = [BitConverter]::GetBytes([uint32] $number)
    [Array]::Reverse($bytes)
    [System.Net.IPAddress]::new($bytes).ToString()
}

# The remote addresses the rules block, as New-NetFirewallRule takes them:
# every IPv4 address but `$own`, in ranges, and every IPv6 address. Loopback
# is in neither list's reach, since the firewall never filters it.
function Get-RemoteAddresses([string[]] $own) {
    $ranges = [System.Collections.Generic.List[string]]::new()
    [uint64] $next = 0
    foreach ($address in @($own | ForEach-Object { ConvertTo-Number $_ } | Sort-Object -Unique)) {
        if ($address -gt $next) {
            # Built apart: inside Add's parentheses the comma would end its argument.
            $range = '{0}-{1}' -f (ConvertTo-Address $next), (ConvertTo-Address ($address - 1))
            $ranges.Add($range)
        }
        $next = $address + 1
    }
    if ($next -le [uint32]::MaxValue) {
        $range = '{0}-255.255.255.255' -f (ConvertTo-Address $next)
        $ranges.Add($range)
    }
    $ranges.Add('::-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff')
    , $ranges.ToArray()
}

$rule = 'grafton-ndi-ci-' + [guid]::NewGuid().ToString('N')
$scratch = Join-Path ([System.IO.Path]::GetTempPath()) $rule
$config = Join-Path $scratch 'config'
$log = Join-Path $scratch 'log'
$result = 1
try {
    New-Item -ItemType Directory -Path $config, $log | Out-Null
    $ndi = @{
        log = @{ enable = $true; level = 'info'; dir = $log }
        networks = @{ discovery = '127.0.0.1' }
    }
    @{ ndi = $ndi } |
        ConvertTo-Json -Depth 5 |
        Set-Content -LiteralPath (Join-Path $config 'ndi-config.v1.json') -Encoding utf8NoBOM
    $env:NDI_CONFIG_DIR = $config
    Set-NetFirewallProfile -Profile Domain, Public, Private -Enabled True
    $remote = Get-RemoteAddresses @(Get-NetIPAddress -AddressFamily IPv4 | ForEach-Object IPAddress)
    foreach ($direction in 'Outbound', 'Inbound') {
        try {
            New-NetFirewallRule -DisplayName $rule -Direction $direction -Action Block -Program $program -Profile Any -RemoteAddress $remote | Out-Null
        }
        catch {
            # The cmdlet's message does not name the program it refused.
            throw ('New-NetFirewallRule refused the program {0}: {1}' -f $program, $_)
        }
    }
    $env:CARGO_NET_OFFLINE = 'true'
    & $program @arguments
    $result = $LASTEXITCODE
    if ($result -ne 0) {
        foreach ($file in Get-ChildItem -LiteralPath $log -File) {
            Write-Output "NDI SDK log $($file.Name), written while $program ran:"
            Get-Content -LiteralPath $file.FullName | Write-Output
        }
    }
}
finally {
    Get-NetFirewallRule -DisplayName $rule -ErrorAction SilentlyContinue | Remove-NetFirewallRule
    Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
}
exit $result
