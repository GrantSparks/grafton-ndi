# Hosted SDK validation

All validation jobs use GitHub-hosted runners. Set the repository variable
`NDI_CI_RELEASED=true` to enable SDK jobs in a trusted repository context.
SDK archives, private source locators, credentials and restricted generated
bindings stay private; jobs share only bounded result records and the public
inputs described below. Pages deployment and crate publication have separate
[publication gates](#publication-gates).

This guide defines CI operation and required coverage. Completed checks and their
limits belong in the [SDK validation record](../docs/sdk-validation.md).

`build_profiles.tsv` describes the exact vendor artifact selections. `plan.py`
joins those profiles with execution policy. `batch.py` executes a runner group,
and `run.py` executes one check locally or on its declared hosted runner.
`aggregate.py` requires every planned result for
the same clean commit/tree, suite fingerprint, dependency lockfile, SDK archives
and cross-toolchain package recipes. Missing, skipped, cancelled, stale or duplicate
results fail. Local extracted-SDK overrides are useful for development but cannot
satisfy the hosted release gate.

The required plan has **52 checks in 16 SDK runner jobs**, covering 20 supported
profiles. Each check keeps its own SDK identity, compiler, coverage and result record;
grouping related checks shares setup without turning several checks into one pass.
Current Standard and Advanced artifacts for every supported profile receive
independent C/Rust ABI checks and a final vendor link or application package.
`SUPPORTED_PROFILES` in `plan.py` declares the supported set. The other 19 retained
vendor profiles are unsupported candidates; archive presence alone does not add
platform support.

Full native suites run on Linux x64, Windows x64 and macOS ARM64, in both SDK
editions. The same three representatives exercise Standard compatibility in both
directions: baseline headers with the current runtime, and current headers with
the baseline runtime. Linux x64 checks the minimum Rust version and optional
features against baseline and current SDKs, plus public API compatibility. One
Android ARM64 baseline check covers its minimum Rust toolchain. This samples the
distinct loader, ABI and packaging families without repeating every SDK/toolchain
combination on every architecture.

| Required job groups | Jobs | Checks |
| --- | ---: | ---: |
| Native Linux x64, Windows x64 and macOS ARM64, Standard and Advanced | 6 | 6 |
| Additional Linux, Windows, Apple and Android profile families, including Android MSRV | 6 | 35 |
| Standard header/runtime compatibility on the three native representatives | 3 | 6 |
| Linux MSRV, optional features and public API compatibility | 1 | 5 |
| **Total** | **16** | **52** |

The default `required` scope gates pull requests and publication. An explicit
`extended` scope retains the broader 82-check audit for SDK upgrades or targeted
investigation. It is available on demand and does not replace the required release
record. Select the `scope` input when manually dispatching **Rust CI** to run an
extended audit. Neither scope removes checks when dependencies are missing or
tests fail.

Dynamic targets also link a C smoke program against the selected vendor library;
compiling the Rust runtime loader alone does not exercise that link interface.
Apple device/simulator checks link the real static archive through Cargo. Current
headers additionally require independent production Discovery and File ABI probes.

## Coverage and qualification

A successful `abi-link` cell is **not** native, simulator or emulated execution.
Mobile cells first build the library with all features and prove its target ABI,
then link the real application described below. Desktop CLI examples are built
by the other cells; their `ctrlc`/`nix` process dependency requires APIs absent on
tvOS and is not part of the library's mobile interface. No library features or
required mobile package/runtime checks are omitted by this distinction.
`mobile.py` packages real Rust JNI Android APKs for all four ABIs and checks
signing/ZIP alignment. The required plan executes the Standard x64/API35 emulator
as its Android runtime representative. Apple profiles link UIKit app bundles;
the Standard Intel tvOS check boots, installs and runs on a matching simulator.
Those runtime checks initialize the selected provider, send a synthetic frame and
destroy the sender. Other mobile checks require package/link coverage and do not
claim execution. The extended audit also runs the other declared emulator and
simulator combinations.
These fixed 16×16 synthetic-frame smoke tests run on disposable hosted virtual
devices. They do not use the desktop confinement executor or prove that off-host
networking is blocked. They establish the declared lifecycle/runtime behavior,
not private-LAN isolation or physical-device networking behavior.

Emulator page size and OS image identity are recorded; a 4KiB emulator pass is not
a 16KiB runtime pass. Device profiles require package/link coverage without
claiming device execution. QEMU execution is not implemented or claimed.

Every native and compatibility cell runs the 39 registered core SDK tests across
six executables under confinement. The two feature cells run the same core tests
for each of the nine existing feature selections, preserving `--no-default-features`
and the exact selected features. Reports bind every execution to its feature
selection; an all-features result cannot replace a smaller configuration. This
adds execution coverage within the existing 16 jobs and 52 checks.
The source inventory is [`core_tests.py`](core_tests.py). The native helper accepts
`--mode core --feature-set all` for the normal all-features configuration, or
`--feature-set 0` through `8` for the ordered `FEATURE_SETS` in
[`common.py`](common.py). Adding an SDK-dependent test requires updating that
registry and marking it for confined execution.

SDK-dependent tests are explicitly ignored by ordinary `cargo test` and selected
by the core registry. The core runner checks the complete compiled ignored-test
inventory, accounting for known manual benchmarks, manual diagnostics and
feature/platform-specific File tests without executing those as core tests.
The non-Windows DLL diagnostic retains its existing no-op behavior; it is not
evidence of a Windows DLL load. Pure tests and runnable doctests remain in ordinary
Cargo execution, while examples that initialize SDK resources are `no_run`.
Trybuild success fixtures check resource APIs in uncalled functions.
CI points ordinary Cargo test commands at a fresh empty runtime directory. The
default loader cannot fall back to the installed SDK, preventing accidental
default-runtime loads. This is a loader guard, not an OS sandbox. Explicit fake-library loader
fixtures still select their own modules. Native suites and runtime probes retain
the exact selected real runtime.

Advanced native cells require actual constructor/control/queue/allocator tests.
File-capable native cells require probe/player, async owner and record/finalize
roundtrips. Standard POSIX native checks run explicit rejection tests; the
extended Windows x86 native checks verify platform rejection too. Each ignored
test runs in a separate process and must report exactly one executed pass.
Linux uses a network namespace; Windows uses process firewall rules. macOS uses
the `macos-pf-offline` executor described below. The aggregate checks the exact
core and Advanced/File mode reports as well as the main regression report; an exit code
alone does not replace the required executed-test records.

Native media regressions require ten tests across five suites on Linux and nine
tests across four suites on macOS and Windows. The live padded-video regression
is Linux-only: it checks Linux SDK output padding and supports Memcheck reads
through `/dev/null`. Each platform checks its exact compiled ignored-test
inventory before execution; unavailable, extra, skipped or duplicate tests fail
the gate rather than counting as passes. `run.py` captures native helper failures
privately and exports only bounded diagnostic summaries. Direct `native.py`
output can contain SDK diagnostics and must remain private.

On disposable GitHub-hosted macOS runners, an independent root guardian briefly
replaces the reviewed initial PF rules with a loopback-only, no-state policy for
the whole VM. It verifies the installed rules, interface filtering and cleared
states, then proves that established and fresh external IPv4 connections are
blocked. IPv4/IPv6 loopback TCP/UDP and anonymous socket pairs must work. An
external IPv6 canary is explicitly not claimed. Only then does the unprivileged
worker execute the selected test under its process sandbox. The SDK worker has a
30-second limit and the offline window has a 45-second ceiling; successful tests
restore connectivity as soon as the checks complete.

Before restoring connectivity, the guardian must reap the SDK worker, canary and
local commands. It restores the exact original filter, releases its own PF
reference, checks the original disabled state, and verifies a fresh HTTPS
connection. A short online interval separates windows. Failure to establish the
boundary, stop the processes or restore the host fails the check, even if the SDK
test itself passed. Unreviewed initial PF state and non-hosted machines are
rejected; this helper is not a general local-machine firewall tool.

Each macOS test, ignored-test listing and fixed runtime-identity probe exports a
closed receipt binding its operation and executable hash to the boundary,
loopback, cleanup, restoration and recovery checks. The aggregate requires those
receipts for native and compatibility runtime probes and every applicable native
SDK suite, including negative File tests. Global drop counters establish boundary
activity; they do not identify SDK packets. Raw PF state, host addresses, source
paths and SDK output remain in private scratch. Only bounded receipts and log
hashes enter result artifacts. Runtime identity exports only four numeric version
components, the selected edition and its verification flag; missing, duplicate or
malformed probe markers fail validation. Runtime probes use the same platform
confinement as the native SDK tests. Compilation, pure unit tests and pure runnable
doctests retain their ordinary execution environment.

After a native test failure, fixed socket diagnostics may run through the same
executor. They cannot turn the original failure into a pass or substitute for
the required native test evidence.

Physical codec, SoC and peripheral qualification is a separate manual private-LAN
activity; it does not require a private GitHub runner. An unavailable SDK slice or
Rust target is not converted into a successful skipped row.

The declared Apple profiles cover supplied usable device/x86 simulator slices.
No arm64 simulator slice is supplied, and the vendor x86 visionOS slice lacks a
built-in Rust target. Those intersections are unavailable. Intel tvOS requires
a qualified dated nightly with `build-std`; ARM64 tvOS/visionOS use Rust 1.98.0
independently of the Rust 1.88 baseline.
The four selected ARMv7 GNU Linux profiles use a reviewed public cross compiler
and glibc 2.36 sysroot recipe. They prove ABI and linking only; they do not
establish the oldest supported glibc or physical Raspberry Pi generation. The
`rpi1` vendor artifact is ARMv7, so its directory name does not establish ARMv6
hardware compatibility.

The SDK matrix covers **6.3.2 Standard plus current 6.4 Standard and Advanced**.
`COMPATIBILITY_BASELINES` declares this policy explicitly; catalog availability
never shrinks the matrix. The catalog contains 12 complete platform bundles.
The library's compatibility target is NDI 6.x; this matrix does not provide
validation for SDKs 6.0 and 6.1.1.

Required target toolchains come from public distribution repositories or the
Rust/Xcode/Android SDK channels. SDK acquisition uses the private source and
credential configuration described below. Every required artifact and toolchain
must be available; missing inputs fail the release gate. Changes to vendor archive
bytes require reviewed catalog digests. Aggregate acceptance requires every check
in the selected, reviewed scope and all 12 exact package receipts. Policy tests and workflow syntax checks alone cannot
satisfy it.

| Selected profiles | Required hosted coverage |
| --- | --- |
| Linux x64; Windows x64; macOS ARM64 | Standard/Advanced native suites, ABI and vendor link; Standard compatibility in both header/runtime directions |
| Linux x86/ARM64; Windows x86; macOS Intel | Standard/Advanced C/Rust ABI and final vendor link; no required native execution |
| `arm-rpi1`/`arm-rpi2`/`arm-rpi3`/`arm-rpi4` GNU hard-float artifacts | Standard/Advanced C/Rust ABI and vendor executable link using public GNU tools |
| Android ARM64/ARMv7/x86/x64 | Standard/Advanced APK packaging and ABI/link; Standard x64 emulator execution; Standard ARM64 baseline MSRV |
| iOS ARM64 and x64 simulator | Standard/Advanced package/link; no required simulator execution |
| tvOS ARM64 and x64 simulator | Standard/Advanced package/link; Standard x64 simulator execution |
| visionOS ARM64 | Standard/Advanced device package/link |

The Linux quality group runs Rust 1.88 and optional-feature checks with Standard
6.3.2 and current SDKs. Public API compatibility is checked on Linux x64. The
required plan does not claim an MSRV or runtime result for every target; additional
native architectures and broader compiler/SDK permutations are in the extended
audit. Physical devices require their own qualification.

The unsupported candidates are the seven AArch64 HiMix/Mix/NewTek/Rockchip
profiles and twelve ARM HiMix/HiSilicon/HiSTB/NewTek/Rockchip/SigmaStar profiles
listed explicitly in `UNSUPPORTED_PROFILES`. They need a separately reviewed,
obtainable and qualified compiler/sysroot before promotion. No proprietary BSP
secrets are required for the selected release. A newly discovered artifact also
fails policy review until it is explicitly classified; support never follows
archive presence automatically.

## Artifact sources and availability

`ci/sdk-packages.json` is the shared authority for GitHub CI and the development
store. Package IDs such as `current-standard-linux` are selection aliases. Each
resolved package has an immutable `artifact_id` containing version, edition,
platform and the complete archive SHA256. Known digests cannot be overridden by
environment variables. Adding a future retained version requires an explicit
catalog and validation-policy update; local archive availability cannot add or
remove a supported validation row.

Use a private schema1 source map outside either checkout. Local development can
select an existing archive; GitHub CI can select a private release asset:

```json
{
  "schema": 1,
  "packages": {
    "current-standard-linux": {
      "archive": "/private/sdk-downloads/vendor-installer.tar.gz"
    },
    "current-advanced-linux": {
      "github_release_asset": {
        "repo": "OWNER/PRIVATE_REPOSITORY",
        "asset_id": 123456,
        "token_env": "NDI_SDK_ARCHIVE_TOKEN"
      }
    }
  }
}
```

An entry can instead contain `{"url_env":"PRIVATE_SDK_URL"}` for a permitted
HTTPS endpoint. It selects exactly one source. Never put raw tokens in a map, or
commit a private map, SDK archive, receipt inventory or extracted SDK. Alias and
immutable-artifact source entries cannot both select the same package.

For hosted jobs, configure two separate repository secrets:

- `NDI_SDK_SOURCES_JSON` is a **credential-free locator manifest** containing
  repository names, release asset IDs, or environment-variable names. Keep it
  secret because it identifies private artifacts and storage locations. It
  contains no authentication token and does not grant access by itself.
- `NDI_SDK_ARCHIVE_TOKEN` is the **authentication credential**: a fine-grained PAT
  restricted to the private archive repository with Contents: read. It is never
  included in the locator manifest, retained archives or SDK store receipts.

Store the source manifest as compact, single-line JSON. GitHub masks individual
lines of multiline secrets; a line containing only a JSON brace can obscure
unrelated output. Matrix generation runs in a separate job without acquisition
secrets, followed by a credentialed source check against the same frozen plan.
Only that verified plan can start the grouped acquisition and validation jobs.

GitHub requires Contents: read for release
asset downloads; metadata access permits checking that the exact configured
repository remains private. The configured workflow uses that narrowly scoped
PAT. Do not duplicate a broad development/write credential.
The provisioner authenticates only to `api.github.com`, requests octet-stream
content, strips Authorization/Cookie on every redirect, requires HTTPS throughout,
and verifies the archive hash. Private repository names, asset IDs and token
values are not exported in result receipts. See the official
[release asset API](https://docs.github.com/en/rest/releases/assets#get-a-release-asset).

Package sources can also use `NDI_SDK_<PACKAGE>_URL` secrets. Punctuation becomes
underscores and letters become uppercase, for example
`NDI_SDK_CURRENT_STANDARD_LINUX_URL`. Do not configure that URL and a source-map
entry for the same package. All active package hashes are pinned in the catalog;
`NDI_SDK_<PACKAGE>_SHA256` overrides cannot replace a reviewed catalog identity.

The plan collects **all** missing source, digest and public-toolchain recipe
prerequisites in one bounded configuration report without making network requests.
After that gate, each SDK runner group fetches, hashes, extracts and inventories
its packages before use. Checks in a group reuse only verified installations;
there are no separate package-preflight runners. Each of the 12 packages has one
designated availability receipt owner, and every check still records the exact
headers and runtime it used. A successful configuration check is not a successful
availability check. The final aggregate requires all exact package receipts and
all individual check results.

Rust setup is also reused within a group. Each check compares the compiler's
reported identity and locally installed component/target inventories before
reuse, avoiding repeated toolchain downloads after successful setup. New targets
or components still require installation; missing previously verified state
fails the check. Per-check results retain that setup evidence and bounded SDK
provisioning diagnostics, while detailed acquisition logs remain private.

Only bounded reports cross jobs; SDK archives and extracted SDKs are not Actions
artifacts or shared caches. Each runner group independently acquires and verifies
its packages, so a stale or replaced remote asset cannot silently change a build.

`provision.py --offline --sources /private/sources.json` forbids network access.
Installation is transactional. Caller-owned archives are copied into private
staging, hashed there, consumed from that snapshot and checked again after
extraction; changing the original path cannot change verified input. Acquisition
credentials, including custom names from an explicit private source-map file,
are removed from installer and compiler environments. A completed receipt records hashes for the selected
include/lib/bin trees and Version.txt; `--reuse` rehashes them before use. Version
comparison is exact after padding trailing numeric zeros (6.3.2 equals 6.3.2.0),
not substring matching. Windows installers lack Version.txt and explicitly use
archive-bound version evidence instead of claiming a parsed installed version.
The portable [development SDK store](sdk-store.md) retains original archives and
side-by-side installations; GMS mounts that populated store read-only at runtime.

The four supported GNU cross profiles use `public-toolchains.json` and
`public_toolchain.py`. Every public compiler/sysroot dependency is pinned by
SHA256. The prepared toolchain records its reviewed recipe, verified packages,
profile-specific flags and configuration identity. The plan freezes that recipe
identity, and the aggregate rejects mismatched toolchain evidence. Replacing an
artifact profile with another one sharing the same Rust target is forbidden.

The Intel tvOS compiler pin, `nightly-2026-11-10`, is part of the checked-in CI
policy, independently of the desktop/Android Rust MSRV. Its checks require a target
standard-library build and an Apple SDK link. The Standard check also requires
matching simulator execution in the required scope; both editions execute in the
extended scope.
Cargo's JSON artifact messages identify generated bindings, libraries and rebuilt
std metadata without assuming a particular Cargo output-directory layout.
Compiler changes must pass the same checks before promotion.

All CI jobs use the same generated `Cargo.lock`. The aggregate rejects evidence
from a different dependency resolution. SDK archives, extracted libraries,
generated bindings, target directories, linked SDK code, and raw diagnostics are
not uploaded or cached. Only the plan, lockfile, bounded check results and package receipts cross
job boundaries. On failures, raw diagnostic logs remain on the ephemeral runner;
reproduce the cell locally to inspect vendor-containing diagnostics privately.

Untrusted fork PRs receive SDK-free policy/tooling/format checks, with no SDK
secrets. The full required gate deliberately remains unsatisfied until a reviewed
commit runs in the trusted repository context. Do not use `pull_request_target`
with an untrusted checkout. The obsolete setup actions are not used by this
workflow; their SDK caches are not a confidentiality boundary.

## Local reproduction

Use Python 3.11+, LLVM tools, the selected Rust toolchain and appropriate sysroot.
Keep outputs outside the checkout. For a local plan and verified archive extract:

```sh
python ci/plan.py --output /private/ci-plan.json
python ci/provision.py --package current-standard-linux \
  --archive /private/vendor-installer.tar.gz --output /private/sdk-extract
python ci/run.py \
  --cell x86_64-linux-gnu-standard-native-current-current-stable \
  --header-sdk /private/sdk-extract/unpacked/NDI\ SDK\ for\ Linux \
  --output /private/validation
```

To reproduce a whole runner group, use a clean checkout and the configured
verified archive sources:

```sh
cargo generate-lockfile
python ci/plan.py --scope required --output /private/ci-plan.json
python ci/batch.py --plan /private/ci-plan.json --group native-linux-standard \
  --output /private/ndi-ci --install-toolchain
```

The plan lists the available group IDs. For the broader audit, generate a plan
with `--scope extended`; pass the same scope to `ci/run.py` when reproducing one
of its individual checks. `batch.py` reads the scope from the frozen plan.

Batch output separates `evidence/cells/CELL/result.json` and
`evidence/availability/PACKAGE/availability.json` from private `cells/` logs/builds
and `sdks/` installations. Only the `evidence/` directory is eligible for the
workflow's bounded result artifact. `--evidence DIR` can select a separate output
directory; it does not make raw build or SDK directories uploadable.

A native suite initializes NDI and may advertise synthetic sources. `native.py`
requires working unprivileged `bwrap` network namespaces on Linux or an elevated
PowerShell session with Windows Defender Firewall control on Windows. Its macOS
executor accepts only disposable GitHub-hosted runners; an ordinary local Mac or
private LAN does not satisfy that executor's preconditions. Independent manual
device testing belongs on an authorized isolated private LAN. Cross/link
suites do not execute target programs. For local cross builds only,
`NDI_LOCAL_TOOLCHAIN` can name an extracted toolchain bundle; this intentionally
cannot produce release-grade archive evidence.

```sh
python -m unittest discover -s ci/tests -v
python -m unittest discover -s tools/sdk -v
python tools/sdk/audit.py emit-capabilities --check
```

For the next SDK, inventory exact artifacts, update reviewed package digests and
profile metadata, snapshot/classify API changes, update capability dispositions,
then regenerate the plan. New profiles require an explicit support-policy classification. Promoting a
candidate requires obtainable tools and mandatory ABI/link coverage, plus native,
simulator or emulator checks where feasible; execution policy changes require
explicit review. Never mark an API implemented
or downgrade a cell's coverage merely to make the release gate pass.

Apple simulator execution does not test device privacy prompts or multicast
entitlements. Device qualification must cover those separately; Apple documents
that the simulator does not implement local-network privacy in
[TN3179](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy).

Linux i686 uses explicit multilib libc/libstdc++ and Avahi dependencies. Required
CI proves its ABI and final links. Native-on-x64 execution, compatibility and
32-bit File behavior are available in the extended audit.

## Publication gates

`rust.yml` calls `publish.yml` only after `required-ci` passes for a trusted
`main` push/manual run or version tag in the `required` scope, and only with
`NDI_CI_RELEASED=true`. An extended audit cannot publish.
Pages deploys from `main`; crate publication requires a version tag.
Publication rechecks the same run's plan, aggregate evidence, clean commit/tree,
suite fingerprint and dependency lockfile before provisioning its SDK. A tag
must exactly match the Cargo package version (with an optional `v` prefix).
A crate release requires an unpublished package version and reviewed release
notes. Crates.io trusted publishing and GitHub Pages must be configured for the
repository before their corresponding publication can succeed.
For crates.io, verify the caller filename `rust.yml` in its trusted-publisher
configuration: the current registry matches the token's `workflow_ref`, rather
than automatically using the reusable `publish.yml` filename. See its
[GitHub claim implementation](https://github.com/rust-lang/crates.io/blob/6cd78f2c7b09da12f6d7b3f70970b6d4d805a5b1/crates/crates_io_trustpub/src/github/claims.rs).

Only the reviewed public API documentation and crate package may be published.
Documentation omits source pages, and the publication helper checks generated
output and package contents before upload. SDKs, generated private FFI, original
archives, private source maps and raw diagnostics remain excluded. The narrowly
scoped archive token is used for SDK acquisition, not for publication.
