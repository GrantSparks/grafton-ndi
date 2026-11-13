# NDI SDK setup and updates

The setup actions download installers, verify their SHA256 hashes, and cache the
installed SDK. Windows also installs LLVM; macOS installs the package and copies
its SDK into the cache. The Linux action extracts either the Linux or Android
SDK and creates Linux linker symlinks for every shipped architecture.

## Current installers

Verified on 2026-10-16 against NDI SDK **6.3.2**. The desktop hashes were already
the primary accepted hashes; no desktop URL-secret or hash update is needed for
this version. Android uses the Linux-hosted installer in CI.

| Installer | SHA256 |
| --- | --- |
| Windows | `4D5DD36A1C7C7634F408BF459B068787CCE6F5310A3EFE832D76B1DDEB54E499` |
| Apple (macOS/iOS) | `5AA101A01B00494724C60B7E1E15B5D1743BF9C68B61739C22AC54932DDE95FB` |
| Linux | `F0314F245446DEFC488B63CEB4689ACF1A965AEEFDADACB70571BB216A8CC183` |
| Android, Linux installer | `895FE323F7329AF86DF5E0A8AD01F708D30362FF2771AE1179794DC1B4ED8668` |
| Android, Windows installer (download inspected; not used by CI) | `426D1709C05188CFDD230AC4D0B93EB6791571187C78DAE37AA6721ECC1B1E92` |

See [SDK validation](../../docs/sdk-validation.md) for the tested targets and
cross-compilation instructions. Installer verification does not establish runtime
coverage for every platform in that installer.

## Keeping download links private

CI supports these **GitHub Actions repository secrets**, each containing a URL:

| Secret | Installer |
| --- | --- |
| `NDI_SDK_URL` | Windows |
| `NDI_SDK_URL_MACOS` | Apple |
| `NDI_SDK_URL_LINUX` | Linux |
| `NDI_SDK_URL_ANDROID` | Android for Linux |

The workflow has vendor-hosted fallback URLs when these secrets are absent.
Personalized links belong only in secrets, never in tracked files, issue/PR
bodies, or release notes. Keep downloaded SDKs outside the working tree.
The actions pass URL environment variables to the downloader without printing
them or interpolating them into shell source.

There is no installer-archive upload step in this repository's current CI. You
upload the **URL as a secret**, not the SDK binary. A private mirror must provide
a URL the downloader can access; these actions do not authenticate to private
GitHub release assets. Do not put SDK installers on public repository releases.

## Updating to a new SDK

1. Download the installers from NDI into a directory outside the repository.
   Confirm the installed `Version.txt`/runtime version, not just the installer
   filename: NDI updates payloads behind the same version-6 URLs.
2. Compute SHA256 on each complete download (`sha256sum`, `shasum -a 256`, or
   PowerShell `Get-FileHash -Algorithm SHA256`). Set the corresponding action's
   `primary` hash to the verified value. Retain prior hashes only when those
   older SDKs are intentionally still accepted. The first hash determines the
   cache key, so a new primary hash refreshes the cache.
3. If the URL changed, use **Settings → Secrets and variables → Actions →
   Repository secrets**, or run the relevant command below and paste the URL at
   the private prompt. A new SDK behind the same URL needs no secret change.

   ```bash
   gh secret set NDI_SDK_URL --repo GrantSparks/grafton-ndi
   gh secret set NDI_SDK_URL_MACOS --repo GrantSparks/grafton-ndi
   gh secret set NDI_SDK_URL_LINUX --repo GrantSparks/grafton-ndi
   gh secret set NDI_SDK_URL_ANDROID --repo GrantSparks/grafton-ndi
   ```

4. Run the native checks and cross-builds in the validation guide. Update the
   version/hash table above and describe coverage using platform names and
   hashes; do not include personalized URLs in the change description.
5. Run the `Rust CI/CD` workflow after merging. It checks native desktop
   platforms and links examples for all four Android ABIs. Publishing waits for
   Android, native tests, formatting, MSRV, and semver checks.

A URL change alone does not invalidate a cache or accept a new installer hash.
The allowlist is a compatibility mechanism: an accepted older payload can still
be installed if the configured URL serves it. To require exactly one SDK
version, remove its predecessors from the allowlist and use a version-pinned
source under your control.

## Release CI policy

The 2.0 workflow pins Linux jobs to `ubuntu-24.04`. Validate Ubuntu 26.04
separately before changing that release baseline. Linux-only checks use
`runner.os == 'Linux'`, and Rust-cache shared keys follow the selected runner
label. Native Windows and macOS coverage remains part of release acceptance.
Hosted macOS ARM64 queue-capacity notices describe scheduling constraints; allow
for queue time and verify the jobs' eventual results.

The workflow generates a lockfile before `cargo audit --deny warnings` and gates
publication on that audit. Checkout, SDK cache restore/save, Pages upload/deploy,
and crates.io authentication use Node 24 action releases. The Pages uploader
also uses a Node 24 nested artifact action; do not suppress deprecation warnings
with runtime override variables. The default archive excludes hidden files;
rustdoc's root redirect and API assets do not require hidden files.

## Preparing and publishing 2.1.0

PR #123 prepares 2.1.0 against the existing 2.x `main`. Obtain explicit approval
before merging it, creating or pushing a release tag, or publishing the crate.
The dated changelog is release preparation; no tag or publication is implied.
If publication happens on a later date, update the changelog date first.

Before approval:

1. Require a successful `Rust CI/CD` run for the PR's final commit. In particular,
   verify the receiver-status, async-send, independent-sender and FrameSync SDK
   regressions on Linux, Windows and macOS, plus all four Android example-link
   jobs, the Rust 1.88 job, audit, formatting and semver checks.
2. Verify the exact package locally with the installed Standard NDI 6.3.2 SDK:
   `cargo package`, `cargo publish --dry-run`, and
   `cargo semver-checks --baseline-rev v2.0.0 --release-type minor --all-features`.
   Neither packaging nor the dry run publishes anything. Confirm that the
   manifest says `2.1.0` and no `v2.1.0` tag already exists.
3. Confirm `main` still contains the 2.x API. This release is intended to precede
   PR #122's 3.0 merge. If the order changes, re-review the maintenance base,
   Pages destination and publishing configuration before proceeding.

After explicit merge approval, merge into `main` and wait for its CI and Pages
deployment. The 2.x API documentation remains at
`https://grantsparks.github.io/grafton-ndi/grafton_ndi/`, matching `Cargo.toml`;
the maintenance branch and PR do not deploy Pages. The crate still requires
installed SDK headers at build time, so this release uses Pages rather than
relying on a docs.rs build.

After explicit release approval, tag the approved release commit `v2.1.0` and
push that tag. The publishing workflow is **`.github/workflows/rust.yml`**,
using the `Publish` job on Windows after all its required jobs succeed. Its
`rust-lang/crates-io-auth-action` step supplies the publishing token; the
crates.io trusted-publisher configuration must name this workflow, rather than
3.0's separate `publish.yml`. No `crates-io` environment is specified in the
2.x job. Verify the resulting crates.io version and downloadable package after
publication. Keep SDK installers and private acquisition URLs out of release
assets and notes.
