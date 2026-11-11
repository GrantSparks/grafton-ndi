# Advanced SDK integration

Enable the `advanced` Cargo feature and provide an authorized Advanced SDK through
`NDI_SDK_DIR`. Select the matching runtime with `RuntimeOptions::edition(SdkEdition::Advanced)`;
`RuntimeOptions::library_path` can identify an exact local library. The existing
`advanced_sdk` compatibility flag remains an accepted no-op.

Enabling a Rust feature does not grant an NDI entitlement. The public Rust API stays
available when built with Standard headers, but native Advanced operations return
`UnsupportedCapability` with a `BuildUnavailable` reason. Packet construction and
validation remain usable without Advanced headers. When Advanced headers are
present, operations require the Advanced edition and resolve exports from the one
selected runtime. Missing exports
remain errors, independently of version labels or library filenames.

`AdvancedConfig::parse` accepts an owned JSON object, preserves unknown extension
keys, and redacts its debug representation. The constructors retain the exact
configuration allocation through native destruction. `expose_json` is an explicit
way to read configuration that may contain credentials. Sender, receiver, finder,
router, both advertisers and both listeners have `new_with_config` constructors.
Avoid putting credentials in debug logs, examples, or environment diagnostics.

## Codec packets and sending

Use `PacketReceiver` with an explicit `PassthroughPreset` for compressed reception.
Its owned result distinguishes decoded video/audio, compressed packets, metadata,
and status events. It does not expose a raw receiver that could enter FrameSync.
Use ordinary `Receiver` for decoded media.

`CompressedVideoFrame` and `CompressedAudioFrame` own their data. Video describes its
codec, program/preview tier, dimensions, timing and scan type through
`VideoPacketFormat`. H.264, HEVC and AAC use a checked packet envelope. Opus is a
packet without that prefix; SpeedHQ has its own header. Structural validation does
not decode or prove the validity of a codec bitstream. Applications must provide
correct codec payloads and the configuration data required for keyframes/AAC.
Protected flags describe SDK packet conventions; they do not encrypt payloads.

Received envelope extensions, unknown flags and reserved data are preserved after
bounds validation. Sending accepts understood header versions and flags, zero
reserved fields, and complete payload lengths. This separates future-compatible
inspection from emitting an unknown native representation.

`CompressedSender` owns an exclusive sender. Its asynchronous sends retain program
and preview buffers separately. A subsequent asynchronous send on the same tier
returns that tier's previous ownership. Synchronous sends conservatively retain
previous async storage. `close` destroys the native sender before returning any
remaining buffers. SDK implementations may internally copy compressed data; Rust
ownership alone is not a zero-copy guarantee.

`ScatterVideoFrame` and `ScatterAudioFrame` own their segments and native pointer
arrays. The entire packet header must fit in the first segment. Both synchronous
scatter operations and asynchronous video scatter are available. Use the explicit
`into_contiguous_copy` conversion when submitting scatter data to `QueuedSender`.

`QueuedSender` uses native completion callbacks with bounded frame and byte counts,
including completed frames the application has not collected. `try_submit` returns
ownership on backpressure. `try_take_completed` and `wait_completed` return completed
frames on the application thread; application code never runs in the completion
trampoline. An existing sender can be consumed only after ordinary async work is
drained and no advertiser retains its native resource. The queue exposes no sender
reference that could add another owner. Drop waits for native callback completion;
it does not promise a destruction deadline.

## Custom receive allocation

Custom allocators require explicit permanent runtime initialization:

```rust,no_run
use grafton_ndi::{NDI, Receiver};
use grafton_ndi::advanced::{
    AllocationBuffer, AllocationRequest, AllocatorLimits, AllocatorService,
};

fn customize(ndi: &NDI, receiver: Receiver) -> grafton_ndi::Result<()> {
    let service = AllocatorService::new(
        ndi.process_lifetime(),
        AllocatorLimits::default(),
    )?;
    let mut receiver = service.attach(receiver)?;
    receiver.set_video_allocator(|request: AllocationRequest| {
        AllocationBuffer::zeroed(request)
    })?;
    receiver.reset_video_allocator();
    let receiver = receiver.into_receiver();
    drop(receiver);
    Ok(())
}
```

`NDI::process_lifetime` permanently retains one initialized runtime reference and
its native code. Dropping tokens does not undo this choice. Ordinary applications
that do not request this token keep their normal runtime lifecycle. Registrations,
policies and frame allocations are still reclaimed normally. An embedding host
that loads this Rust code as an unloadable plugin must keep that containing module
loaded for the process lifetime after opting in; ordinary Rust application linkage
already keeps the callback code and registry alive.

