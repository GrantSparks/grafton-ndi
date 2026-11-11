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

$program, $arguments = $args
$program = (Resolve-Path -LiteralPath $program).Path
$rule = 'grafton-ndi-ci-' + [guid]::NewGuid().ToString('N')
$result = 1
try {
    Set-NetFirewallProfile -Profile Domain, Public, Private -Enabled True
    foreach ($direction in 'Outbound', 'Inbound') {
        New-NetFirewallRule -DisplayName $rule -Direction $direction -Action Block -Program $program -Profile Any | Out-Null
    }
    $env:CARGO_NET_OFFLINE = 'true'
    & $program @arguments
    $result = $LASTEXITCODE
}
finally {
    Get-NetFirewallRule -DisplayName $rule -ErrorAction SilentlyContinue | Remove-NetFirewallRule
}
exit $result
