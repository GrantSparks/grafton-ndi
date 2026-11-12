# Advanced SDK integration

The `advanced` Cargo feature adds the `grafton_ndi::advanced` module: the parts
of NDI that only the NDI Advanced SDK provides. This guide explains what the
module needs, how it reports that something is missing, and the ownership rules
its types follow. The rustdoc of each type is the reference. Where an example
program shows a type in use, its section names the file.

## Requirements

Using the module needs three things, in this order:

1. The `advanced` Cargo feature.

   ```toml
   [dependencies]
   grafton-ndi = { version = "3.0", features = ["advanced"] }
   ```

2. An NDI Advanced SDK on the machine that builds the crate. The Advanced SDK
   headers may not be redistributed, so the crate does not contain them. Their
   declarations, types and constants are generated with bindgen at build time
   from the SDK named by `NDI_SDK_DIR`, or from a default installation
   directory. The crate itself carries only what it needs to call the SDK: the
   name and signature of each Advanced function it uses, which the build checks
   against the generated declarations, and the byte layout of the compressed
   packet header it reads and writes. The generation step needs a `libclang`
   shared library; a build that finds an Advanced SDK and no `libclang` fails.

   Without `NDI_SDK_DIR`, the build looks only where the Advanced SDK
   installer puts the SDK: `/usr/share/NDI Advanced SDK for Linux`,
   `/Library/NDI Advanced SDK for Apple` or
   `C:\Program Files\NDI\NDI 6 Advanced SDK`. An SDK whose `include`
   directory has no `Processing.NDI.Advanced.h` is a Standard SDK, and a build
   with it has no Advanced declarations.

