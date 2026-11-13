# grafton-ndi

[![Crates.io](https://img.shields.io/crates/v/grafton-ndi.svg)](https://crates.io/crates/grafton-ndi)
[![Documentation](https://docs.rs/grafton-ndi/badge.svg)](https://docs.rs/grafton-ndi)
[![CI](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml/badge.svg)](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml)
[![License](https://img.shields.io/crates/l/grafton-ndi.svg)](https://github.com/GrantSparks/grafton-ndi/blob/main/LICENSE)
[![Minimum Rust Version](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](https://www.rust-lang.org)

`grafton-ndi` provides Rust APIs for discovering NDI sources and sending and
receiving video, audio and metadata. It supports NDI 6, with optional async
adapters, image encoding, Advanced SDK operations, and recording and playback.

## Install

```toml
[dependencies]
grafton-ndi = "3.0"
```

Use Rust 1.88 or later and install an [NDI 6 runtime](https://ndi.video/for-developers/ndi-sdk/).
Put the runtime library beside your executable or set `NDI_RUNTIME_DIR_V6` to
its directory. See [setup and supported platforms](docs/setup.md).

Default builds on Windows, macOS, Linux and Android need no SDK at build time.
Advanced native operations need Advanced SDK headers and `libclang`; Apple
mobile targets need a static SDK archive. tvOS and visionOS also need a later
Rust toolchain. See [build requirements](docs/setup.md#build-requirements).

## Supported platforms and NDI SDK versions

`grafton-ndi` 3.0 supports NDI 6 runtimes from the Standard and Advanced SDKs.
SDK version coverage is:

| NDI SDK version | Editions tested |
| --- | --- |
| 6.4.0.2 | Standard and Advanced; the crate's bindings use these headers |
| 6.3.2 | Standard and Advanced |
| 6.1.1 | Standard |
| Other NDI 6 releases, including 6.0 | Not tested |
| NDI 5 and earlier | Not supported |

Platform coverage varies by architecture and SDK edition:

| Platform | Architecture | Validation and SDK versions |
| --- | --- | --- |
| Linux (glibc) | x86-64 | Runtime tests: Standard 6.1.1, 6.3.2 and 6.4.0.2; Advanced 6.3.2 and 6.4.0.2 |
| Linux (glibc) | x86, ARM64 | Runtime tests: Standard 6.4.0.2 |
| Linux (glibc) | ARMv7 hard-float | Compiled and linted; no runtime tests |
| Windows (MSVC) | x64 | Runtime tests: Standard and Advanced 6.3.2 and 6.4.0.2 |
| Windows (MSVC) | x86 | Runtime tests: Advanced 6.4.0.2 |
| macOS | Apple Silicon (ARM64) | Runtime tests: Standard and Advanced 6.3.2 and 6.4.0.2 |
| macOS | Intel (x86-64) | Runtime tests: Standard 6.4.0.2 |
| Android | ARM64, ARMv7, x86, x86-64 | Compiled and linted; no runtime tests |
| iOS | ARM64 devices, x86-64 simulator | Built with NDI SDK for Apple 6.4; no runtime tests |
| tvOS | ARM64 devices, x86-64 simulator | Built with NDI SDK for Apple 6.4; no runtime tests |
| visionOS | ARM64 devices | Built with NDI SDK for Apple 6.4; no runtime tests |

Apple mobile targets link the SDK statically. ARM64 Apple simulators, Mac
Catalyst and Windows ARM64 are not supported; other targets, including musl,
BSD and Windows GNU, have not been compiled or tested. See the
[full platform matrix](docs/setup.md#platforms) for Rust target triples and
Apple toolchain requirements.

Optional APIs depend on the selected runtime's capabilities. Discovery Server
APIs and the File API require NDI 6.4 or later; the File API also needs the
Advanced runtime on Linux, macOS and Android. See
[version compatibility](docs/setup.md#supported-ndi-versions-and-platforms)
and [SDK validation](docs/sdk-validation.md) for details.

## Quick start

```rust
use grafton_ndi::{Finder, FinderOptions, Ndi};
use std::time::Duration;

fn main() -> grafton_ndi::Result<()> {
    let ndi = Ndi::new()?;

    let finder = Finder::new(&ndi, &FinderOptions::new())?;

    for source in finder.find_sources(Duration::from_secs(5))? {
        println!("Found source: {source}");
    }

    Ok(())
}
```

## Features

| Feature | Effect |
| --- | --- |
| `image-encoding` | PNG, JPEG, lossless and lossy WebP with quality control, and data-URL encoding of video frames in every uncompressed format. Off by default. See the [image encoding guide](docs/image-encoding.md). |
| `tokio`, `smol` | Receiver adapters for the named async runtime. Each is independent. |
| `advanced` | The `advanced` module: NDI Advanced SDK operations. See [Advanced SDK](docs/advanced.md). |
| `file-io` | The `file` module: recording and playback through the NDI File API. Independent of the `advanced` Cargo feature; requires the Advanced runtime on Linux, macOS and Android. See the [File API guide](docs/file-io.md). |

No feature is enabled by default; enable each one you need, for example `features = ["image-encoding"]`. Cargo features select Rust APIs only. They do not choose an SDK edition or a runtime library; see [SDK editions](docs/setup.md#sdk-editions).

## Guides and examples

- [Setup](docs/setup.md): runtime selection, compatibility and deployment.
- [Usage](docs/usage.md): discover, receive, send, synchronize and use async adapters.
- [Examples](examples/README.md): runnable programs grouped by task.
- [API reference](https://docs.rs/grafton-ndi): types, methods and their contracts.
- [Troubleshooting](docs/troubleshooting.md): loading errors and SDK limitations.
- [Migration guides](docs/migration/): upgrade existing applications.
- [Changelog](CHANGELOG.md): changes by release.

Run a discovery example from this checkout:

```sh
cargo run --example NDIlib_Find
```

## Contribute

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup and checks, and the
[CI guide](ci/README.md) for workflow and release operations.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.

The bindings in `src/sys/bindings.rs` and `src/sys/declarations.rs` are generated from NDI SDK headers that the vendor distributes under the MIT license. Each file starts with that copyright and permission notice, and the package's license expression is therefore `Apache-2.0 AND MIT`.

The NDI SDK and runtime are supplied separately under their vendor terms.

## Disclaimer

This is an unofficial community project and is not affiliated with NewTek or Vizrt.

NDI® is a registered trademark of Vizrt NDI AB.
