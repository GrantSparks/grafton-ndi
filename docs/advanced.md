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

Set an options value's `config(AdvancedConfig)` before its ordinary constructor
when the SDK object needs JSON configuration. `AdvancedConfig::parse` checks a
JSON object of at most one MiB and preserves its text, including unknown
properties. Property meanings come from the SDK. Debug output and parse errors
hide the content; `expose_json()` deliberately reveals it, so avoid logging
credentials or network details it may contain.

[`advanced_source_metadata`](../examples/advanced_source_metadata.rs) shows
`Finder::sources_with_metadata`. Configuration is also supported by sender,
receiver, router and Discovery Server options. `CompressedSender` and
`PacketReceiver` use the configured entry point even when given no explicit
configuration. Refer to their constructors for defaults.

## Compressed streams

### Receiving packets

Choose `PacketReceiver` when the application needs codec packets. For ordinary
decoded mixed media use the feature-free
[`Receiver::capture_any_ref` workflow](usage.md#receive-whichever-media-arrives-next).

Construct a packet receiver with `ReceiverOptions` and a `PassthroughPreset`.
A supported format named by the preset passes through compressed; other media
can arrive decoded. `capture(timeout)` returns an owned
`PacketItem::{Video, Audio, Metadata, Event}`. Notifications use the common
`ReceiverEvent` model and are consumed once. `Ok(None)` means the wait budget
expired, rather than proof that the source disconnected. Packet receivers
cannot feed `FrameSync`.

Capture takes `&mut self`; shared controls remain available through
`receiver()`. `controls()` reaches exclusive stream selection and KVM, and a
packet KVM session captures through its own `capture()`. Neither route exposes
raw receiver capture. See
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
Received envelopes can preserve unknown header fields that sending refuses;
call `validate_for_send` before forwarding an arbitrary received packet.
Protection flags describe packet conventions and do not encrypt content.
The [packet types](https://grantsparks.github.io/grafton-ndi/grafton_ndi/advanced/struct.CompressedVideoFrame.html)
and [envelope reference](https://grantsparks.github.io/grafton-ndi/grafton_ndi/advanced/struct.PacketEnvelope.html)
specify exact limits and send validation.

Owned compressed/scatter frames edit timecode and metadata with `set_timecode`,
`set_metadata` and `clear_metadata`. Invalid replacement metadata preserves the
old value. Audio uses `set_protected`; reusable `VideoPacketFormat` uses fluent
`with_*` settings. Scatter frames retain separate owned blocks rather than
joining payloads; `into_contiguous_copy` explicitly joins them.

### Sending packets

`CompressedSender` owns its sender. Synchronous video/audio and scatter sends
borrow frames **mutably**, since the SDK can write their bytes during the call.
The frame is reusable afterwards. Asynchronous video sends hold a frame until
the next asynchronous send on the same program/preview stream returns it.
Audio sends are synchronous. `close()` waits for pending SDK use and returns
remaining video as `ReclaimedVideo`; reclamation does not confirm delivery.
Moving owned payloads into this wrapper makes no payload copy in the crate;
the SDK can still copy internally.

The attached sender retains metadata, tally, connection and encoder controls,
but offers no uncompressed sends and cannot be registered with a
`SendAdvertiser`. This keeps the native sender from outliving pending storage.
`CompressedSender::split()` lends video and audio views to separate scoped
threads. Each view is `Send` and neither is `Sync` or `Clone`; the audio view's
`sender()` provides metadata and controls while video is being submitted.

## Queued sending

Use `QueuedSender` to submit multiple owned raw or compressed video frames
without waiting for the previous frame's completion on each submission.
`QueueOptions` bounds retained frames and bytes, including completed frames not
yet collected and spare payload/metadata capacity. Check defaults and exact
accounting in [`QueueOptions`](https://grantsparks.github.io/grafton-ndi/grafton_ndi/advanced/struct.QueueOptions.html).

`try_submit` returns a `SubmissionId` or the rejected frame. For `QueueFull`,
collect completions and retry. A frame whose retained allocation capacity
alone exceeds `max_bytes` returns `InvalidInput`; waiting cannot make it fit.
Inspect other errors before retrying.
`try_take_completed` and `wait_completed` return reusable frames on the
application thread. Submitting moves payload ownership but allocates a native
descriptor. The queue's SDK completion callback runs no application code.

`from_sender` drains an ordinary sender's async pipeline and returns any held
frame; it refuses advertised senders. The queue lends audio, metadata and
shared controls through `sender()`, but reserves video sends. `QueuedSender` is
`Send + Sync`; submission/collection require mutable access. `split()` lends
an exclusive `QueuedVideo` view alongside the shared sender, so audio can run
on its own scoped thread while video waits for completions. Neither split
adds a lock or allocation to a send. **Drop waits
without a timeout** for the SDK to finish with queued frames. Keep this blocking
shutdown off an async executor. See
[`advanced_queued_send`](../examples/advanced_queued_send.rs).

## Custom receive allocation

Use `AllocatorService` when received buffers must come from application memory.
Construction requires `ndi.process_lifetime()`, which permanently retains an
initialized runtime reference. If this crate is loaded in a plug-in, its module
must also remain loaded for the process lifetime because the SDK retains
callback pointers. Applications that do not opt in keep ordinary lifetimes.

Attach an ordinary or packet receiver, then install video/audio allocation
policies. Policies execute on SDK threads, potentially concurrently; return
quickly. An error or contained panic declines the allocation. Replacing a policy
keeps the old one alive until the SDK releases its outstanding buffers.
Process-wide `AllocatorLimits` include those older allocations and are fixed by
the first service; later services must request identical limits. Use `usage()`
to inspect retained allocations and decline counters. `declined_allocations()`
reports the process-wide total; `declined(AllocationDecline::ByteLimit)` and
the other typed reasons distinguish exhausted budgets, invalid requests,
unavailable generations, policy errors/panics and refused buffers. Counters
never reset, include retired policies and saturate at `u64::MAX`. Each declined
request increments one reason. Updates add no allocation, callback or logging
on SDK threads.

`AllocationBuffer::zeroed(request)` provides zeroed, 64-byte-aligned memory.
Use `request.with_stride_bytes(stride)` for permitted line/channel padding.
`AllocationBuffer::from_external` supports a custom pool only under the
[`ExternalAllocation` unsafe contract](https://grantsparks.github.io/grafton-ndi/grafton_ndi/advanced/trait.ExternalAllocation.html):
initialized CPU-accessible memory, stable address/capacity, required alignment
and exclusive SDK access until release. Do not infer these guarantees from a
buffer's size alone.

Recover the receiver with `into_receiver()` or `into_packet_receiver()`;
outstanding SDK allocations still retain their policies. See
[`advanced_allocator`](../examples/advanced_allocator.rs), including the
explicit process-lifetime opt-in and padded-stride policy.

## Synchronization

`AvSync` matches audio to a captured video's timestamp. It owns the receiver
and reserves audio capture; capture video through `sync.receiver()`, then pass
it to `synchronize` as an `AvSyncVideo`. `AvSyncVideo::new` accepts a borrowed
capture or an owned copy of one, and returns `None` for a frame without its
sender's timestamp, such as one the application built. `queued_audio` takes
the queued audio without a frame. `AvSyncRequest::automatic()` lets the SDK
choose audio shape. `AvSyncOutcome` distinguishes audio, no matching
samples, format changes and absent audio; SDK internal failures return
`Error::NativeOperationFailed`, unknown codes `Error::UnexpectedSdkResult`.
These successful absence outcomes do not prove recent reception; use the
[media-progress monitor](usage.md#monitor-reception-of-the-expected-media).

`AvSync` is `Send` but not `Sync`; synchronization runs on one thread at a
time. Its shared receiver permits concurrent compatible capture/control work.
`split()` separates synchronization and exclusive controls for KVM;
`into_receiver()` restores audio capture. See
[`advanced_avsync`](../examples/advanced_avsync.rs).

`Genlock` paces a sender from a reference source. Disable the sender's own
clocking, obtain `clocks()`, then wait on the video/audio clock before sending
that media. Each wait reports reference or fallback system clock pacing.
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
for simultaneous FrameSync/AVSync output and a session; packet sessions provide
`capture()` directly. FrameSync and AvSync views retain metadata capture but
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