3. An Advanced SDK runtime when the program runs. Without an edition request
   the search accepts either edition and, in a build with this feature, tries
   the Advanced library first at each location. If it finds only a Standard
   runtime, `NDI::new()` succeeds and the Advanced operations report
   `Error::UnsupportedCapability`. Request `SdkEdition::Advanced` to require
   the Advanced runtime when it is selected. See
   [SDK editions](../README.md#sdk-editions) in the README.

Enabling the feature neither selects an SDK edition nor grants a license
entitlement. The Advanced SDK 6.3.2 and 6.4.0.2 have been tested; see
[SDK validation](sdk-validation.md).

## When a requirement is missing

The public API of the module is the same in every build. What differs is what
an operation returns:

| Situation | Result of a native operation |
| --- | --- |
| Built without Advanced SDK headers, or with headers that do not declare the operation's functions | `Error::UnsupportedCapability` with `reason` `BuildAbiUnavailable`; `detail` names the first function the build does not declare. Rebuild with `NDI_SDK_DIR` naming an Advanced SDK whose headers declare it |
| Built with headers that do not declare the requested `PassthroughPreset` | `Error::UnsupportedCapability` for the compressed receive preset, with `reason` `BuildAbiUnavailable`; `detail` names the receiver color format the headers lack |
| Standard runtime selected | `Error::UnsupportedCapability` with `reason` `WrongEdition`; `detail` names the operation's function when the runtime does not export it |
| Advanced runtime that does not export a function the operation needs | `Error::UnsupportedCapability` with `reason` `SymbolUnavailable`; `detail` names the function |
| Every requirement met | The operation runs |

`reason` is a `CapabilityUnavailable`, so a program can branch on it without
reading the message.

Older or partial Advanced SDK headers disable only the operations whose
functions they do not declare; the rest of the module keeps working. Core
operations are unaffected in every case. The module's data types work in every
build as well: packet formats, compressed frames, packet envelopes, scatter
frames, options and limits can be constructed, validated and inspected without
the Advanced SDK.

The Discovery Server advertisers and listeners also need an NDI 6.4.0 or later
runtime, with or without a configuration, so an Advanced 6.3.2 runtime refuses
to create them.

To find out in advance, ask the runtime. Each capability is checked
separately, so a successful check covers only the operations it names.
Creating a sender or a receiver with a configuration is gated by
`RuntimeFeature::AdvancedSenderConfiguration` and
`RuntimeFeature::AdvancedReceiverConfiguration`: those constructors are refused
exactly when `require_feature` refuses the feature, with the same error. A
runtime that exports only one of the two creation functions is not an Advanced
SDK runtime, so both are refused:

```rust
use grafton_ndi::{capabilities::RuntimeFeature, NDI};

fn main() -> grafton_ndi::Result<()> {
    let ndi = NDI::new()?;
    match ndi.require_feature(RuntimeFeature::AdvancedSenderConfiguration) {
        Ok(()) => println!("Advanced sender configuration can be used"),
        Err(error) => println!("{error}"),
    }
    Ok(())
}
```

Types built around one group of functions report a missing requirement when
they are created: `AvSync`, `Genlock`, `QueuedSender`, `PacketReceiver`,
`KvmSession` (through `Receiver::kvm`) and the allocator attachments
(`AllocatorService::attach` and `attach_packets`). `CompressedSender` checks
sender configuration when it is created, and its scatter sends, encoder
guidance and keyframe methods check their own functions when they are called.
The Advanced methods on the ordinary `Sender`, `Receiver` and `Router` report a
missing requirement when they are called.

## Ownership

An operation that takes ownership of a receiver, a sender or a frame hands the
value back when it fails. The error type is `grafton_ndi::Rejected<T>`:

```rust
use grafton_ndi::advanced::{CompressedVideoFrame, VideoCodec, VideoPacketFormat};

fn main() -> grafton_ndi::Result<()> {
    let format = VideoPacketFormat::new(VideoCodec::H264, 1920, 1080)?;
    // Not an H.264 packet envelope, so the bytes come back with the reason.
    let rejected = CompressedVideoFrame::new(format, vec![0; 16]).unwrap_err();
    println!("not sendable: {}", rejected.error());
    let bytes: Vec<u8> = rejected.into_inner();
    assert_eq!(bytes.len(), 16);
    Ok(())
}
```

`Rejected<T>` converts into `Error`, dropping the value, so `?` works in a
function that returns `grafton_ndi::Result`. Use `into_inner` or `into_parts`
when the value matters: a receiver to keep using, a frame to retry.

## Configuration

`AdvancedConfig` holds the JSON document the Advanced SDK accepts when it
creates an object. `AdvancedConfig::parse` checks that the text is a JSON object
of at most one mebibyte and keeps it byte for byte, unknown properties
included. The properties themselves are defined by the NDI Advanced SDK
documentation.

A configuration can carry credentials or network details. Its `Debug` output
shows no content, and no error quotes it. `expose_json` returns the text and is
named as a reminder not to log it.

Constructors that take a configuration are named `new_with_config`. `Sender`,
`Receiver`, `Finder`, `Router`, both advertisers, both listeners,
`CompressedSender` and `QueuedSender` have one; `QueuedSender::new_with_config`
also takes its `QueueOptions`. `PacketReceiver::new` takes a passthrough preset
and a configuration, and `Genlock::new` an optional reference source and an
optional configuration. Each object keeps its configuration for as long as it
lives.

`Router::add_connection_metadata` and `clear_connection_metadata` are Advanced
SDK operations too: a router sends the added metadata to every connected
receiver, and to each receiver that connects later, until it is cleared.

`Finder::sources_with_metadata` returns each source together with the metadata
it advertises. A sender advertises metadata through its configuration, and a
finder learns it only from a Discovery Server. The metadata of a source that
advertises none is `None`.

Example: `examples/advanced_source_metadata.rs`.

## Compressed streams

### Receiving packets

`PacketReceiver` delivers a frame as the codec packet the source sent when the
frame's format is one that its `PassthroughPreset` names, and as a decoded frame
otherwise. One `capture` call returns whichever frame or event arrives first as
a `PacketEvent`, and every result owns its data. A `PacketReceiver` is used from
the thread that owns it and cannot feed a `FrameSync`. Use an ordinary
`Receiver` for decoded media only.

### Packet formats

`CompressedVideoFrame` and `CompressedAudioFrame` own their bytes. A video
frame's `VideoPacketFormat` names its codec, its stream (program or preview),
dimensions, frame rate, aspect ratio and scan type. An aspect ratio of zero
means square pixels and is the default.

H.264, HEVC and AAC payloads travel inside a packet envelope: a 44-byte header
followed by the payload and the codec configuration. `packet_envelope` builds
one and `PacketEnvelope::parse` reads one. Opus and SpeedHQ frames have no
envelope.

Construction validates the memory layout the SDK relies on: sizes and, where
there is one, the envelope. It does not decode or validate the codec bitstream;
supplying a correct bitstream, and the codec configuration that keyframes and
AAC packets need, is the application's job. The `protected` setting describes
an SDK packet convention. It does not encrypt anything.

The SDK accepts any compressed frame and silently discards those it will not
deliver. Construction therefore also refuses frames that would be discarded:

- a SpeedHQ frame whose first byte, its quality, is 100 or more;
- an H.264 or HEVC picture on the preview stream that is wider or taller than
  640 pixels;
- Opus at a sample rate other than 8, 12, 16, 24 or 48 kHz, or with a frame
  length other than 2.5, 5, 10, 20, 40 or 60 ms;
- an AAC frame that does not hold 1024 samples per channel, lacks its
  AudioSpecificConfig, or whose configuration names another sample rate or
  channel layout than the frame's own. AAC is sent at one of the thirteen
  indexed sample rates from 7350 Hz to 96 kHz, with one to six or eight
  channels.

These rules were observed with the 6.4.0.2 runtime on Linux. A compressed
frame holds at most 256 MiB (`MAX_PACKET_BYTES`).

Parsing a received envelope keeps what it does not understand: later header
versions, unknown flags and the reserved field stay readable. Sending is
stricter. A frame is sendable only with a version-0 header, no unknown flags
and no bytes after the lengths the header declares; `validate_for_send` checks
this.

### Sending packets

`CompressedSender` owns its sender exclusively. Its program and preview streams
are independent. The synchronous sends (`send_video`, `send_audio`,
`send_scatter_video`, `send_scatter_audio`) borrow the frame mutably, because
the SDK writes to the frame's bytes while it sends them. A frame can be sent
again afterwards. A received packet can be relayed when `validate_for_send`
accepts it; a frame the sender refuses stays with the caller. Video can also be
sent asynchronously (`send_video_async`, `send_scatter_video_async`): the
sender keeps the frame until the next asynchronous send on the same stream,
which returns the earlier frame for reuse. Audio is always sent synchronously.
`close` destroys the native sender, which waits for the SDK to finish with
pending frames, and returns the frames that were still held as `UnsentVideo`,
one field per stream.

`ScatterVideoFrame` and `ScatterAudioFrame` hold one frame in up to 65,536
owned, non-empty blocks, which are sent without being joined. The first block
must hold the frame's complete header: the 44-byte envelope header for H.264,
HEVC and AAC, the first four bytes for SpeedHQ. `into_contiguous_copy` joins the
blocks into an ordinary compressed frame.

Moving a frame into a sender does not copy it in this crate. The SDK may still
copy data internally.

Example: `examples/advanced_passthrough.rs` relays a source's packets
unchanged.

## Queued sending

An ordinary asynchronous video send has to wait for the previous frame before
it accepts the next. `QueuedSender` keeps every submitted frame until the SDK
reports it sent, so submission does not wait for a slow connection.

- It takes uncompressed `VideoFrame`s and `CompressedVideoFrame`s, both as a
  `QueuedVideoFrame`.
- `QueueOptions` bounds the queue by frame count and by bytes. A frame counts
  from submission until the application takes it back. The defaults are 8
  frames and 512 MiB.
- `try_submit` returns a `SubmissionId`, or returns the frame in a `Rejected`
  when a limit is reached or the frame cannot be sent. Collect completed frames
  and submit again.
- `try_take_completed` and `wait_completed` return completed frames on the
  application's thread. No application code runs on an SDK thread.
- `from_sender` converts an existing `Sender`. It returns the frame the sender
  was still holding, if any. It refuses, and returns the sender, when the
  sender is registered with an advertiser.
- `source` returns the source the sender advertises, as it does on
  `CompressedSender`.
- A `QueuedSender` is neither `Send` nor `Sync`: it stays on the thread that
  created it. Dropping it destroys the native sender, which waits without a
  timeout for the SDK to finish with the frames still queued.

Example: `examples/advanced_queued_send.rs`.

## Custom receive allocation

By default the SDK allocates the memory of received frames. An
`AllocatorService` lets the application supply that memory instead.

The SDK can call an allocator's free function long after the allocator was
replaced. Custom allocation therefore needs the runtime to stay initialized
until the process exits, and creating the service requires the explicit opt-in
for that:

```rust
use grafton_ndi::advanced::{
    AllocationBuffer, AllocationRequest, AllocatorLimits, AllocatorService,
};
use grafton_ndi::{Receiver, NDI};

fn customize(ndi: &NDI, receiver: Receiver) -> grafton_ndi::Result<Receiver> {
    let service = AllocatorService::new(ndi.process_lifetime(), AllocatorLimits::default())?;
    let mut receiver = service.attach(receiver)?;
    // Pad every video line to a multiple of 256 bytes.
    receiver.set_video_allocator(|request: AllocationRequest| {
        let stride = (request.stride() + 255) / 256 * 256;
        AllocationBuffer::zeroed(request.with_stride(stride)?)
    })?;
    // ... capture through `receiver.receiver()` ...
    receiver.reset_video_allocator();
    Ok(receiver.into_receiver())
}
```

`NDI::process_lifetime` keeps one initialized runtime reference permanently.
Dropping the returned value does not undo it. Applications that never call it
keep the normal lifecycle. A program that loads this crate inside a plug-in
must also keep that plug-in loaded for the life of the process, because the SDK
holds pointers to the crate's callbacks.

What to know about allocator policies:

- A policy runs on SDK threads, possibly several at once. It should return
  quickly. A panic in a policy is contained; like an error it fails that
  allocation, and the SDK then drops the frame.
- `AllocationBuffer::zeroed` supplies zeroed, 64-byte-aligned memory.
  `AllocationRequest::with_stride` asks for padded lines. `ExternalAllocation`
  is the `unsafe` trait for memory from another pool; its implementer
  guarantees the properties the trait documents.
- Setting an allocator replaces the previous one for frames allocated from then
  on. Frames allocated earlier stay in the memory the earlier policy supplied,
  and that policy is kept until the SDK has freed the last of them.
- `AllocatorLimits` are process-wide and fixed by the first service; later
  services must name the same limits. They count allocations made by policies
  that have since been replaced. The defaults allow 16 concurrent policy calls
  and 64 MiB per allocation, within 1 GiB in total. Limits that could not all
  be honored at once are rejected. A request the limits do not admit is
  declined, and the SDK then drops that frame. `AllocatorService::usage`
  reports what is currently held.
- `set_video_allocator`, `set_audio_allocator` and
  `AllocationBuffer::from_external` return what they were given inside
  `Rejected` when they fail.
- `attach_packets` does for a `PacketReceiver` what `attach` does for a
  `Receiver`, and hands the `PacketReceiver` back inside `Rejected` when it
  fails.

Example: `examples/advanced_allocator.rs`.

## Synchronization

`AvSync` returns the audio that belongs to each video frame's timestamp. It
takes over the receiver's audio, so it owns the receiver: capture video through
`AvSync::receiver`, then call `synchronize`. `AvSync::new` takes the receiver
and hands it back inside `Rejected` when it fails. `AvSyncRequest::automatic()`
takes whatever audio belongs to the frame. `AvSyncRequest::new(Some(samples),
Some(sample_rate))` asks for that many samples per channel, computed for that
sample rate; each `None` leaves the choice to the SDK. The SDK may return a
different number of samples to follow the audio that belongs to the frame, and
then reports the count as adjusted. The outcome is the audio, or says why there
is none: no samples for this frame, a changed audio format, no audio stream, no
samples matching the frame's time, or an SDK error. Nothing is retried or
converted. `into_receiver` stops synchronizing and returns the receiver.

`Genlock` paces a sender from the timing of a reference source. `clocks`
returns a video clock and an audio clock; wait on the matching one before
submitting each frame, and create the sender with its own clocking disabled.
Each wait reports whether the reference or the system clock paced it; the
system clock is used while no reference signal arrives. The waits block their
thread and cannot be cancelled or given a deadline. Both clocks borrow the
genlock, so the reference cannot be changed while a clock is in use.

Examples: `examples/advanced_avsync.rs`, `examples/advanced_genlock.rs`.

## Encoder guidance, keyframes and stream selection

These are methods on the ordinary `Sender` and `Receiver`:

- `Sender::target_frame_bytes`, `target_bit_rate`, `legacy_quality` and
  `quality_plan` report what an encoder should aim for when it produces a given
  `VideoPacketFormat` for the sender's connections. `legacy_quality` and
  `quality_plan` apply to SpeedHQ formats only.
- `Sender::keyframe_required` reports whether a connected receiver needs a
  keyframe, and `wait_for_keyframe_change` waits for that to change.
  `Receiver::request_keyframe` asks the source for one; the source decides
  whether to send it.
- `Sender::set_external_tally` sets the sender's own contribution to its tally.
- `Receiver::set_video_quality` switches between the source's program and
  preview streams. `Receiver::set_streams` chooses which streams the source
  sends. The SDK reports only whether the selection changed, so the
  `StreamChange` that `set_streams` returns cannot tell "already in effect"
  from "refused". The SDK applies either change only under a vendor ID that
  licenses the function, and reports no change without one.

`CompressedSender` has the same encoder guidance, keyframe and tally methods
as `Sender`.

## KVM

`Receiver::kvm` starts a `KvmSession`, exclusive remote control of the source
the receiver is connected to. The receiver is borrowed mutably for the session.
The session sends pointer, button, wheel, key, touch and clipboard messages;
each method returns whether the SDK accepted the message, which is not a
confirmation that the source acted on it. `is_supported` reports whether the
connected source accepts remote control. Pointer and touch positions are fractions of the
source's screen, `0.0..=1.0` on each axis, and keys are X11 key symbols. The
session never reads the local clipboard. It remembers what it has pressed and
touched and releases it when dropped, on a best-effort basis; call
`release_all` to learn whether that succeeded.

Example: `examples/advanced_kvm.rs`.

## Running the tests

The Advanced tests compile in every build with the `advanced` feature. Those
that need an Advanced runtime start with the `advanced_ndi()` test helper and
are skipped, with a message, when the build lacks the declarations for sender
configuration or the selected runtime is a Standard one.

To run them, build with the Advanced SDK and select its runtime. On Linux
x86_64:

```sh
export NDI_SDK_DIR="/path/to/NDI Advanced SDK for Linux"
export NDI_RUNTIME_DIR_V6="$NDI_SDK_DIR/lib/x86_64-linux-gnu"
GRAFTON_NDI_REQUIRE_RUNTIME=advanced cargo test --locked --config ci/offline.toml --features advanced
```

`GRAFTON_NDI_REQUIRE_RUNTIME=advanced` makes a test that would have skipped
fail instead, so the run cannot pass without exercising the Advanced runtime.
The tests create their own senders and receivers and need no hardware. Those
announce themselves on the network like any NDI program; `--config
ci/offline.toml` confines them to loopback networking and needs `bubblewrap`.

What the tests establish is limited to what they run. The Advanced runtime
tests were run on Linux x86_64 with the Advanced SDK 6.3.2 and 6.4.0.2, each
with its own headers and with the two crossed; see
[SDK validation](sdk-validation.md). They do not
cover KVM against a real source, allocator layouts for 4:2:0 and planar 4:4:4
video, real codec bitstreams, license entitlements, long-running clock
behavior, or any other platform.
