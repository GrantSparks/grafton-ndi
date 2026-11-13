# Continuous integration

Two workflows: [`rust.yml`](../.github/workflows/rust.yml) checks every push to
`main` and every pull request, and [`publish.yml`](../.github/workflows/publish.yml)
releases a version tag. Everything here is plain YAML, a few shell scripts and
standard-library Python, tested by `python -m unittest discover -s ci/tests`.

The crate builds without an NDI SDK, so most of CI needs no secrets. Only the
`sdk` job downloads the vendor's SDK packages, to run the tests against real
runtimes.

## Jobs in `rust.yml`

| Job | Runs for | What it checks |
|---|---|---|
| `lint` | every event | CI script tests, shellcheck, actionlint, rustfmt, clippy (`-D warnings`) for the workspace with all features and for the crate with no features and with each of `image-encoding`, `tokio`, `smol`, `advanced`, `file-io`, `advanced,file-io`; the bindings generator's tests; rustdoc (`-D warnings`); `cargo check` on Rust 1.88; `cargo package`; the semver check; `cargo audit` |
| `test` | every event | `cargo test` with all features and with default features on Linux, macOS and Windows. No NDI runtime is installed, so tests that need one print `skipped: <test> needs …` |
| `cross` | every event | clippy (`-D warnings`, all features, all targets) for every dynamically loaded target: Linux aarch64, armv7 and i686; Android aarch64, armv7, i686 and x86_64; Windows x86_64 and i686; macOS aarch64 and x86_64. Then a link of the examples for `aarch64-linux-android` with the runner's NDK ([`scripts/build-android.sh`](../scripts/build-android.sh)) |
| `sdk` | runs that are given the secrets | the tests against real runtimes; see below |
| `required-ci` | every event, `if: always()` | passes only if every job the event requires succeeded ([`gate.py`](gate.py)) |

`required-ci` is the one check branch protection should require. For pushes,
manual runs, releases and pull requests from this repository, every job must
succeed, and a skipped `sdk` job is a failure. Two kinds of run are given no
secrets: pull requests from forks, and everything Dependabot starts. There the
`sdk` job is skipped and the gate passes on `lint`, `test` and `cross`.

