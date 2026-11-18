# Advanced SDK integration

Enable `advanced` for compressed packets, queued sending, custom receive
allocation, AV synchronization, genlock and KVM. Start with
[SDK editions and build requirements](setup.md#sdk-editions); the
[example index](../examples/README.md#advanced-sdk) supplies complete programs.
Exact per-method contracts are in the
[3.0 Advanced API reference](https://grantsparks.github.io/grafton-ndi/grafton_ndi/advanced/).

## Requirements

Native Advanced operations need Advanced SDK headers and `libclang` at build
time, and an Advanced runtime at execution time. Set `NDI_SDK_DIR` to the SDK
root to generate declarations. The Rust API also compiles without those
headers, allowing data types to remain usable while unavailable native
operations return `Error::UnsupportedCapability`.

A Cargo feature does not require a runtime edition or establish license
entitlements. Use `SdkEdition::Advanced` when selecting a runtime to require
that edition. Otherwise `Ndi::new()` can succeed with a Standard runtime and an
Advanced operation can fail later. See [runtime selection](setup.md).

## Check availability

Before an optional workflow, call `ndi.require_feature(feature)` or inspect
`ndi.capabilities().status(feature)`. Failure identifies a feature, reason and
detail: for example `BuildAbiUnavailable`, `WrongEdition` or
`SymbolUnavailable`. Missing header families disable their own operations.
Packet formats, envelopes and options are usable independently of native
support. Consult [`RuntimeFeature`](https://grantsparks.github.io/grafton-ndi/grafton_ndi/capabilities/enum.RuntimeFeature.html)
for the capability to check.

## Ownership

Fallible operations that take a receiver, sender, frame or allocation return
the supplied value inside `Rejected<T>` when they fail. Use `into_inner()` or
`into_parts()` to repair or retry it. Converting `Rejected` to `Error`, including
through `?`, drops that value.

Attached handles reserve the capture or send paths they manage. Their shared
`receiver()`/`sender()` views preserve compatible controls but cannot replace
the native handle or bypass its ownership rules. See the single
[receiver ownership table](usage.md#use-framesync) for available media paths.

## Configuration

For objects with an options value, set `config(AdvancedConfig)` before their
ordinary constructor. Genlock uses `GenlockOptions::config` and
`Genlock::new(&ndi, &options)`. `AdvancedConfig::parse` checks a
JSON object of at most one MiB and preserves its text, including unknown
properties. Property meanings come from the SDK. Debug output and parse errors
hide the content; `expose_json()` deliberately reveals it, so avoid logging
credentials or network details it may contain.

[`advanced_source_metadata`](../examples/advanced_source_metadata.rs) shows
`Finder::current_sources_with_metadata`. Configuration is also supported by sender,
receiver, router, Discovery Server and Genlock options.
`CompressedSender` and
`PacketReceiver` use the configured entry point even when given no explicit
configuration. Refer to their constructors for defaults.

## Compressed streams

### Receiving packets

Choose `PacketReceiver` when the application needs codec packets. For ordinary
decoded mixed media use the feature-free
[`Receiver::capture_any_ref` workflow](usage.md#receive-whichever-media-arrives-next).

Construct a packet receiver with `ReceiverOptions` and a `PassthroughPreset`.
A supported format named by the preset passes through compressed; other media
can arrive decoded. `capture_any_ref(timeout)` returns a borrowed
`PacketItemRef::{Video, Audio, Metadata, Event}`; `capture_any(timeout)` makes the
same capture and copies it into an owned `PacketItem`. Compressed references
expose codec bytes without copying their payload or metadata. Their `to_owned`
and `copy_into` methods make lossless `ReceivedVideoPacket` and
`ReceivedAudioPacket` values; `copy_into` reuses buffers.
Notifications use the common `ReceiverEvent` model and are consumed once.
`Ok(None)` means the wait budget expired, rather than proof that the source
disconnected. Packet receivers cannot feed `FrameSync`.

Both capture methods take `&self`; controls remain available through
`receiver()` while a packet is borrowed. `controls()` reaches exclusive stream
selection and KVM. Packet sessions capture through `session.receiver()` using
`capture_any` or `capture_any_ref`, so capture can run on a worker while the
session sends input. Borrowed results retain the receiver, so inputs can be
released while a packet is held.
Neither route exposes raw receiver capture. See
[`advanced_passthrough`](../examples/advanced_passthrough.rs) for receiving and
relaying a source's compressed packets.

### Packet formats

`CompressedVideoFrame` and `CompressedAudioFrame` own their bytes.
`VideoPacketFormat` describes codec, stream tier, dimensions, timing, picture
aspect and scan type. Its default timing is 60 fps: set the rate to match your
encoded media. `PacketEnvelope::encode` constructs the H.264, HEVC or AAC
envelope; `parse` reads one. Opus and SpeedHQ use no envelope.

Constructors validate layout and wrapper send constraints, **not codec
bitstreams**. Supply valid encoded media and required codec configuration.
Received envelopes can preserve unknown header fields that sending refuses.
Use `CompressedVideoFrame::try_from(packet)` or its audio equivalent to move
received storage into a sendable frame; a refusal returns the original packet.
Borrowed packets also convert through `TryFrom<&CompressedVideoRef>` and its
audio equivalent, checking admission before copying. Their `try_copy_into`
methods reuse an existing sendable frame and leave it unchanged on refusal.
Contiguous compressed frames and packets expose `envelope()` as a validated
view of H.264, HEVC or AAC headers and payloads; SpeedHQ and Opus return `None`.

`validate_for_send` is available on the lossless received packet types for
inspection; constructed compressed frames carry the sendability guarantee.
Protection flags describe packet conventions and do not encrypt content.
The [packet types](https://grantsparks.github.io/grafton-ndi/grafton_ndi/advanced/struct.CompressedVideoFrame.html)
and [envelope reference](https://grantsparks.github.io/grafton-ndi/grafton_ndi/advanced/struct.PacketEnvelope.html)
specify exact limits and send validation.

Owned compressed/scatter frames edit timecode and metadata with `set_timecode`,
`set_metadata` and `clear_metadata`. Invalid replacement metadata preserves the
old value. Reusable `VideoPacketFormat` and `AudioPacketFormat` describe codec
and stream settings; their fluent `with_*` methods include `with_protected`.
Build each format once and pass it with the payload to contiguous or scatter
frame constructors. Scatter frames retain separate owned blocks rather than
joining payloads; `into_contiguous_copy` explicitly joins them.

### Sending packets

`CompressedSender` owns its sender. Synchronous video/audio and scatter sends
borrow frames **mutably**, since the SDK can write their bytes during the call.
Contiguous video and audio sends return `()`; asynchronous contiguous video
returns the displaced `Option<SubmittedVideo>`. Scatter sends retain their
fallible runtime capability checks. The frame is reusable afterwards.
Asynchronous video sends hold a frame until
the next asynchronous send on the same program/preview stream returns it.
Audio sends are synchronous. `reclaim_video()` waits for pending SDK use
and returns both streams' remaining video as `ReclaimedVideo`, keeping the
source available for further sends. `close()` removes the source and also
reclaims remaining video. Reclamation does not confirm delivery.
Moving owned payloads into this wrapper makes no payload copy in the crate;
the SDK can still copy internally.

The attached sender retains metadata, tally, connection and encoder controls,
but offers no uncompressed sends and cannot be registered with a
`SendAdvertiser`. This keeps the native sender from outliving pending storage.
`CompressedSender::split()` returns `CompressedVideo<'_>` and
`&Sender<CompressedAttached>` for separate scoped workers. The video view is
`Send` and not `Sync` or `Clone`; the shared sender supports compressed audio,
metadata and controls while video is submitted. Use
`sender.sender().send_compressed_audio(&mut frame)` for audio without splitting.

## Queued sending

Use `QueuedSender` to submit multiple owned raw, compressed or scatter video
frames without waiting for the previous frame's completion on each submission.
`QueueLimits` bounds retained frames and bytes, including completed frames not
yet collected and spare payload/metadata capacity. Check defaults and exact
accounting in [`QueueLimits`](https://grantsparks.github.io/grafton-ndi/grafton_ndi/advanced/struct.QueueLimits.html).
Pass limits by reference to `QueuedSender::new` or `from_sender`.

With `clock_video` enabled, the default, the SDK paces video inside every
submission call. The first can return immediately; later calls wait for the
next frame time. Set `clock_video(false)` and pace submissions in the
application, or use [Genlock](#synchronization), to avoid that frame-rate wait.
SDK back-pressure can still block a call, so consider submission as well as
teardown when choosing work for an async executor.

`try_submit` returns a `SubmissionId` or the rejected frame. For `QueueFull`,
collect completions and retry. A frame whose retained allocation capacity
alone exceeds `max_bytes` returns `InvalidInput`; waiting cannot make it fit.
Inspect other errors before retrying.
`try_next_completed` and `next_completed` return reusable frames on the
application thread. Pass a `ScatterVideoFrame` directly to `try_submit` when
the encoder supplies separate header, payload and parameter-set blocks; it
returns as `QueuedVideoFrame::Scatter`, and `into_blocks()` recovers the original
allocations. This requires scatter-send support as well as completion support.
The byte budget counts block capacities, the owned block collection, both
scatter arrays, the frame-owned scatter list and metadata. Queue construction
reserves stable native frame descriptors for `max_frames`, plus bookkeeping
scratch for allocation-free index rebuilds.
Accepted submissions, completion callbacks and completion collection allocate
nothing. A `QueueFull` refusal allocates only the returned `Rejected` value.
The SDK completion callback runs no application code.

`from_sender` drains an ordinary sender's async pipeline and returns any held
frame; it refuses advertised senders. The queue lends audio, metadata and
shared controls through `sender()`, but reserves video sends. Use
`send_compressed_audio(&mut frame)` for AAC or Opus, or `send_scatter_audio`
for audio held in blocks; uncompressed `send_audio` remains available. These
same methods are available on the shared sender returned by `split()`, so
queued compressed video can carry compressed audio on a separate scoped
thread. Audio and video formats are selected independently; the native
transport test covers compressed video with AAC and Opus. `QueuedSender` is
`Send + Sync`; submission/collection require mutable access. `split()` lends
an exclusive `QueuedVideo` view alongside the shared sender, so audio can run
on its own scoped thread while video waits for completions. Neither split
adds a lock or allocation to a send. `close()` consumes the queue and returns
every retained frame with its submission id: completed frames awaiting
collection in completion order, followed by unmatched frames in submission
order. Use it to
recover buffers at shutdown or after `completion_protocol_failed()` reports an
unmatched native callback. Native destruction ends the SDK's access before any
frame is returned. **Close and drop wait without a timeout** for native
destruction. Keep this blocking shutdown off an async executor. See
[`advanced_queued_send`](../examples/advanced_queued_send.rs).

## Custom receive allocation

Use `AllocatorService` when received buffers must come from application memory.
Construct it with `AllocatorService::new(ndi.process_lifetime(), &limits)`;
the process-lifetime runtime permanently retains an
initialized runtime reference. If this crate is loaded in a plug-in, its module
must also remain loaded for the process lifetime because the SDK retains
callback pointers. Applications that do not opt in keep ordinary lifetimes.

Attach an ordinary or packet receiver, then install video/audio allocation
policies. Follow the
[`ReceiveAllocator` callback contract](https://grantsparks.github.io/grafton-ndi/grafton_ndi/advanced/trait.ReceiveAllocator.html#callback-contract)
for threading, permitted shared operations and ownership restrictions. Return
quickly. An error or contained panic declines the allocation. Replacing a policy
installs the replacement before retiring the old policy, so allocations begun
by the setter can still use it. Only calls of the old policy still running
when it is retired are declined. Outstanding buffers keep the old policy
alive until the SDK releases them. Its destructor runs in the replacing call
when nothing is outstanding, otherwise in the last releasing call or free
callback, possibly on an SDK thread. It must not wait for that receiver's frames.
Process-wide `AllocatorLimits` include those older allocations and are fixed by
the first service; later services must request identical limits. Use `usage()`
to inspect retained allocations and decline counters. `declined_allocations()`
reports the process-wide total; `declined(AllocationDecline::ByteLimit)` and
the other typed reasons distinguish exhausted budgets, invalid requests,
unavailable generations, policy errors/panics and refused buffers. Counters
never reset, include retired policies and saturate at `u64::MAX`. Each declined
request increments one reason. Updates add no allocation, callback or logging
on SDK threads.

Audio requests above the 64 MiB capture limit are declined as
`AllocationDecline::RequestNotRepresentable` before calling the allocation
policy. Unrepresentable video and audio requests are declined without allocating
on the SDK thread.

`AllocatorService::attach_packets` returns `AllocatedReceiver<PacketAttached>`
with the same application buffers. Use its `capture_any_ref` to inspect those
buffers in place; owned `capture_any` copies them. Keeping a packet reference retains its
allocation and usage charge until the SDK releases that buffer after the
reference is dropped. For example, after constructing `service` and `packets`:

```rust,ignore
let mut receiver = service.attach_packets(packets)?;
receiver.set_video_allocator(|request| AllocationBuffer::zeroed(request))?;
if let Some(PacketItemRef::Video(ReceivedVideoRef::Compressed(packet))) =
    receiver.capture_any_ref(Duration::from_secs(1))?
{
    inspect_codec_bytes(packet.data()); // Still in the policy's allocation.
}
```

See [`advanced_allocator`](../examples/advanced_allocator.rs) for allocator setup
and [`advanced_passthrough`](../examples/advanced_passthrough.rs) for packet
capture and forwarding.

`AllocationBuffer::zeroed(request)` provides zeroed, 64-byte-aligned memory.
Use `request.with_stride_bytes(stride)` for permitted line/channel padding.
`AllocationBuffer::from_external` supports arbitrary application-owned handles
under the [`ExternalAllocation` unsafe contract](https://grantsparks.github.io/grafton-ndi/grafton_ndi/advanced/trait.ExternalAllocation.html).
It boxes the handle once per buffer, on the thread that constructs it.

A preallocated pool can avoid that allocation with
`unsafe { AllocationBuffer::from_pool(request, &pool, block) }`. Its `Arc<P>`
implements [`ExternalPool`](https://grantsparks.github.io/grafton-ndi/grafton_ndi/advanced/trait.ExternalPool.html),
and `PoolBlock::new(pointer, capacity, tag)` describes a pool slot. A successful
construction clones only the Arc and reads the descriptor's plain data; it calls
no application accessors. The pool's `unsafe release(block)` receives that same
descriptor once the accepted buffer is dropped, including a publication refusal.
A constructor refusal instead returns `Rejected<PoolBlock>` without releasing it;
the caller still owns that claim and must return it to its pool.

Both paths require initialized, writable, CPU-accessible memory with a stable
address/capacity, at least 64-byte alignment, and exclusive SDK access until
release. `PoolBlock` is a copyable descriptor, not an ownership token. Copies
must never be lent concurrently or released twice. Its constructor cannot check
those facts, so `from_pool` requires an unsafe per-loan proof that the block is
valid, exclusively claimed, and belongs to that pool. `release` is also unsafe
to prevent safe early release of a copied descriptor. Do not infer memory
validity from address, capacity or alignment checks alone.

Allocator callbacks and release follow the
[`ReceiveAllocator` callback contract](https://grantsparks.github.io/grafton-ndi/grafton_ndi/advanced/trait.ReceiveAllocator.html#callback-contract),
including execution on a thread changing the receiver's connection. Shared
controls and captures are permitted; changing that connection or dropping its
last allocated owner from allocator code can deadlock. Release and the pool's
final destructor run outside the registry lock. Release must not panic. A panic while reclaiming a
published block is contained and leaves that block's count and bytes charged,
since cleanup may be incomplete. A refused buffer has never been charged; its
release panic is contained by the allocation callback.

Recover the receiver with `into_receiver()` or `into_packet_receiver()`;
outstanding SDK allocations still retain their policies. See
[`advanced_allocator`](../examples/advanced_allocator.rs), including the
explicit process-lifetime opt-in and padded-stride policy.

The ignored allocation-cycle harness compares pool and external-handle paths
under same-thread and concurrent callback/free workloads:

```sh
cargo test --release --locked --features advanced --lib \
  custom_allocation_cycle_cost -- --ignored --nocapture
```

Measure representative callback/free concurrency before drawing latency or
throughput conclusions. The [pool-constructor issue](https://github.com/GrantSparks/grafton-ndi/issues/246#issuecomment-6901596756)
retains the historical measurements and their limits.

## Synchronization

`AvSync` matches audio to a captured video's timestamp. It owns the receiver
and reserves audio capture; capture video through `sync.receiver()`, then pass
it to `synchronize` as an `AvSyncVideo`. `AvSyncVideo::new` accepts a borrowed
capture or an owned copy of one, and returns `None` for a frame without its
sender's timestamp, such as one the application built. Matching reads the
video's timing independently of send-format capabilities: owned and borrowed
planar 4:4:4 video can both synchronize on older Advanced runtimes.
`queued_audio` takes
the queued audio without a frame. `AvSyncRequest::automatic()` lets the SDK
choose audio shape. `AvSyncOutcome` distinguishes audio, no matching
samples, format changes and absent audio; SDK internal failures return
`Error::NativeOperationFailed`, unknown codes `Error::UnexpectedSdkResult`.
These successful absence outcomes do not prove recent reception; use the
[media-progress monitor](usage.md#monitor-reception-of-the-expected-media).

`synchronize_ref` and `queued_audio_ref` return `AvSyncOutcomeRef`, whose
matched `AvSyncAudioRef` borrows the native samples and implements `AudioSource`.
Send or convert them in place, call `to_owned()` to retain a copy, or `copy_into`
to reuse an existing `AudioFrame` without allocating when its capacity suffices.
The owned methods delegate to this same capture path. Borrowed audio stays on
its capture thread, may coexist with later results, and keeps the synchronizer
borrowed until released. AV-sync's zero unknown timestamp reads as `None`;
matched sender timestamps read as `Some`.

`AvSync` is `Send` but not `Sync`; synchronization runs on one thread at a
time. Its shared receiver permits concurrent compatible capture/control work;
see the [`Receiver` waiting contract](https://grantsparks.github.io/grafton-ndi/grafton_ndi/receiver/struct.Receiver.html#waiting).
`split()` separates synchronization and exclusive controls for KVM;
`into_receiver()` restores audio capture. See
[`advanced_avsync`](../examples/advanced_avsync.rs).

`Genlock` paces a sender from a reference source. Disable the sender's own
clocking, obtain `clocks()`, then wait on the video/audio clock before sending
that media. Each wait reports reference or fallback system clock pacing.
Create it with `Genlock::new(&ndi, &GenlockOptions::new().source(source))`.
Options expose `config`, `clear_source`, `clear_config` and matching
`configured_*` readback. `connect_to(&source)` and `disconnect()` change its
reference after clock views have been released.
**Waits block without a cancellation or deadline mechanism.** The clocks borrow
the genlock, preventing a reference change while in use. `Genlock` is `Send`
but not `Sync`; the two clocks can wait on separate threads. See
[`advanced_genlock`](../examples/advanced_genlock.rs).

## Encoder guidance, keyframes and stream selection

Advanced methods on ordinary `Sender` and `Receiver` provide encoder size/rate
and quality guidance, keyframe requests and external tally. A receiver's
keyframe request is a request to the source, rather than confirmation that a
keyframe was sent. SpeedHQ-only quality methods reject other codecs.

`set_stream_tier` chooses program/preview; `set_streams` chooses media types.
Their `SettingChange::Unchanged` cannot distinguish a redundant request from a
refused request. Consult SDK licensing for permitted use. Attached receiver
owners expose these controls through `controls()`; see the
[ownership table](usage.md#use-framesync).

## KVM

`Receiver::kvm()` creates an exclusive `KvmSession`; attached owners use
`controls().kvm()`. A source must offer remote control. `session.receiver()`
returns a restricted `KvmReceiver<'_, M>` for mode-permitted capture, event polling,
diagnostics, tally, PTZ, metadata and other shared controls while input stays
held. Borrowed frames can coexist with mutable input calls. Use `split()`
for simultaneous FrameSync/AVSync output and a session; packet sessions capture
through `session.receiver()`. FrameSync and AvSync views retain metadata capture but
have no event polling; use capability getters, source-name polling and connection
queries.

The session sends pointer, button, wheel, key, touch and explicit clipboard
text; it never reads the local clipboard. Pointer/touch coordinates are
fractions in `0.0..=1.0`, and keys use X11 key symbols. A native send failure
returns `Error::NativeOperationFailed`; success does not confirm the source
acted on the message. The view excludes `connect_to`, `disconnect` and
`reconnect`, and cannot recover an unrestricted receiver. To switch sources,
successfully `release_all()`, end the session, change the connection through its
owner, then create a new session. This applies throughout the session, even
when no inputs are held or the view moves to a scoped thread.

Drop attempts release only on a best-effort basis; explicit release reports
failure and retains failed inputs for retry. Native disconnection, Discovery
Server remote control and failed release delivery can still leave remote input
unresolved. The session cannot release input on a source it has lost.
See [`advanced_kvm`](../examples/advanced_kvm.rs).

## Validation

Use [runtime checks](../ci/README.md#test-against-an-installed-runtime) with
your selected SDK and platform. Compiling these APIs does not establish runtime
capabilities, hardware interoperability or license entitlements.
