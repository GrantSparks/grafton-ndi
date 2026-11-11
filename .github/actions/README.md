# Legacy NDI SDK setup actions

The composite actions in this directory are retained historical tooling. The
current [Rust workflow](../workflows/rust.yml) uses the shared `ci/` provisioner
and validation plan; it does not invoke these actions or trust their SDK caches.
Use [hosted validation](../../ci/README.md), the
[development SDK store](../../ci/sdk-store.md), and the
[SDK review tools](../../tools/sdk/README.md) for current maintenance.

## Current provisioning policy

The active catalog retains 12 reviewed platform bundles: four Standard 6.3.2
bundles and eight Standard/Advanced 6.4 bundles. Future retention is prospective;
collecting earlier SDK releases is not required. NDI 6.0/6.1.1 remain unvalidated
by the current change, without a new minimum-SDK declaration.

The active workflow uses only GitHub-hosted runners. Its release matrix is limited
to profiles with obtainable SDK/toolchain inputs and the applicable hosted checks.
Required validation groups 52 individually recorded checks into 16 SDK runner
jobs: representative native and compatibility suites, plus ABI/link or packaging
for the other supported profiles. The optional extended audit retains broader
combinations. Package verification runs within these groups, with all 12 exact
availability receipts still required by the aggregate. Vendor BSP entries can
remain inventoried candidates outside the supported matrix. Configured jobs,
archive availability and installer hashes do not prove that any native, emulator,
simulator or device test ran.

Private source-map locators and authentication tokens are separate inputs. Keep
both out of tracked files and public diagnostics. The provisioner verifies exact
archive bytes and extracted contents. A content identity based on SHA256 does not
make a GitHub release asset immutable or prevent its deletion. SDK payloads are
not public release attachments, Actions artifacts or shared Actions caches. See
the current provisioning guide for the precise credential and receipt contracts.

## Historical 6.3.2 installer record

These hashes were recorded on 2026-10-16 for the 2.0 release workflow. Preserve
them as historical evidence; they are not the active package catalog or a 6.4
qualification result. The Android Windows installer was inspected but was not
used by that CI workflow.

| Installer | SHA256 |
| --- | --- |
| Windows | `4D5DD36A1C7C7634F408BF459B068787CCE6F5310A3EFE832D76B1DDEB54E499` |
| Apple (macOS/iOS) | `5AA101A01B00494724C60B7E1E15B5D1743BF9C68B61739C22AC54932DDE95FB` |
| Linux | `F0314F245446DEFC488B63CEB4689ACF1A965AEEFDADACB70571BB216A8CC183` |
| Android, Linux installer | `895FE323F7329AF86DF5E0A8AD01F708D30362FF2771AE1179794DC1B4ED8668` |
| Android, Windows installer (download inspected; not used by CI) | `426D1709C05188CFDD230AC4D0B93EB6791571187C78DAE37AA6721ECC1B1E92` |


The legacy actions downloaded installers, checked a SHA256 allowlist and cached
installed SDKs. Windows also installed LLVM. macOS installed a package and copied
its SDK into the cache. Linux extracted the Linux/Android package and prepared
linker symlinks for shipped Linux architectures.

Their URL secrets were `NDI_SDK_URL` (Windows), `NDI_SDK_URL_MACOS`,
`NDI_SDK_URL_LINUX`, and `NDI_SDK_URL_ANDROID`. A changed URL alone did not invalidate
a cache or accept a new hash; the primary accepted hash contributed to the cache
key. Those actions did not authenticate private GitHub release-asset downloads.
The current provisioner has a separate, documented private-source mechanism.

The previous workflow also contained Pages/crate publication jobs. Their historical
behavior must not be treated as current release authorization or as proof that
new SDK jobs have executed. [SDK validation](../../docs/sdk-validation.md) retains
the earlier target results separately from this integration's evidence.
