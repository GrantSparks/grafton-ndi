# Private local SDK store

`sdk_store.py` retains reviewed original installers and extracted SDKs outside the
checkout. Retention, verification, listing and command selection never download
anything. SDKs remain in private storage; mounting the store does not authorize
publishing its contents.

The approved retained set contains 12 complete platform bundles: Standard 6.3.2
for Linux, Windows, Apple and Android, and Standard plus Advanced 6.4.0.2 for the
same four platforms. All 12 original archive identities have been verified and
retained privately. This does not mean every package has been installed or tested
on its target host. The policy applies prospectively to future SDK releases;
collecting earlier releases is not an active task or a release prerequisite.

The same canonical `sdk-packages.json` and `provision.py` used by CI validate archive
identity, edition, platform and exact SDK version. A private source map supplies
local archive paths without entering the store:

```json
{"schema":1,"packages":{"current-standard-linux":{"archive":"/private/vendor-linux.tar.gz"}}}
```

Source JSON describes where an artifact can be found; it is not an authentication
token. The shared provisioner can also use an authorized private release-asset
locator or a URL environment-variable name, as described in
[CI source configuration](README.md#artifact-sources-and-availability). Keep those
locators private and supply read credentials separately through the named secret
environment variable. Do not place token values in source JSON. The store's
`retain` and `populate` commands accept local files only; remote acquisition is a
separate, explicitly authorized step through the shared provisioner.

Remote acquisition retries only selected transient network errors, with at most
three attempts and 1-second then 3-second delays. Attempts share a 300-second
transfer-progress budget; urllib socket and DNS behavior means this is not a hard
wall-clock deadline. Authentication, TLS, privacy, archive identity, local I/O and
extraction failures are not retried. A private-asset retry checks repository
privacy again and discards partial downloads. Published bootstrap bundles keep
their embedded downloader until a newly reviewed bundle and hash pin are deployed.

For a fresh outer development host that can reach the private GitHub archive
repository, GMS can download a pinned portable bootstrap bundle and prepare the
host store before mounting it. The host needs Python 3.10 or later, GitHub CLI
authentication with read access to that private repository, and sufficient disk
space for the originals plus extracted SDKs. It does not need this source checkout
or a path inside an existing development container.

Maintainers create that private bundle locally with:

```sh
python3 ci/bootstrap_bundle.py --sources /private/ndi-github-sources.json \
  --output /private/ndi-sdk-bootstrap.tar.gz
```

This command does not download or publish anything. It accepts only private GitHub
release-asset descriptors using `NDI_SDK_ARCHIVE_TOKEN`, and rejects output inside
the checkout. It produces a deterministic, mode 0600 archive containing the four
portable store files, `prepare_store.py`, a credential-free `sources.json`, and
`bootstrap.json` with exact file hashes and package selections. The private source
map contains locators, so the bundle itself also belongs in private storage.

The outer host verifies the configured private repository, bundle size and SHA256,
then extracts only the seven expected regular files. The verified entrypoint runs
as `python3 BUNDLE/prepare_store.py --store HOST_CACHE_DIR`. It verifies every bundle
file before importing helpers, downloads each missing original through the shared
authenticated provisioner, retains all 12 originals, and populates the six Linux
and Android packages. Apple and Windows originals remain available for their native
host workflows. It strips acquisition credentials before store subprocesses;
the source map and bootstrap directory remain outside the mounted store. Existing
retained content is verified before reuse, and interrupted acquisition can resume
without downloading already verified originals. No SDK is initialized by bootstrap.

```sh
python3 ci/sdk_store.py --store /private/ndi-sdks retain current-standard-linux \
  --sources /private/ndi-sources.json
python3 ci/sdk_store.py --store /private/ndi-sdks populate current-standard-linux
python3 /private/ndi-sdks/tools/sdk_store.py --store /private/ndi-sdks verify
python3 /private/ndi-sdks/tools/sdk_store.py --store /private/ndi-sdks list --json
```

`retain` also accepts `--archive FILE`. It verifies the reviewed SHA256 both before
and after copying, then atomically preserves the original at
`archives/<sha256>.tar.gz`, `.pkg` or `.exe`. Retention supports every catalog
platform without running an installer. `populate` retains an explicitly supplied
archive first, or uses already-retained bytes. Linux and Android shell payloads
are extracted without executing their installers. Apple extraction needs native
macOS `pkgutil`; Windows installation requires a Windows host. Retaining an archive
does not claim that it has been extracted or tested on its target platform.

The index has package aliases plus immutable artifact IDs containing version,
edition, platform and full archive digest. Extracted SDKs live side by side under
`artifacts/<artifact_id>/`; switching an alias preserves retained older artifacts.
The index, receipts and manifests use relative paths. `tools/sdk_store.py` is a
stable portable launcher. The index atomically selects an immutable bundle under
`tools/generations/<digest>/` containing `sdk_store.py`, `provision.py`, `common.py`
and the reviewed catalog. A failed helper copy or index publication leaves the
previous generation active. Existing flat-helper stores migrate through a verified
legacy handoff, so an interrupted migration still supports read-only verification
and an ordinary retry. An extracted SDK stranded before index publication is
adopted on retry only after comparison with a fresh, verified extraction.
The private source map and source host paths are never copied into the store.

`verify` and `list` check every original archive, installed receipt and complete
extracted payload inventory. `exec` performs the same content checks for its selected
header/runtime pair, deduplicated when both select the same artifact. Unselected
retained payloads do not add command-startup I/O or affect the activated pair.
Every command checks the shared index references and portable helpers/catalog. These
commands work after relocation and do not write to the store, including Python
bytecode. A changed cached receipt, header, library, documentation file or helper
fails verification. This detects changes against retained local evidence; it is
not a vendor signature or protection against replacing the whole trusted store.

For local development, mount the complete store read-only and select both package
IDs explicitly for each command:

```sh
python3 /opt/ndi-sdks/tools/sdk_store.py --store /opt/ndi-sdks exec \
  --headers current-standard-linux --runtime 6.3.2-standard-linux \
  --build-root /private/ndi-builds -- cargo test --offline
```

Command selection currently supports native Linux x64, x86 and AArch64 GNU
profiles. AArch64 uses the reviewed artifact profile `aarch64-rpi4-linux-gnueabi`
from both SDK editions; it never guesses another vendor BSP directory. Headers
and runtime must be Linux artifacts of the same edition, and
the runtime ELF architecture must match the host. Android packages may be retained
and populated for a separate explicit cross-compilation workflow. `exec` rejects
Android, Apple and Windows runtime selections on Linux.

The command sets `NDI_SDK_DIR`, `NDI_SDK_EDITION`, `NDI_SDK_PROFILE` and
`NDI_RUNTIME_DIR_V6`; the selected runtime directory is first in `LD_LIBRARY_PATH`.
Inherited NDI library paths/selectors and preload injection are removed while
unrelated loader directories remain. Acquisition credentials, including custom
source-map token/URL environment variable names, are removed before the child
starts. Rust compiler wrappers are explicitly empty, including Cargo configuration
overrides; GMS/sccache/ccache reuse is disabled. Each immutable header/runtime pair
and native target receives a separate, mode 0700
`CARGO_TARGET_DIR` outside the checkout and SDK store. By default it is beneath
`~/.cache/grafton-ndi/private-builds`; `--build-root` selects another private writable
location. Existing globally installed SDKs and the base development image remain
unchanged.

No version is inferred from an alias such as `6.1`, a mutable download URL or a
SONAME. Unpinned historical packages cannot be retained or silently substituted.
Archive availability, successful extraction and target runtime qualification are
separate facts.

The Linux read-only store and mixed 6.3.2/6.4 command selection have local execution
evidence. Portable bootstrap tests use independent installer fixtures to check
bundle integrity, credential removal, interrupted acquisition and offline reuse.
GMS launcher tests check argument transport and selection policy; they
do not establish that a complete Docker/devcontainer image has been built and run
with every SDK. Hosted jobs, other native hosts, emulators and physical devices
retain their separate validation requirements. See the
[validation record](../docs/sdk-validation.md) for the current evidence.
