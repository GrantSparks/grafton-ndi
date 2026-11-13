# Contributing to grafton-ndi

`grafton-ndi` wraps the NDI SDK's runtime, discovery, capture, and sending APIs.
A useful contribution explains the application behavior it changes and preserves
the ownership and memory-layout guarantees around the SDK calls.

## Local setup

Default desktop builds need only Rust. With `advanced`, an installed Advanced
SDK triggers binding generation and requires `libclang`. Apple mobile builds
require the NDI SDK for Apple. See [build requirements](docs/setup.md#build-requirements).

Use current stable Rust for development. [rust-toolchain.toml](rust-toolchain.toml)
selects stable with `clippy`, `rustfmt`, and `rust-src`. The library's minimum Rust
version is 1.88; development dependencies and compiler-diagnostic fixtures use
current stable.

Start with:

```bash
cargo build
cargo test
cargo run --example NDIlib_Find
```

`cargo test` passes without an NDI runtime: the tests that need one print
`skipped: <test> needs an NDI runtime` and return. The discovery example needs
a runtime and a network.

### Getting a runtime for the real tests

Install an [NDI SDK](https://ndi.video/for-developers/ndi-sdk/) and tell the
crate where its runtime library is. On Linux x86_64, for example:

```bash
export NDI_RUNTIME_DIR_V6="/usr/share/NDI SDK for Linux/lib/x86_64-linux-gnu"
cargo test --all-features
```

`ci/sdk.py` unpacks a downloaded SDK archive into a new directory of your
choice without running the vendor's installer, after checking it against a
pinned hash, and prints the variables to export:

```bash
python ci/sdk.py install current-standard-linux --dest ~/ndi/standard-6.4 \
    --archive ~/Downloads/Install_NDI_SDK_v6_Linux.tar.gz --target x86_64-unknown-linux-gnu
```

It prints `NDI_SDK_DIR` and, with `--target`, `NDI_RUNTIME_DIR_V6` and
`GRAFTON_NDI_EXPECTED_RUNTIME`, `GRAFTON_NDI_EXPECTED_VERSION` and
`GRAFTON_NDI_EXPECTED_EDITION`. With those three set, every test that loads a
runtime checks that it loaded exactly that one, so unset them before switching
to another runtime. `python ci/sdk.py list` names the packages it knows. See the
[CI guide](ci/README.md#sdk-packages).

The [setup guide](docs/setup.md#finding-the-runtime) describes the full search order.
Tests against a real runtime create senders, receivers and finders, which
announce themselves on the local network. Run them on a network where that is
acceptable.

Four more things are optional and unlock more tests:

- An NDI **Advanced SDK**. Set `NDI_SDK_DIR` to its root at build time so the
  `advanced` feature can generate its declarations (this needs `libclang`), and
  point `NDI_RUNTIME_DIR_V6` at its runtime directory.
- The NDI **File library**, for the `file-io` tests. The SDK installs it beside
  the runtime library. On Linux and macOS it works only with the Advanced SDK
  runtime.
- The SDK's **`ndi-discovery-server`** program, for the end-to-end Discovery
  Server test. The test looks for it in the SDK that the selected runtime
  library belongs to, at `bin/<target>/ndi-discovery-server` beside
  `lib/<target>/`.
- An **NDI|HX video decoder**, for the test that decodes HEVC from an Advanced
  sender with a 6.4 runtime. Windows and macOS runtimes have one built in. On
  Linux the runtime decodes NDI|HX only through the NDI|HX Driver for Linux
  copied beside its runtime library, or through FFmpeg 7's
  `libavcodec-ndi.so.61` or `libavcodec.so.61` on the loader path; an older
  FFmpeg does not count. Without one, the test is skipped. To install the
  driver into an SDK that `ci/sdk.py install` unpacked, download it from NDI
  and run:

  ```bash
  python ci/sdk.py install-driver current-hx-driver-linux --sdk ~/ndi/advanced-6.4 \
      --archive ~/Downloads/NDIHXDriverForLinux.tar.gz --target x86_64-unknown-linux-gnu
  ```

  It checks the archive against its pin and refuses a runtime from another
  vendor build, since the driver works only with its own. The driver comes
  under the NDI|HX licence agreement, which the archive includes. On Linux, NDI
  grants no licence for the H.264 and AAC technology the driver uses, so make
  sure your use is licensed; the bundled FFmpeg libraries have their own
  licence as well. The driver needs `libcurl.so.4` on the system.

Keep SDK archives, SDK headers and SDK documentation outside the source
checkout.

## Checks before review

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --all-features
cargo build --locked --examples --all-features
cargo test --locked --all-features
```

Run relevant tests while developing. For API, feature, FFI or build changes,
follow the [additional checks](docs/development.md#checks-before-review).
Real-runtime tests may skip when their prerequisites are absent; report which
runtime, edition and platform you exercised.

## Maintenance references

- [Implementation and SDK contracts](docs/development.md): module ownership, bindings, safety and compile contracts.
- [SDK validation](docs/sdk-validation.md): measured SDK behavior and reproduction commands.
- [CI guide](ci/README.md): workflows, SDK provisioning, isolation and releases.

## Pull requests and documentation

Describe the concrete trigger, previous behavior, resulting behavior, and why
the change belongs in the wrapper. Include test results and any relevant SDK
contract. For public API changes, explain compatibility and allocation/blocking
costs, update rustdoc and affected examples, and add an appropriate entry to
[CHANGELOG.md](CHANGELOG.md). Update migration guidance when existing callers
must change their code. Prefer shared helpers over parallel implementations.

Every public item has rustdoc: what it does, its units, its error conditions
and its threading rules. The `advanced`, `capabilities`, `discovery`, `file`
and `routing` modules enforce this with `#![warn(missing_docs)]`.

Use a concise title describing the resulting change; existing commits commonly
use `fix:`, `feat:`, `docs:`, or a scoped form such as `fix(sender):`.
Contributions use the repository's [Apache-2.0 license](LICENSE).

Keep each user-facing fact in one authoritative guide and link to it elsewhere.
Describe current behavior in guides; put release comparisons in the changelog or
migration guides. Keep examples compilable and document errors, ownership and
blocking behavior beside the relevant API.