The gate is computed by files a pull request can change: `rust.yml` decides
which jobs exist and when they run, and [`gate.py`](gate.py) decides the result.
A pull request can therefore weaken the check that judges it, and nothing inside
a `pull_request` workflow can prevent that. The control is review: `.github/`
and `ci/` have a code owner, so with "Require review from Code Owners" on `main`
such a change cannot merge without the owner's approval (see
[the settings](#repository-settings-in-order)).

The semver check ([`semver.py`](semver.py)) runs `cargo semver-checks` with no
options against the newest release on crates.io. It is skipped while the crate's
major version is ahead of every published release: a new major version may
change anything, so the tool would run no lint.

A new push to a pull request cancels its running check. Runs for `main`, manual
runs and releases are never cancelled. Every job has a timeout, the default
token permission is `contents: read`, and every action is pinned to a commit.

## The `sdk` job

The job runs once per row of this matrix. The table is checked against the
workflow by the script tests.

| Row | Runner | Target | Package | Promise, all features | Promise, no default features | Also runs |
|---|---|---|---|---|---|---|
| linux-x64 standard-6.1.1 | ubuntu-24.04 | x86_64-unknown-linux-gnu | 6.1.1-standard-linux | `1` | `1` | |
| linux-x64 standard-6.3.2 | ubuntu-24.04 | x86_64-unknown-linux-gnu | 6.3.2-standard-linux | `1` | `1` | |
| linux-x64 advanced-6.3.2 | ubuntu-24.04 | x86_64-unknown-linux-gnu | 6.3.2-advanced-linux | `advanced` | `1` | Advanced lint |
| linux-x64 standard-6.4 | ubuntu-24.04 | x86_64-unknown-linux-gnu | current-standard-linux | `1,discovery-server` | `1,discovery-server` | vendored bindings check |
| linux-x64 advanced-6.4 | ubuntu-24.04 | x86_64-unknown-linux-gnu | current-advanced-linux | `advanced,file,discovery-server,h264-decoder,hevc-444-decoder` | `1,discovery-server` | Advanced lint |
| linux-x86 standard-6.4 | ubuntu-24.04 | i686-unknown-linux-gnu | current-standard-linux | `1,discovery-server` | `1,discovery-server` | |
| linux-arm64 standard-6.4 | ubuntu-24.04-arm | aarch64-unknown-linux-gnu | current-standard-linux | `1,discovery-server` | `1,discovery-server` | |
| windows-x64 standard-6.3.2 | windows-2025 | x86_64-pc-windows-msvc | 6.3.2-standard-windows | `1` | `1` | |
| windows-x64 advanced-6.3.2 | windows-2025 | x86_64-pc-windows-msvc | 6.3.2-advanced-windows | `advanced,h264-decoder` | `1` | Advanced lint |
| windows-x64 standard-6.4 | windows-2025 | x86_64-pc-windows-msvc | current-standard-windows | `1,file` | `1` | |
| windows-x64 advanced-6.4 | windows-2025 | x86_64-pc-windows-msvc | current-advanced-windows | `advanced,file,h264-decoder` | `1` | Advanced lint |
| windows-x86 advanced-6.4 | windows-2025 | i686-pc-windows-msvc | current-advanced-windows | `advanced,h264-decoder` | `1` | Advanced lint |
| macos-arm64 standard-6.3.2 | macos-15 | aarch64-apple-darwin | 6.3.2-standard-apple | `1` | `1` | |
| macos-arm64 advanced-6.3.2 | macos-15 | aarch64-apple-darwin | 6.3.2-advanced-apple | `advanced,h264-decoder` | `1` | Advanced lint |
| macos-arm64 standard-6.4 | macos-15 | aarch64-apple-darwin | current-standard-apple | `1` | `1` | Apple mobile builds |
| macos-arm64 advanced-6.4 | macos-15 | aarch64-apple-darwin | current-advanced-apple | `advanced,file,h264-decoder,hevc-444-decoder` | `1` | Advanced lint, Apple mobile builds |
| macos-x64 standard-6.4 | macos-15-intel | x86_64-apple-darwin | current-standard-apple | `1` | `1` | |

On the Linux x64, Windows x64 and macOS ARM64 current Advanced rows, four
additional runs, with all features, cross versions in both directions: a
build with the current Advanced headers against each retained baseline
runtime, and a build from each baseline SDK against the current Advanced
runtime. A Standard SDK has no headers a build reads, so the Standard
baseline's build has no Advanced declarations. The Linux Standard
baseline is 6.1.1; the Windows and macOS Standard baselines and every Advanced
baseline are 6.3.2. Each compatibility run also starts a peer process on the
other SDK, under the same network confinement, which sends to a receiver on the
run's runtime; the test checks video pixels, audio samples, timecodes and
metadata. A run with a Standard runtime promises `1`, a run with an Advanced
baseline runtime `advanced`, and a run with the current Advanced runtime the
row's promise without its decoders, and with `advanced` replaced by `1` when
the build has no Advanced declarations. No compatibility run promises an
NDI|HX decoder; the decode tests run there all the same, and pass or skip
visibly. Only the current runtime has the row's NDI|HX driver beside it.
Every run checks the loaded runtime's version, edition and path against the
values `sdk.py` exported for it. Advanced operations without matching build
declarations report unavailable.

Each row:

1. installs what the row needs beyond the runner image: `bubblewrap`,
   `libavahi-client3` and `libclang-dev` on Linux (plus the i386 linker and
   libraries for the x86 row), LLVM on Windows;
2. provisions the package with [`sdk.py`](sdk.py), which exports `NDI_SDK_DIR`
   and `NDI_RUNTIME_DIR_V6` to the following steps, together with
   `GRAFTON_NDI_EXPECTED_VERSION`, `GRAFTON_NDI_EXPECTED_EDITION` and
   `GRAFTON_NDI_EXPECTED_RUNTIME`, and also as step outputs. Every real-runtime
   test checks the loaded module against those values. A row with an
   `hx-driver` copies that driver package beside the runtime it just
   installed (see [The NDI|HX driver](#the-ndihx-driver)). A row with
   compatibility runs provisions its two baseline packages too, into separate
   directories, and writes their values to files under `$RUNNER_TEMP` for those
   runs. This step is the only one given the secrets in its environment;
3. proves that the test runner isolates ([`offline/check.py`](offline/check.py)
   outside and inside it);
4. runs `cargo test --locked --config ci/offline.toml --target <target>` twice,
   with `--all-features` and with `--no-default-features`;
5. runs the row's extra steps:
   - **vendored bindings check**: `cargo xtask bindgen --check --sdk "$NDI_SDK_DIR"`;
   - **Advanced lint**: clippy (`-D warnings`) with all features, with
     `advanced` and with `advanced,file-io`, and rustdoc (`-D warnings`).
     Without the Advanced headers the `advanced` feature compiles to stubs, so
     these rows are the only place the real implementation is linted;
   - **Apple mobile builds**: [`apple-mobile.sh`](apple-mobile.sh) builds the
     library for the statically linked targets that need the SDK at build
     time: iOS and its x86_64 simulator, tvOS, visionOS, and the x86_64 tvOS
     simulator, which needs a pinned nightly toolchain and `-Zbuild-std`;
   - **compatibility runs**: on the rows that have them, the four crossed runs
     described above.

The job uses no dependency cache and uploads nothing, so SDK files and the
Advanced bindings generated into the target directory never leave the runner.

### Who can reach the secrets

The job as a whole is the trust boundary, not the provisioning step. Every step
runs on the machine the secrets were sent to, with administrator rights: the
tests, every build script and procedural macro in `Cargo.lock`, and whatever a
step installs. And the job runs the workflow of the commit being tested, so
anyone who can push a branch to this repository can run their own version of it
with the secrets, through a pull request, a manual run or a tag. The secrets are
therefore as private as the list of people with write access, and no more.

What limits the damage is the owner's to choose:

- make `NDI_SDK_ARCHIVE_TOKEN` a fine-grained token that can only read the
  contents of the archive repository, give it an expiry, and rotate it;
- optionally keep both secrets in an environment that only `main` and `v*` tags
  may use. Pull requests then get no SDK rows at all, and the `sdk` condition
  and the gate's expectation in `rust.yml` have to exclude them to match.

### What a row promises

A test that needs native code runs where that code is available and prints a
skip elsewhere. `GRAFTON_NDI_REQUIRE_RUNTIME` lists what the environment
provides, which turns the matching skips into failures, so a provisioning
mistake cannot pass as a skip:

| Entry | The row provides | A build can check it when |
|---|---|---|
| `1` | an NDI runtime (implied by every other entry) | always |
| `advanced` | an Advanced runtime, compiled with its headers | the `advanced` feature is on |
| `file` | a File library the selected runtime can drive: an Advanced runtime on Linux and macOS, any 6.4 runtime on 64-bit Windows | the `file-io` feature is on |
| `discovery-server` | the SDK's Discovery Server program and a 6.4 runtime, whose advertisers and listeners can use it | the target is Linux |
| `h264-decoder` | an NDI\|HX decoder for H.264 the selected runtime can use. It is built into the runtime on macOS and Windows. On Linux the runtime needs the NDI\|HX Driver for Linux or FFmpeg 7; the runner has no FFmpeg 7, so a row provides the driver through `hx-driver` | the `advanced` feature is on |
| `hevc-444-decoder` | the same for HEVC 4:4:4, with a 6.4 runtime. No Windows row provides one: the hosted Windows Server 2025 runners delivered black placeholder frames for HEVC 4:4:4 | the `advanced` feature is on |

A promise that no test in a build can check is itself a failure, and a missing
one lets a test skip, so each step promises exactly what its build can check:
the all-features step everything the row's package provides, the
no-default-features step only `1` and, on Linux with a 6.4 package,
`discovery-server`. The script tests hold every row to this rule.

Two tests send NDI|HX from an Advanced sender and check the decoded picture:
one H.264 4:2:0 frame received as UYVY, which `h264-decoder` covers, and one
HEVC 4:4:4 frame received with the 6.4 Best444 receiver mode, which
`hevc-444-decoder` covers. Both macOS Advanced rows promise `h264-decoder`,
and the macOS and Linux x64 Advanced 6.4 rows promise both; the Linux row
installs the driver for them. Without a decoder the runtime delivers black
1920x1080 frames instead of the picture, and no error. On Linux each test
looks for a decoder first: `hx_decoder()` in `src/test_support.rs` lists
where. Everywhere, it then reports its decoder missing, as a skip or as a
broken promise, when every frame it receives is that placeholder.

The Advanced SDK manual documents H.264 decoding on Windows and macOS without
plugins, but H.265 only in 4:2:0 and where decoding hardware supports it. In
run 43332050213 both Windows Advanced 6.4 rows received only placeholders for
HEVC 4:4:4, while the macOS Advanced 6.4 row decoded it; the hosted Windows
runners have no GPU, which is the probable cause, not a confirmed one. So no
Windows row promises `hevc-444-decoder`. H.264 is another matter: in run
43342188690 the H.264 test decoded on every Windows Advanced row, 6.3.2 and
6.4, x64 and x86, so those rows promise `h264-decoder`.

## SDK packages

[`sdk-packages.json`](sdk-packages.json) pins each SDK package by size and
SHA-256. [`sdk.py`](sdk.py) installs one:

```sh
python ci/sdk.py list
python ci/sdk.py install current-standard-linux --dest ~/ndi/standard-6.4 \
    --archive ~/Downloads/Install_NDI_SDK_v6_Linux.tar.gz --target x86_64-unknown-linux-gnu
```

It copies or downloads the archive into a staging directory, checks the copy
against the pin before opening it, unpacks it without running the vendor's
installer script (the Windows installer is the exception: it is the package),
and prints the values that select the installation: `NDI_SDK_DIR` and, with
`--target`, `NDI_RUNTIME_DIR_V6` and the three `GRAFTON_NDI_EXPECTED_*`
identity values. `--github-env FILE` appends them to a file in the
`NAME=value` form and `--github-output FILE` appends them as step outputs.
The `--dest` directory must not exist yet. `--target` accepts the Linux
x86_64, i686 and aarch64, Windows x64 and x86, and macOS x64 and arm64
targets. Every archive member must stay inside the destination, and the
unpacked tree is checked again for links that lead out of it. Apple packages
unpack on macOS only and Windows packages on Windows only.

Without `--archive`, the package comes from the source map in
`NDI_SDK_SOURCES_JSON`:

```json
{"schema": 1, "packages": {"current-standard-linux": {"github_release_asset": {"repo": "owner/private-store", "asset_id": 1}}}}
```

An entry is one of `github_release_asset` (read with the token in
`NDI_SDK_ARCHIVE_TOKEN`, or in the variable named by `token_env`), `url_env`
(the name of a variable holding an HTTPS URL) or `archive` (an absolute local
path). The token is sent to `api.github.com` only and a transient HTTP failure
is retried twice. A package's entry is keyed by its id, as above, or by its
content-qualified name, `ndi-<version>-<edition>-<platform>-<sha256>`, and
exactly one of the two must be present. A driver package's name has
`hx-driver` in place of the edition. The map needs an entry for every
package the `sdk` matrix uses, including the baseline packages of the
compatibility runs and the `hx-driver` packages.

Nothing the script prints contains a value from the source map, from a variable
the map names, or the token: its own messages are written without them, a
failing vendor tool is reported by name and exit status, and anything unexpected
by its type and the line that raised it. The vendor tools are given the staged
copy, never a path from the map, and none of those variables.

### The NDI|HX driver

On Linux the NDI runtime decodes NDI|HX video (H.264 and HEVC) only through a
decoder it finds when it runs. NDI ships one separately as the NDI|HX Driver
for Linux: `libndihx.so` and the FFmpeg 7 libraries it bundles under `-ndi`
names, which the runtime loads from its own directory or, failing that, by name
through the loader. `current-hx-driver-linux` pins that download.
Its pin has `driver` where an SDK's has `edition`, and its version is the SDK
release it was built with: the driver's `Version.txt` carries no version
number, only the vendor build (`git-…`).

```sh
python ci/sdk.py install-driver current-hx-driver-linux --sdk ~/ndi/advanced-6.4 \
    --archive ~/Downloads/NDIHXDriverForLinux.tar.gz --target x86_64-unknown-linux-gnu
```

`--sdk` is the directory `install` unpacked an SDK into, or the SDK root. The
command acquires and checks the archive as `install` does, unpacks the
self-extracting `NDIHXDriverForLinux*.sh` without running it, and refuses links
that lead out of the unpacked tree. It then requires that the SDK's
`Version.txt` names the pinned release and the same vendor build as the
driver's, and copies only `libndihx.so` and the `lib*-ndi.so.*` libraries of the
target's directory (`x86_64-linux-gnu` or `i686-linux-gnu`) beside the
runtime library. It refuses any other file there, refuses to overwrite a file
the runtime directory already has, removes what it copied when a copy fails,
and prints the files it added. The driver also needs `libcurl.so.4` on the
system, which the Ubuntu runner image includes.

In the `sdk` job, the provisioning step installs a row's `hx-driver` beside the
runtime it has just installed, so the secrets stay with that one step. The
driver comes under the NDI|HX licence agreement, which its archive includes. On
Linux, NDI grants no licence for the H.264 and AAC technology the driver uses,
so whoever runs it must hold any licence that use needs; the bundled FFmpeg
libraries carry their own licence too.

## Loopback-only test runs

Tests against a real runtime create NDI senders and finders. `ci/offline.toml`
makes Cargo start every test executable through a wrapper in
[`offline/`](offline) that leaves it loopback networking only, so nothing is
announced on, or reached over, the network the machine is attached to.
Doctests are included: the current stable Cargo hands the same runner to
rustdoc, which starts each compiled doctest through it, with or without
`--target`.

| | Linux ([`linux.sh`](offline/linux.sh)) | macOS ([`macos.sh`](offline/macos.sh)) | Windows ([`windows.ps1`](offline/windows.ps1)) |
|---|---|---|---|
| Network | own network namespace (`bwrap --unshare-net`) holding only `lo` | Seatbelt profile allowing only binding, loopback connections and traffic arriving on a local address | Defender Firewall block rules for the test executable, both directions; the firewall never filters loopback |
| Host daemons | `/run` replaced by an empty tmpfs: no D-Bus, no Avahi | mDNSResponder socket denied; the SDK is given a loopback discovery address through `NDI_CONFIG_DIR` | untouched |
| Machine's own name | resolves to `127.0.0.1` through a private `/etc/hosts` | cannot be relied on to resolve | resolves as usual |
| `TMPDIR` | private tmpfs (`/run/tmp`) | private directory, removed afterwards | untouched |
| SDK log | off | off | on, through `NDI_CONFIG_DIR`; printed when the executable fails |
| Needs | `bubblewrap`, unprivileged user namespaces | nothing | an elevated shell |

What each wrapper confines, and what it does not:

- **Linux.** The namespace holds the test executable and every process it
  starts, and it has no interface but loopback, so no traffic of any kind can
  leave or arrive. Only the network is confined: the file system and other
  processes are as visible as before.
- **macOS.** The Seatbelt profile applies to the test executable and every
  process it starts. It refuses outbound traffic to anything but loopback and
  connections to Unix-domain sockets. It is not a packet filter: traffic that
  other machines send to a listener the tests open on a wildcard address is
  still delivered, because the profile language cannot restrict the peer of
  inbound traffic.
- **Windows.** The two firewall rules name one program, the test executable.
  Its traffic to and from anything but loopback is blocked for as long as it
  runs. A process it starts from a different program file is not covered, and
  nothing else on the machine is affected. The rules name the program by its
  long path, which the wrapper gets from `GetLongPathNameW`: rustdoc starts
  each doctest from a directory under `%TEMP%`, which the hosted images set to
  the 8.3 form `C:\Users\RUNNER~1\AppData\Local\Temp`, and in run 43332050213
  `New-NetFirewallRule` refused every doctest's program with "The parameter is
  incorrect" while the executables under `target\` got their rules. The log
  prints none of those paths, so the short name is the probable cause, not an
  observed one.

Each `sdk` row runs [`offline/check.py`](offline/check.py) outside and then
inside the wrapper. Inside, IPv4 loopback TCP and UDP must work, and the
operating system must refuse at once a TCP connection to each of three public
addresses and a UDP datagram to the mDNS multicast group that NDI discovery
uses; a timeout, or any answer from a remote host, fails the check. Outside, the
same attempts must succeed, which shows that the refusals are the wrapper's
doing. There is one exception: the hosted macOS machines refuse the multicast
datagram of the unconfined process itself (`refused`, as inside the wrapper). The
control then prints, on stdout, that this is a limitation and does not fail;
the refusal of the mDNS group inside the runner shows nothing about the wrapper
on such a machine, and only the public TCP connections attribute the refusals to
it. A timeout or any other outcome of the datagram still fails the control.
Inside the Windows wrapper there is a second exception: Defender Firewall drops
a blocked outbound datagram without failing the send, so `sendto` succeeds and
the check cannot observe the refusal (Linux and macOS report it at once). When
every public TCP connection was refused, which shows that the block rules apply
to the program, a datagram that reports `reached` is printed as a limitation and
does not fail; with any TCP connection not refused, or any other datagram
outcome, the check fails as on every other platform. The check exercises the one process the wrapper starts. It does not show what the
points above say a wrapper leaves open.

Two parts of the wrappers exist because of how the SDK behaves:

- **The machine's own name.** A sender advertises itself as `<host name>
  (<sender name>)`, and looking a source up by host name asks the SDK to resolve
  that name. Only loopback exists in the namespace, so the name has to resolve
  to `127.0.0.1`: with the machine's usual address, or with no answer, the
  lookup fails with `NoSourcesFound { criteria: "host: …" }`. The wrapper
  therefore binds a private `/etc/hosts` that maps the name, and the name with
  a trailing dot, to loopback. One test depends on this,
  `tests/video_padding_sdk.rs`, and it is compiled for Linux only; every other
  test addresses its sender as `127.0.0.1:<port>`. The macOS and Windows
  wrappers therefore do nothing about name resolution. A test that looks
  sources up by host name on macOS would need more than that wrapper offers.
- **`TMPDIR`.** The SDK reserves every sender and receiver name machine-wide
  with a lock file, `$TMPDIR/NTK_SEND_<sender name>`. In a shared directory two
  unrelated processes that use the same sender name fail each other with
  "Failed to create NDI send instance", and the files are left behind. This was
  observed on Linux; the macOS wrapper isolates `TMPDIR` as a precaution and
  the Windows wrapper leaves it alone.

The Windows wrapper also turns on the SDK's own log, at level `info`, through
an `NDI_CONFIG_DIR` whose configuration sets nothing else, and prints the log
after an executable that failed. The log names the port each sender listens
on, the address each receiver opens and whether it connected, and the
transport it receives over (`single-TCP` over loopback on Linux with 6.4.0.2).
It exists because the first Windows runs (run 43332050213) failed tests that
wait for media from a sender of their own: in a different set of tests on each
row, a receiver got nothing for 15 seconds, and the tests could not say whether
it had connected. On Linux the namespace leaves the SDK no address but
loopback, and on macOS it is given a loopback discovery address; on Windows it
sees the machine's network adapter and is given nothing, and the log shows
which addresses it used. In run 43342188690 the three Windows Advanced rows
printed it; the Windows Standard 6.4 row's failed executable printed none,
although the Linux Standard 6.4 runtime writes the log with the same
configuration.

### Receivers that never connect on Windows

In run 43342188690 the SDK logs of the three Windows Advanced rows show 70
receivers that `tests/advanced_runtime.rs` opened at `127.0.0.1:<port>` to a
sender of its own (one test's 16 simultaneous receivers counted once). 15
never connected: the receiver reported no connection for the whole 15 s, and
its sender logged no `connection opened`. Where a test opened a second receiver
3 to 10 ms later to the same address, that one connected within 2 ms; every
receiver that connected did so within 4 ms. The failures move between tests
and rows from run to run. What preceded each receiver:

| Within 150 ms before the receiver was opened | Receivers | Never connected |
| --- | --- | --- |
| a receiver of the process closed, and a sender shut down | 43 | 12 |
| a sender shut down, but no receiver the log shows closed within 300 ms | 15 | 1 |
| neither | 12 | 2 |

The three in the last two rows each followed a test whose receivers' teardown
the log does not show: one had created receivers by name only and with no
source, and in the other two rows the test just before had no sender and only
a genlock without a reference. So the failures follow the teardown of
receivers in the same process more closely than they follow the reuse of the
port. Two more things count against a listener of the previous sender that
outlives it and takes the next connection, which a socket of the new sender
would have to share the port with:

- On Linux, `strace` of the Advanced 6.4 runtime shows that a sender binds its
  listener on `0.0.0.0` without `SO_REUSEADDR`, and a sender created while
  another socket holds 5961 is given 5962 (both measured with
  `ci/offline/linux.sh`). If the Windows runtime binds the same way, a
  listener that outlived its sender would have moved the next sender off 5961;
  on Windows every such sender was given 5961.
- On the Advanced 6.3.2 row, the 15 receivers opened within 100 ms after the
  one that failed in the 16-receiver test all connected to the same sender, as
  did the second receivers above: the sender's listener accepted connections
  throughout.

`tests/receiver_video_only.rs` failed differently, on the Windows Standard 6.4
row of runs 43332050213 and 43342188690 (no other Windows row has run it with a
runtime that has video-only receiving): both times its `VideoOnlyLowest`
receiver, opened as soon as the `VideoOnlyHighest` one was dropped, was given
no video in 15 s while the ordinary receiver was given audio throughout, and in
the second run, which reported the counts, it counted no connection while the
sender counted two, its ordinary receiver and one more. In
`a_genlock_moves_between_threads` on the x64 Advanced 6.4 row of run
43342188690 the genlock's receiver connected and received, and the genlock
never acquired the reference: a third failure, not one of a connection.

`tests/common` therefore gives every receiver on Windows a loopback address no
receiver of its process was given before: `loopback_endpoint` names
127.0.0.2, 127.0.0.3 and so on with the port the SDK advertises (see
`loopback_host`). Every address in 127.0.0.0/8 reaches a sender's listener on
`0.0.0.0`, so a receiver never connects to a destination that a torn-down
connection used, whatever the SDK or the operating system keeps for one.
Linux and macOS keep 127.0.0.1: neither has shown the failure, and macOS has
no other loopback address. The helper first checks that 127.0.0.2 reaches a
listener on `0.0.0.0`, and otherwise keeps 127.0.0.1 and says why on standard
error. Tests that open several receivers resolve the endpoint once for each.

`tests/peer_recreation.rs` asserts that receivers connect to senders and as
receivers that are recreated in quick succession, and prints a line for each
receiver: the sender's port and the one before, the address, when the receiver
first counted a connection and when it was given a frame, and both ends'
connection counts. On Windows each arm also runs as an unasserted control whose
receivers are given `127.0.0.1:<port>`. Read with this table, its report
separates the explanations:

| The report shows | A listener that outlives its sender | State per destination, left by a connection's teardown | State per process, left by a receiver's teardown |
| --- | --- | --- | --- |
| control `sender` arm, gaps of 0 and 50 ms | failures | failures | failures |
| helper `sender` arm, gaps of 0 and 50 ms | failures | none | failures |
| `settled` arms, receiver dropped 500 ms before its sender | failures | none | none |
| control `receiver` arm | none | failures | failures |
| helper `receiver` arm | none | none | failures |

The gaps of 250 ms and 1 s show how long whatever the failure needs lasts. The
port probe, which runs first, says whether the first explanation is possible
at all: it needs the SDK's listener to share a port with another socket, which
the probe shows as a sender given the port it holds, and a port that cannot be
bound as soon as its sender is dropped is a socket that outlived it. A
receiver that has not connected after 2 s is diagnosed with the Windows TCP
table of its port (`common::socket_table`, also added to the failure messages
of `tests/advanced_runtime.rs` and `tests/receiver_video_only.rs`): two
`LISTENING` rows for `0.0.0.0:<port>` owned by the test, told apart by their
`CreationTime`, are a listener that outlived its sender; a row from the
receiver to the port that is `ESTABLISHED` while the sender counts no
connection is a connection nobody services; `SYN_SENT` is a connection
attempt nobody answers; and no row at all is a receiver that has not tried,
which is state in the SDK rather than in TCP. A second receiver is then opened
to the same address, which shows whether the sender takes connections at that
moment.

On Linux the same run works locally:

```sh
export NDI_RUNTIME_DIR_V6="$HOME/ndi/standard-6.4/NDI SDK for Linux/lib/x86_64-linux-gnu"
GRAFTON_NDI_REQUIRE_RUNTIME=1 cargo test --config ci/offline.toml --all-features
```

## Releasing

1. Merge the version bump to `main`.
2. Tag that commit `v<version>` and push the tag.

`publish.yml` runs for `v*` tags only, in four jobs that each need the one
before:

| Job | What it does |
|---|---|
| `release` | the tag is `v` plus the `Cargo.toml` version ([`release.py`](release.py)) and its commit is on `main` |
| `ci` | the whole of `rust.yml`, including `required-ci` |
| `verify` | `cargo package --locked`, which builds the packaged crate with every dependency build script, then reports the package's SHA-256 and the toolchain version. No registry access |
| `publish` | in the `crates-io` environment with `id-token: write`: re-creates the package with `--no-verify` on the same toolchain, requires its SHA-256 to be the verified one, and runs `cargo publish --locked --no-verify` through crates.io trusted publishing |

The `publish` job holds the publishing identity, so it runs nothing from the
repository:

- no crate or dependency is compiled (`--no-verify`), and no repository script
  is called;
- Cargo is started outside the checkout, with `--manifest-path`. Cargo and
  rustup read configuration from the working directory upwards, so started
  there they would obey the repository's `/.cargo/config.toml` (which can route
  requests through a proxy or name a program to hand the registry token to) and
  its `rust-toolchain.toml`. Started outside, only the runner's own Cargo home
  applies;
- from the `verify` job, which built code, it takes two strings and checks each
  for its exact form: a 64-digit SHA-256, compared with the digest of the one
  package file this job creates, and a toolchain version number.

docs.rs builds the documentation from the published crate; nothing is deployed
to GitHub Pages.

### What decides who may publish

Not the workflow. A tag runs the workflow file of the commit it points at, so
whoever can push a `v*` tag can also change or remove every check in
`publish.yml`, the tag-on-`main` check included; that check only catches a
mistaken tag. The controls are the repository settings below: the reviewers the
`crates-io` environment requires, the binding of the crates.io trusted publisher
to that environment, and the rule on who may create `v*` tags. GitHub creates an
environment that a workflow names, without any protection, the first time the
workflow runs, which is why the order matters.

## Repository settings, in order

Used as they exist today:

| Setting | Purpose |
|---|---|
| Secret `NDI_SDK_SOURCES_JSON` | source map for the private SDK archives, with an entry for every package the `sdk` matrix uses, including `6.1.1-standard-linux` and the 6.3.2 baselines |
| Secret `NDI_SDK_ARCHIVE_TOKEN` | read-only token for the archive repository |
| Variable `NDI_CI_RELEASED` = `true` | switches the `sdk` job on. With any other value the job is skipped, which fails `required-ci` wherever the secrets exist |

To be set up by the repository owner, in this order, before the first `v*` tag
is pushed:

1. **Environment `crates-io`.** Create it by hand, with required reviewers, with
   "Prevent self-review" if more than one person can approve (with a single
   maintainer it would block every release), and with deployment restricted to
   tags matching `v*`.
2. **Tag ruleset.** Restrict who may create, move or delete tags matching `v*`.
3. **Protection of `main`.** Require a pull request with "Require review from
   Code Owners", and require the `required-ci` check. Code-owner review is what
   protects the gate, the workflows and the files in
   [`CODEOWNERS`](../.github/CODEOWNERS): besides `.github/`, `ci/` and
   `scripts/`, those are `/.cargo/` and `rust-toolchain.toml`, which cargo and
   rustup obey in every job without being named anywhere, and `Cargo.toml` and
   `Cargo.lock`, which choose the third-party code that runs next to the SDK
   secrets and the files a release publishes.
4. **crates.io trusted publisher.** Only now register it: repository
   `GrantSparks/grafton-ndi`, workflow `publish.yml`, environment `crates-io`.
5. **Archive token** (optional, see [Who can reach the secrets](#who-can-reach-the-secrets)):
   replace it with a fine-grained, read-only, expiring token.

No longer read by the workflows: the per-package `NDI_SDK_*_URL` secrets and
`NDI_SDK_*_SHA256` variables. Nothing deploys to the `github-pages` environment
any more.

## Running the checks locally

```sh
python -m unittest discover -s ci/tests        # CI scripts and workflow guards
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --all-features
cargo test --locked --all-features             # runtime tests skip without an NDI runtime
```

To run the runtime tests, install a package with `ci/sdk.py`, export the printed
`NDI_RUNTIME_DIR_V6`, and use the loopback-only command above. For the Advanced
implementation also export the printed `NDI_SDK_DIR` of an Advanced package and
have libclang installed.

## Files

| File | Purpose |
|---|---|
| `sdk.py`, `sdk-packages.json` | verify and install a pinned SDK package, or a pinned driver beside an installed runtime |
| `offline.toml`, `offline/` | loopback-only test runner per operating system, and the check that it isolates |
| `gate.py` | the `required-ci` decision |
| `semver.py` | the semver check against the newest published release |
| `release.py` | the release tag check |
| `apple-mobile.sh` | builds for the statically linked Apple targets |
| `tests/` | tests for the scripts, and the guards on the workflows (`tests/workflows.py` reads the workflow files for them) |

The workflow guards in `tests/test_workflows.py` state the security rules of the
two workflows exactly: the permissions of every job, every place that mentions
secrets, the jobs `required-ci` waits for, the actions each job uses, and how
the `publish` job starts Cargo. They also list wrong edits that must be
reported. A change to a workflow that touches one of these rules has to change
the guards too, and both have a code owner.

## Not yet run on a hosted runner

These workflows have not run on GitHub. On Linux x86_64 the commands of `lint`,
`test` and `cross`, the test commands of the five Linux x64 `sdk` rows and of
their compatibility runs, and SDK provisioning from local archives have been
run by hand, and the shell steps of `verify` and `publish` have been run on a
local clone with `cargo publish --dry-run`; the rest is unproven until the
first hosted run. Remove an item from this section once a hosted run has
exercised it.

Carried over from the previous CI, which ran on hosted runners:

- the source-map schema, the GitHub asset request and its headers (the code
  around them is rewritten);
- the installer invocations: `pkgutil --expand-full` on macOS and the silent
  Windows installer flags;
- the Seatbelt policy and the loopback discovery configuration;
- the Windows firewall cmdlets;
- the Linux network namespace with a private `/etc/hosts` (its other `bwrap`
  options differ), the user-namespace `sysctl` and the i386 packages;
- the Windows LLVM installation, the Apple mobile deployment targets and SDK
  roots, and the nightly pin for the tvOS simulator;
- the crates.io authentication action.

New, and so unverified anywhere but Linux x86_64:

- every macOS and Windows step as a whole, including the wrappers used as Cargo
  runners (`pwsh -File` on Windows, `sandbox-exec` without a packet filter on
  macOS) and the build against Xcode's libclang. In run 43332050213 the Windows
  wrapper started `check.py` and the integration-test executables under
  `target\`, but no doctest (see the Windows point under
  [Loopback-only test runs](#loopback-only-test-runs)); the long-path
  expansion that answers this has not yet run on a hosted runner;
- the network check on macOS and Windows: that a refused connection or datagram
  fails at once there (with a permission error) instead of timing out. The first
  hosted run showed that the unconfined macOS runner cannot send to the mDNS
  group (the datagram is refused), which the `--online` control now reports as a
  limitation; the unconfined Windows runner reached both the public address
  and the mDNS group in run 43332050213. The Windows run of the loopback-only
  check (run 43318507917) showed that the public TCP connections are refused at
  once inside the wrapper but that the mDNS datagram is `reached`, because the
  firewall discards it silently; the check reports that as a limitation on
  Windows, and run 43332050213 passed that way on every Windows row;
- the Windows wrapper's SDK log with the Standard runtime: run 43342188690
  printed the Advanced runtime's log after a failed executable, but the
  Standard 6.4 row's failed executable printed none;
- the distinct loopback addresses that `tests/common` gives receivers on
  Windows, and `tests/peer_recreation.rs` with its control, port probe and TCP
  tables there (see
  [Receivers that never connect on Windows](#receivers-that-never-connect-on-windows));
- every promise of the Windows and macOS rows, in particular `file` beside a
  Standard runtime in the Windows x64 Standard 6.4 row;
- the Advanced lint on Windows and macOS;
- the Linux x86 and Linux arm64 rows;
- the compatibility runs on Windows and macOS, including the peer process under
  their wrappers, and on every platform the hand-off of the baseline values
  through the files under `$RUNNER_TEMP` and the step outputs;
- provisioning the 6.1.1 and 6.3.2 baseline packages in CI;
- installing the NDI|HX driver in CI, from its source-map entry, and the
  `h264-decoder` and `hevc-444-decoder` promises on Linux, which have passed by
  hand against a local copy of the Advanced 6.4 runtime with the driver beside
  it and the system FFmpeg hidden;
- building tvOS and visionOS on the stable toolchain;
- the Android example link in `cross`;
- access to the secrets and the download from the private store;
- how Dependabot's runs appear to the workflow (`github.actor`), and the skip
  of the `sdk` job for them;
- the semver check once a release with the crate's major version exists (until
  then it is skipped);
- the release path: the `crates-io` environment, trusted publishing, passing
  the digest and toolchain between jobs, and reproducing the package in the
  `publish` job.
