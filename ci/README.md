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
3. on Linux and macOS, proves that the test runner isolates
   ([`offline/check.py`](offline/check.py) outside and inside it). Every step
   after this one runs whenever this one passed, whatever the steps in between
   did, so a run reports everything that is wrong with a row and not only the
   first thing; none runs when the runner was not shown to isolate;
4. runs `cargo test --locked --config ci/offline.toml --target <target>
   --no-fail-fast` twice, with `--all-features` and with
   `--no-default-features`. Every test binary runs, so a failing one does not
   hide the outcomes of the rest, and every step that runs tests has a timeout
   of its own, well inside the job's (20 minutes for the all-features run,
   which passes in 7), so a test that never returns fails its step with the
   binaries before it reported;
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
     described above;

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
plugins, but H.265 only in 4:2:0 and where decoding hardware supports it. On
the hosted runners the Windows runtimes decode H.264, 6.3.2 and 6.4, x64 and
x86, and deliver only placeholders for HEVC 4:4:4, probably for want of a GPU;
the macOS runtime decodes both. So the Windows Advanced rows promise
`h264-decoder`, and no Windows row promises `hevc-444-decoder`.

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

Tests against a real runtime create NDI senders and finders. On Linux and
macOS, `ci/offline.toml` makes Cargo start every test executable through a
wrapper in [`offline/`](offline) that leaves it loopback networking only, so
nothing is announced on, or reached over, the network the machine is attached
to. Doctests are included: the current stable Cargo hands the same runner to
rustdoc, which starts each compiled doctest through it, with or without
`--target`. Windows has no such wrapper (see [Windows](#windows)).

| | Linux ([`linux.sh`](offline/linux.sh)) | macOS ([`macos.sh`](offline/macos.sh)) |
|---|---|---|
| Network | own network namespace (`bwrap --unshare-net`) holding only `lo` | Seatbelt profile allowing only binding, loopback connections and traffic arriving on a local address |
| Host daemons | `/run` replaced by an empty tmpfs: no D-Bus, no Avahi | mDNSResponder socket denied |
| SDK networks | only `lo` exists | a loopback discovery address instead of multicast DNS, through `NDI_CONFIG_DIR` |
| Machine's own name | resolves to `127.0.0.1` through a private `/etc/hosts` | cannot be relied on to resolve |
| `TMPDIR` | private tmpfs (`/run/tmp`) | private directory, removed afterwards |
| Needs | `bubblewrap`, unprivileged user namespaces | nothing |

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

The Linux and macOS rows run [`offline/check.py`](offline/check.py) outside and
then inside the wrapper. Inside, IPv4 loopback TCP and UDP must work, and the
operating system must refuse at once a TCP connection to each of three public
addresses and a UDP datagram to the mDNS multicast group that NDI discovery
uses; a timeout, or any answer from a remote host, fails the check. Outside, the
same attempts must succeed, which shows that the refusals are the wrapper's
doing. There is one exception: the hosted macOS machines refuse the multicast
datagram of the unconfined process itself (`refused`, as inside the wrapper). The
control then prints, on stdout, that this is a limitation and does not fail;
the refusal of the mDNS group inside the runner shows nothing about the wrapper
on such a machine, and only the public TCP connections attribute the refusals to
it. A timeout or any other outcome of the datagram still fails the control. The
check exercises the one process the wrapper starts. It does not show what the
points above say a wrapper leaves open.

Two parts of the wrappers exist because of how the SDK behaves:

- **The machine's own name.** A sender advertises itself as `<host name>
  (<sender name>)`, and looking a source up by host name asks the SDK to resolve
  that name. Only loopback exists in the namespace, so the name has to resolve
  to `127.0.0.1`: with the machine's usual address, or with no answer, the
  host lookup returns no sources. The wrapper
  therefore binds a private `/etc/hosts` that maps the name, and the name with
  a trailing dot, to loopback. One test depends on this,
  `tests/video_padding_sdk.rs`, and it is compiled for Linux only; every other
  test addresses its sender as `127.0.0.1:<port>`. The macOS wrapper does
  nothing about name resolution.
- **`TMPDIR`.** The SDK reserves every sender and receiver name machine-wide
  with a lock file, `$TMPDIR/NTK_SEND_<sender name>`. In a shared directory two
  unrelated processes that use the same sender name fail each other with
  "Failed to create NDI send instance", and the files are left behind. Both
  wrappers give each run a `TMPDIR` of its own.

### Windows

The Windows rows run the tests against the real runtime without a wrapper, as
on any Windows machine. Confining them would change what the SDK does: a
Windows receiver needs multicast DNS, and with multicast DNS blocked, or with
a discovery server it cannot reach configured in its place, receivers may
never connect, even to an explicit loopback address. The tests are the same on
every platform, and the Linux and macOS wrappers show that they need no
network.

Tests leave out what a Windows runtime cannot do, and print that they did: the
genlock tests on the 6.4 Advanced runtimes, whose genlock acquires no
reference, and the PQ test of `tests/image_color_sdk.rs` on the Standard
runtimes, which give an RGB receiver nothing of a P216 frame tagged PQ.

When a receiver does not connect, the failure messages of
`tests/peer_recreation.rs`, `tests/advanced_runtime.rs` and
`tests/receiver_video_only.rs` carry the Windows TCP table of the sender's port
(`common::socket_table`). A row from the receiver to the port that is
`ESTABLISHED` while the sender counts no connection is a connection nobody
services; `SYN_SENT` is a connection attempt nobody answers; and no row at all
is a receiver that has not tried.


## Releasing

1. Merge the version bump to `main`.
2. Tag that commit `v<version>` and push the tag.

`publish.yml` runs for `v*` tags only, in four jobs that each need the one
before. It has not run yet: the 3.0.0 release is its first run.

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