Policies run on SDK threads, can run concurrently/reentrantly, and should return
quickly. The wrapper validates allocation sizes and strides, uses initialized
64-byte-aligned owned storage, and contains policy panics. `with_stride` requests
padding within the configured per-allocation limit. `ExternalAllocation` is the
unsafe bridge to a native pool: its implementation guarantees writable initialized
CPU-accessible memory, alignment, exclusivity, stable capacity and a non-panicking
release on an arbitrary SDK thread.

Video/audio allocators can be replaced or reset during use. Replacement allocates a
fresh generation before changing the current registration. Closing generations
reject pending allocation commits; already-published buffers retain their generation
until the matching native free. Opaque callback tokens are never dereferenced or
reused. Native callbacks may finish after replacement. This is why the permanent
runtime opt-in is separate from buffer ownership.

The first `AllocatorService` fixes process-wide limits. Later services must request
the same limits. Limits cover old generations, admitted calls, retained allocations,
and reclamation still in progress. Arbitrary working memory allocated by a user
policy is outside this accounting. `usage` reports current ownership pressure.
Unknown frees never release arbitrary memory. Failed or panicking reclamation keeps
its budget charged conservatively. `attach_packets` provides the same allocator
ownership for packet receivers without exposing decoded-only capture methods.

## Synchronization and controls

`AvSync` consumes a receiver and exposes a restricted video/control view. It cannot
be used alongside raw audio capture or FrameSync on that receiver. Synchronization
returns owned audio or distinct format-change/no-stream/no-match/error outcomes;
all native audio is freed with the matching AVSync function. `into_receiver`
destroys AVSync before returning the receiver.

`Genlock` retains its source/configuration and supplies separate non-cloneable video
and audio clock tokens for scoped workers. Source changes require exclusive access.
Clock waits can block and have no native timeout/cancellation contract. A fallback
result means system-clock pacing, rather than an invented timeout.

Dynamic receive bandwidth accepts only the SDK's documented program/preview choices.
Stream changes preserve the ambiguous native false result as unchanged-or-rejected.
Encoder guidance preserves quality-format negotiation, and keyframe waiting remains
separate from querying current demand. Routing metadata and external tally are
available through their owning objects.

KVM requires an explicit exclusive `KvmSession`. It does not read the local clipboard
or automatically control discovered devices. The session validates coordinates,
tracks pressed keys/buttons/touch, and attempts release on drop. Connection failure
can prevent cleanup delivery; callers can invoke `release_all` explicitly.

## Verification

Local native evidence for this integration includes Linux x86_64 with Advanced
SDK 6.4. Pure/compile coverage also checks the old Standard 6.3.2 header path and
the existing Rust MSRV. Required hosted native suites use Linux x64, Windows x64
and macOS ARM64 representatives; other supported profiles receive ABI/link or
package checks. Consult the exact revision's results in [SDK validation](sdk-validation.md)
and the [CI matrix](../ci/README.md); workflow definitions are not completed runs.
SDKs 6.0/6.1.1 and physical devices do not have current validation results.
Supported profiles require SDKs and toolchains obtainable and usable on GitHub-hosted
CI; a specialized BSP artifact without those prerequisites remains unsupported.

Pure Rust tests cover packet bounds/extensions, independent stream ownership,
concurrent/out-of-order completion, malformed completion callbacks, allocator
retirement/free races, global budgets, panic cleanup and blocked reclamation.
Authorized-header builds compare manually typed native function pointers and packed
packet offsets with locally generated SDK declarations. Compile contracts exclude
raw audio through AVSync and concurrent reconnects while clock tokens are borrowed.

The ignored `advanced_runtime` integration tests exercise actual Advanced SDK
constructors, controls, callback completion and local video/audio capture through
padded custom allocators with replacement/reset. Build without starting native
peers:

```sh
cargo test --features advanced --test advanced_runtime --no-run
```

Run each ignored test in its own process through the platform network confinement
used by [the native CI runner](../ci/native.py); see its [operating guide](../ci/README.md).
A Cargo test command by itself does not establish network isolation.
`GRAFTON_NDI_RUNTIME` is an exact-library-path override used by these tests, not a
general crate loader environment variable; applications use `RuntimeOptions`.
These tests use test-owned loopback peers and do not require physical hardware. Codec
interoperability, entitlements, physical KVM devices and long-duration clock behavior
require their corresponding fixtures; a constructor or symbol check does not prove
those behaviors. Platform evidence remains identified separately as native,
emulated, or build-only.
