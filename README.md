# grafton-ndi

[![Crates.io](https://img.shields.io/crates/v/grafton-ndi.svg)](https://crates.io/crates/grafton-ndi)
[![Documentation](https://docs.rs/grafton-ndi/badge.svg)](https://docs.rs/grafton-ndi/3.0.0/grafton_ndi/)
[![CI](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml/badge.svg)](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml)
[![License](https://img.shields.io/crates/l/grafton-ndi.svg)](https://github.com/GrantSparks/grafton-ndi/blob/v3.0.0/LICENSE)

Rust APIs for discovering NDI sources and sending and receiving video, audio and
metadata over IP. Build monitors, relays and media processing applications with
NDI 6; optional features add image encoding, async adapters, Advanced SDK
operations, and recording and playback.

Capture into borrowed frames for immediate processing, copy into owned frames
for longer retention, or reuse buffers. FrameSync provides clock-driven output;
raw capture exposes incoming frames and their timing. The native SDK performs the
network and codec work. Applications still need to choose their buffering,
threading and backpressure policies.

## Install

```toml
[dependencies]
grafton-ndi = "3.0"
```

Use Rust 1.88 or later and an NDI 6 runtime. Default Windows, macOS, Linux and
Android builds need no SDK at build time. Advanced native operations need
Advanced SDK headers and `libclang`; Apple mobile targets link a static SDK
archive and some need a later Rust toolchain. See
[setup and platform support](https://github.com/GrantSparks/grafton-ndi/blob/v3.0.0/docs/setup.md).

## Try receiving a picture

From this checkout, with an NDI video source running on a reachable network:

```sh
export NDI_RUNTIME_DIR_V6="/path/to/ndi/runtime/directory"
cargo run --features image-encoding --example NDIlib_Recv_PNG -- --output capture.png
```

On PowerShell, set the directory with `$env:NDI_RUNTIME_DIR_V6 = "C:\path\to\runtime"`.
The runtime can also sit beside the executable. The example waits for a source,
receives video from the first one, prints its format and writes `capture.png`.
After discovery, capture fails if no video arrives within 60 seconds. See the
[example source](https://github.com/GrantSparks/grafton-ndi/blob/v3.0.0/examples/NDIlib_Recv_PNG.rs)
for extra-IP arguments and the
[example index](https://github.com/GrantSparks/grafton-ndi/blob/v3.0.0/examples/README.md)
for sending, relaying, playback and other tasks.

## Features

No Cargo feature is enabled by default. Enable the APIs your application uses:

| Feature | Adds |
| --- | --- |
| `image-encoding` | PNG, JPEG, lossless/lossy WebP and data URLs from uncompressed video |
| `tokio`, `smol` | Receiver adapters for the chosen async runtime; independent features |
| `advanced` | Advanced SDK operations; availability also depends on build headers and runtime |
| `file-io` | Recording and playback through the separate File library; independent of `advanced` |

Cargo features select Rust APIs. They do not select a native library or establish
license entitlement. Check
[runtime capabilities](https://github.com/GrantSparks/grafton-ndi/blob/v3.0.0/docs/setup.md#runtime-capabilities)
before depending on optional native operations.

## Guides and examples

- [Setup](https://github.com/GrantSparks/grafton-ndi/blob/v3.0.0/docs/setup.md): build, runtime selection, platforms and deployment.
- [Usage](https://github.com/GrantSparks/grafton-ndi/blob/v3.0.0/docs/usage.md): discovery, capture, sending, synchronization and async workflows.
- [API reference](https://docs.rs/grafton-ndi/3.0.0/grafton_ndi/): ownership, errors, units and method contracts.
- [Troubleshooting](https://github.com/GrantSparks/grafton-ndi/blob/v3.0.0/docs/troubleshooting.md): runtime errors and operational precautions.
- [Migration](https://github.com/GrantSparks/grafton-ndi/blob/v3.0.0/docs/migration/README.md) and [changelog](https://github.com/GrantSparks/grafton-ndi/blob/v3.0.0/CHANGELOG.md).
- [Contributing](https://github.com/GrantSparks/grafton-ndi/blob/v3.0.0/CONTRIBUTING.md): development setup and checks.

## License

The package license expression is `Apache-2.0 AND MIT`: wrapper code uses
[Apache-2.0](https://github.com/GrantSparks/grafton-ndi/blob/v3.0.0/LICENSE), and generated SDK declarations carry their included MIT notice.
Obtain the NDI SDK and runtime separately under their vendor terms.

This is an unofficial community project, unaffiliated with NewTek or Vizrt.
