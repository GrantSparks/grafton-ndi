# grafton-ndi

[![Crates.io](https://img.shields.io/crates/v/grafton-ndi.svg)](https://crates.io/crates/grafton-ndi/)
[![Documentation](https://img.shields.io/badge/docs-grantsparks.github.io-blue.svg)](https://grantsparks.github.io/grafton-ndi/grafton_ndi/)
[![CI](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml/badge.svg)](https://github.com/GrantSparks/grafton-ndi/actions/workflows/rust.yml)
[![License](https://img.shields.io/crates/l/grafton-ndi.svg)](https://github.com/GrantSparks/grafton-ndi/blob/main/LICENSE)

Rust APIs for discovering NDI sources and sending and receiving video, audio and
metadata over IP. Build monitors, relays and media processing applications with
NDI 6; optional features add image encoding, async adapters, Advanced SDK
operations, and recording and playback.

Capture into borrowed frames for immediate processing, copy into owned frames
for longer retention, or reuse buffers. FrameSync provides clock-driven output;
raw capture exposes incoming frames and their timing. The native SDK performs the
network and codec work. Applications still need to choose their buffering,
threading and backpressure policies.

## Supported SDKs and platforms

- **NDI SDKs:** 6.1.1 Standard; 6.3.2 and 6.4.0.2 Standard and Advanced.
- **Platforms:** Linux (glibc), Windows (MSVC), macOS, Android, iOS, tvOS and visionOS.

Runtime coverage varies by version and architecture. Android and Linux ARMv7
have compile checks; Apple mobile targets have static build checks. See the
[support matrix](https://github.com/GrantSparks/grafton-ndi/blob/main/docs/setup.md#supported-ndi-versions-and-platforms)
for supported architectures and requirements.

## Install

```toml
[dependencies]
grafton-ndi = "3.0"
```

Use Rust 1.88 or later and an NDI 6 runtime. Default Windows, macOS, Linux and
Android builds need no SDK at build time. Advanced native operations need
Advanced SDK headers and `libclang`; Apple mobile targets link a static SDK
archive and some need a later Rust toolchain. See
[setup and platform support](https://github.com/GrantSparks/grafton-ndi/blob/main/docs/setup.md).

## Discover, receive and relay

With an NDI source running on your network, this program discovers a source,
receives its video, audio and metadata, and forwards them as a new NDI source
for ten seconds. It uses the default features:

```rust
use grafton_ndi::{
    Finder, FinderOptions, Ndi, Receiver, ReceiverItemRef, ReceiverOptions,
    Sender, SenderOptions,
};
use std::time::{Duration, Instant};

fn main() -> grafton_ndi::Result<()> {
    let ndi = Ndi::new()?;
    let finder = Finder::new(&ndi, &FinderOptions::new())?;
    let sources = finder.find_sources(Duration::from_secs(5))?;
    let Some(source) = sources.into_iter().next() else {
        eprintln!("No NDI sources found.");
        return Ok(());
    };
    println!("Receiving {}", source.name());

    let receiver = Receiver::new(&ndi, &ReceiverOptions::new(source))?;
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new("Rust relay").clock_video(false).clock_audio(false),
    )?;

    let stop = Instant::now() + Duration::from_secs(10);
    while Instant::now() < stop {
        match receiver.capture_any_ref(Duration::from_millis(100))? {
            Some(ReceiverItemRef::Video(frame)) => {
                println!("{}x{} {:?}", frame.width(), frame.height(), frame.pixel_format());
                sender.send_video(&frame)?;
            }
            Some(ReceiverItemRef::Audio(frame)) => sender.send_audio(&frame),
            Some(ReceiverItemRef::Metadata(frame)) => sender.send_metadata(&frame),
            Some(ReceiverItemRef::Event(event)) => eprintln!("{event:?}"),
            _ => {} // No item arrived within this capture's timeout.
        }
    }
    Ok(())
}
```

`Ndi` keeps the selected runtime alive. The finder returns owned source
descriptors; the receiver lends captured frames until they leave scope, when
their native buffers are released. Synchronous sends borrow those same frames.
For processing that must retain media, make an owned copy or reuse storage
with `copy_into`. The incoming stream paces this relay, so sender clocks are
disabled. Native video staging can still be needed for SDK padding requirements.

Set `NDI_RUNTIME_DIR_V6` if the runtime is outside its default locations; see
[setup](https://github.com/GrantSparks/grafton-ndi/blob/main/docs/setup.md).
For a runnable relay with source selection and Ctrl-C handling, see
[`mixed_media_relay`](https://github.com/GrantSparks/grafton-ndi/blob/main/examples/mixed_media_relay.rs).
The [examples index](https://github.com/GrantSparks/grafton-ndi/blob/main/examples/README.md)
also covers owned buffers, async capture, FrameSync, snapshots and recording.

## Features

No Cargo feature is enabled by default. Enable the APIs your application uses:

| Feature | Adds |
| --- | --- |
| `image-encoding` | PNG, JPEG, lossless/lossy WebP and data URLs from uncompressed video |
| `tokio`, `smol` | Receiver adapters for the chosen async runtime; independent features |
| `advanced` | Advanced SDK operations; availability also depends on build headers and runtime |
| `file-io` | Recording and playback through the separate File library |

Cargo features select Rust APIs. They do not select a native library or establish
license entitlement. Check
[runtime capabilities](https://github.com/GrantSparks/grafton-ndi/blob/main/docs/setup.md#runtime-capabilities)
before depending on optional native operations.

## Guides and examples

The authoritative API reference is built in CI and published to GitHub Pages
from `main`. It shows the crate version it documents; for an older release,
check out its tag and run `cargo doc --locked --no-deps --all-features`.
The guides and examples below also track `main`. docs.rs is supplementary and
publication does not depend on its build service.

- [Setup](https://github.com/GrantSparks/grafton-ndi/blob/main/docs/setup.md): build, runtime selection, platforms and deployment.
- [Usage](https://github.com/GrantSparks/grafton-ndi/blob/main/docs/usage.md): discovery, capture, sending, synchronization and async workflows.
- [API reference](https://grantsparks.github.io/grafton-ndi/grafton_ndi/): ownership, errors, units and method contracts.
- [Troubleshooting](https://github.com/GrantSparks/grafton-ndi/blob/main/docs/troubleshooting.md): runtime errors and operational precautions.
- [Migration](https://github.com/GrantSparks/grafton-ndi/blob/main/docs/migration/README.md) and [changelog](https://github.com/GrantSparks/grafton-ndi/blob/main/CHANGELOG.md).
- [Contributing](https://github.com/GrantSparks/grafton-ndi/blob/main/CONTRIBUTING.md): development setup and checks.

## License

The package license expression is `Apache-2.0 AND MIT`: wrapper code uses
[Apache-2.0](https://github.com/GrantSparks/grafton-ndi/blob/main/LICENSE), and generated SDK declarations carry their included MIT notice.
Obtain the NDI SDK and runtime separately under their vendor terms.

This is an unofficial community project, unaffiliated with NewTek or Vizrt.
