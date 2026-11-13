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
$rule = 'grafton-ndi-ci-' + [guid]::NewGuid().ToString('N')
$result = 1
try {
    Set-NetFirewallProfile -Profile Domain, Public, Private -Enabled True
    foreach ($direction in 'Outbound', 'Inbound') {
        try {
            New-NetFirewallRule -DisplayName $rule -Direction $direction -Action Block -Program $program -Profile Any | Out-Null
        }
        catch {
            # The cmdlet's message does not name the program it refused.
            throw ('New-NetFirewallRule refused the program {0}: {1}' -f $program, $_)
        }
    }
    $env:CARGO_NET_OFFLINE = 'true'
    & $program @arguments
    $result = $LASTEXITCODE
}
finally {
    Get-NetFirewallRule -DisplayName $rule -ErrorAction SilentlyContinue | Remove-NetFirewallRule
}
exit $result
