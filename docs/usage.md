# Use grafton-ndi

Start with the [README quick start](../README.md#quick-start) and [runtime setup](setup.md).
The snippets below use an initialized `ndi`; capture examples also need a discovered
`camera` source. See the [runnable examples](../examples/README.md) for complete programs.

## Discover sources

`Finder` wraps the NDI discovery API. It can return a current source snapshot, wait for source-list changes, or perform a bounded discovery pass.

```rust
use grafton_ndi::{Finder, FinderOptions};
use std::time::Duration;

let options = FinderOptions::new()
    .groups("Public,Studio")
    .extra_ips("192.168.1.100");
let finder = Finder::new(&ndi, &options)?;

if finder.wait_for_sources(Duration::from_secs(2))? {
    for source in finder.current_sources()? {
        println!("{source}");
    }
}
```

Select a specific feed on the configured finder. Selection checks the current
snapshot immediately, then waits within one budget until a match appears:

```rust
let camera = finder
    .find_source_by_name("STUDIO (Camera 2)", Duration::from_secs(5))?
    .ok_or_else(|| grafton_ndi::Error::NoSourcesFound {
        criteria: "STUDIO (Camera 2)".into(),
    })?;
```

Exact names are case-sensitive. Duplicate names at different addresses select the
first match in the snapshot; disambiguate with `find_source(timeout, |source|
...)`, inspecting both name and address. Predicates run outside the SDK snapshot
lock. A zero timeout checks once; an oversized timeout is refused even when an
immediate match exists. Neither selection nor enumeration guarantees continued
availability.

A host can publish several feeds. `SourceCache::new(&finder)` binds a cache to
that finder's groups, IP hints and configuration. `sources_by_host` observes the
full window and caches every distinct matching descriptor, leaving feed selection
explicit:

```rust
use grafton_ndi::SourceCache;
let cache = SourceCache::new(&finder);
let feeds = cache.sources_by_host("192.168.1.100", Duration::from_secs(5))?;
for feed in feeds { println!("{feed}"); }
```

Entries remain stale until `invalidate(host)` or `clear()` removes them, and an
empty result is not cached. Configure IP hints on the finder itself. Lookup never
creates another finder or changes scope. Cache hits return owned descriptor
copies; misses use the same bounded allocations as full-window enumeration.

## Receive streams

`Receiver` captures video, audio, and metadata from a selected `Source`. Use bandwidth and color-format options to match the role of the receiver, from low-bandwidth monitors to full-quality capture.

```rust
use grafton_ndi::{
    Receiver, ReceiverBandwidth, ReceiverColorFormat, ReceiverOptions,
};
use std::time::Duration;

let options = ReceiverOptions::new(camera.clone())
    .color_format(ReceiverColorFormat::RgbxRgba)
    .bandwidth(ReceiverBandwidth::Highest);
let receiver = Receiver::new(&ndi, &options)?;

// Each capture waits up to its timeout, and returns `None` if nothing came.
if let Some(video) = receiver.video().capture(Duration::from_secs(5))? {
    println!(
        "{}x{} {:?}",
        video.width(),
        video.height(),
        video.pixel_format()
    );
}

let audio = receiver.audio().capture(Duration::from_millis(100))?;
let metadata = receiver.metadata().capture(Duration::from_millis(100))?;
```

Options can be inspected before constructing an SDK object, including in
tests with no runtime installed:

```rust
use grafton_ndi::{ReceiverBandwidth, ReceiverOptions, Source};
let options = ReceiverOptions::new(Source::new("Venue (Audio)"))
    .bandwidth(ReceiverBandwidth::AudioOnly)
    .name("venue-audio");
assert_eq!(options.configured_bandwidth(), ReceiverBandwidth::AudioOnly);
assert_eq!(options.configured_name(), Some("venue-audio"));
assert_eq!(options.configured_source().name(), "Venue (Audio)");
```

These accessors report stored requests, including unset values that ask the
SDK to choose a default. They do not query a live object's effective settings.

Borrowed receiver and FrameSync video expose only initialized active pixels.
`frame.data()` returns `Some(&[u8])` when all active pixels are contiguous, and
`None` when row or plane padding separates them. Traverse padded frames without
allocation or pixel copies:

```rust
if let Some(frame) = receiver.video().capture_ref(Duration::from_secs(1))? {
    for plane in frame.planes() {
        for row in plane.rows() {
            // Each row contains active bytes only, with no SDK padding.
            println!("{} active bytes", row.len());
        }
    }
    let packed = frame.to_owned(); // one pixel allocation; active rows only
}
```

Owned capture packs active rows. Use `to_owned()` when pixels must outlive the
capture buffer, or `copy_into(&mut frame)` to reuse an owned allocation. Copies
preserve media properties but may reduce stride. Immutable plane and row views
support scoped worker threads; FrameSync frame guards remain on their capture thread.

### Receive whichever media arrives next

Use `capture_any_ref` when one event loop should process whichever video, audio,
metadata or receiver invalidation arrives next. One wait requests every media
kind, so an absent audio stream does not delay video behind an audio timeout.
This works with Standard runtimes and no Cargo features:

```rust
use grafton_ndi::ReceiverItemRef;
use std::time::Duration;

match receiver.capture_any_ref(Duration::from_millis(100))? {
    Some(ReceiverItemRef::Video(frame)) => {
        for plane in frame.planes() {
            for row in plane.rows() { println!("{} active video bytes", row.len()); }
        }
    }
    Some(ReceiverItemRef::Audio(frame)) => {
        println!("{} samples per channel", frame.num_samples());
    }
    Some(ReceiverItemRef::Metadata(frame)) => println!("{}", frame.data()),
    Some(ReceiverItemRef::Event(event)) => println!("{event:?}"),
    _ => {}
}
```

Borrowed items retain their receiver borrow, make no wrapper payload copies or
allocations, and support scoped thread transfer. `item.to_owned()` or
`receiver.capture_any(timeout)` packs active rows/channels into owned frames,
preserving timing and metadata. Owned video/audio normally allocate one payload
buffer and another string when metadata is present; standalone metadata
allocates its text; events allocate nothing. Raw combined capture is available
only on an ordinary receiver. FrameSync and Advanced attachment modes reserve
some or all native media for their own capture methods.

Combined capture consumes pending and native events. Typed capture retains
events for `poll_event` or combined readers; all of those consumers compete for
coalesced invalidations. They do not receive a broadcast or ordered history.
After `SourceChanged`, use `poll_source_name_change` to query the connected name.
The same wait budget covers reconnect-lock contention and bounded native
attempts; zero timeout makes one non-waiting attempt. Early SDK misses are
retried with a pause until the budget is spent. Media may already be queued,
and a timeout alone does not establish disconnection or freshness.

For async applications, both `asynchronous::tokio::AsyncReceiver` and
`asynchronous::smol::AsyncReceiver` expose `capture_any(timeout).await`, returning
owned `ReceiverItem` values. Their worker creates and frees native frame guards
locally. The budget starts at first poll and includes worker-queue delay and
receiver-lock waits. Dropping the future suppresses work only before its entry
gate; work that has started may consume an item whose result is discarded.

Choose typed capture for dedicated media workers, combined capture for one
mixed-media loop, and FrameSync when output must follow a playback clock.
[`mixed_media_relay`](../examples/mixed_media_relay.rs) is a complete synchronous
relay using borrowed sends; [`mixed_media_async`](../examples/mixed_media_async.rs)
shows owned analysis with either async runtime.

## Publish sources

Create a `Sender` to publish a source. Use its tally, connection count and
metadata methods to monitor receivers and exchange control messages.

```rust
use grafton_ndi::{FrameRate, PixelFormat, Sender, SenderOptions, VideoFrame};
use std::time::Duration;

let options = SenderOptions::new("Rust Program Output")
    .clock_video(true)
    .clock_audio(true);
let sender = Sender::new(&ndi, &options)?;

let mut frame = VideoFrame::builder()
    .resolution(1920, 1080)
    .pixel_format(PixelFormat::Bgra)
    .frame_rate(FrameRate::new(60, 1)?)
    .build()?;
frame.data_mut().fill(0);

sender.send_video(&frame)?;

let connections = sender.connection_count(Duration::from_millis(500))?;
println!("connected receivers: {connections}");
```

Both video builders default to square pixels: their stored picture aspect is
the SDK's `0.0` sentinel, so display geometry follows width and height. Set a
positive `picture_aspect_ratio` for intentional anamorphic output. Captures and
copies preserve their source aspect value.

Choose a send method by buffer ownership:

| Input and timing | Method |
| --- | --- |
| Owned or borrowed pixels, synchronous | `send_video` |
| Owned frame, SDK async pipeline | `send_video_async` |
| Borrowed pixels, copied into the SDK async pipeline | `send_video_async_copy` |

`send_video_async` takes ownership and returns the previous completed frame for
reuse. Use two frames and call `finish_video_async()` to recover the last one.
This is the SDK's send pipeline, not a Rust future; synchronization can block.

For borrowed synchronous sends, `BorrowedVideoFrame::sends_in_place()` reports
whether the wrapper copies pixels. UYVY/UYVA buffers use packed, zero-tailed
staging by default on targets where the SDK needs tail room. Direct sending
from caller padding and trailing memory requires explicit opt-in. The method's
[API documentation](https://docs.rs/grafton-ndi/latest/grafton_ndi/struct.BorrowedVideoFrame.html#method.sends_in_place)
specifies the required trailing space, including field-layout exceptions.
After `send_video` returns, you can reuse the input buffer.

See [sending examples](../examples/README.md#sending) for allocation reuse.

Synchronous media operations accept sealed, validated source traits.
`VideoSource` covers owned `VideoFrame`, checked external `BorrowedVideoFrame`,
`VideoFrameRef` and `FrameSyncVideoRef`; `AudioSource` covers owned `AudioFrame`,
`AudioFrameRef` and `FrameSyncAudioRef`. Pass a shared reference directly to
`sender.send_video(&video)`, `sender.send_audio(&audio)`, or any
`AudioConverter::to_interleaved_*(&audio, destination, ...)` operation. Receiver
and FrameSync guards keep their existing lifetime and thread restrictions.

Eligible packed video sends use the input pixels directly. A captured guard
proves that active rows are initialized; its padding and trailing bytes remain
unproven. Padded captures and formats needing SDK tail room therefore copy only
active rows into the sender's reusable packed staging, zeroing required tail
bytes. After warm-up, a stable staged shape needs no allocation. External
borrowed buffers retain their explicit padding/tail opt-in; forwarding a capture
never opts it in. `VideoSource::sends_in_place()` reports the chosen path without
making an SDK call. Headers and per-frame metadata follow either path.

Planar audio sends and all three native interleaved conversions (`i16`, `i32`,
`f32`) use captured channel strides directly with no owned planar copy or
wrapper allocation. Conversion writes only the required prefix of the caller's
destination; integer output keeps its explicit reference-level and clipping
semantics. The destination prefix may be partially modified on native failure.
Synchronous calls finish borrowing their input when they return.
`send_video_async_copy(&source)` accepts the same video inputs and allocates an
owned packed pixel/metadata copy before submission, so the source can be freed
immediately. `send_video_async(frame)` continues to require ownership and returns
completed frames for reuse.

## Use FrameSync

`FrameSync` is for pull-based capture when your application has its own clock: a GPU vsync loop, audio callback, timeline, or mixer. Video capture returns the frame appropriate for the requested time base, and audio capture can resample to the requested output shape.

```rust
use grafton_ndi::{FrameSync, FrameSyncAudioRequest, ScanType};

let frame_sync = FrameSync::new(receiver)?;

// Borrow the SDK's frame; `capture_video` returns an owned copy instead.
if let Some(video) = frame_sync.capture_video_ref(ScanType::Progressive)? {
    println!("video: {}x{}", video.width(), video.height());
}

// 1024 stereo samples at 48 kHz, converted from the source's audio.
let request = FrameSyncAudioRequest::capture_at(48_000, 2, 1024)?;
let audio = frame_sync.capture_audio_ref(request)?;
println!("audio: {} channels at {} Hz", audio.num_channels(), audio.sample_rate());
```

For an interleaved playback sink, convert the borrowed output directly:

```rust
use grafton_ndi::audio_conversion::AudioConverter;
let converter = AudioConverter::new(&ndi)?;
let mut interleaved = vec![0.0f32; 2 * 1024]; // allocate once, reuse each tick
let audio = frame_sync.capture_audio_ref(request)?;
let written = converter.to_interleaved_f32(&audio, &mut interleaved)?;
// Submit interleaved[..written] to the audio sink at its playback cadence.
```

The complete [`framesync_audio_playback`](../examples/framesync_audio_playback.rs)
example writes clocked interleaved PCM to stdout, with reusable output storage
and no intermediate owned planar frame. Its software timer demonstrates the
pipeline; a real device's callback supplies the playback clock.

An audio capture always returns samples in the requested shape. FrameSync can
insert silence whenever current input is unavailable, including after input
previously worked. Source-provided all-zero samples are also valid input.
`frame_sync.audio_input_format()` reports the SDK's known source sample rate
and channel count, or `None` when it has no known format. A known format can
persist across a pause, disconnect, reconnect or source change; it does not
establish fresh reception. The query and a later capture are separate
operations. To follow the source's format, wait for `Some` and build the
request with `FrameSyncAudioRequest::for_input(format, 1024)`; query again
and rebuild to follow later changes.

`FrameSync::new` hands the receiver back inside `Rejected` when the SDK creates no
synchronizer. `FrameSync` owns its receiver. `frame_sync.receiver()` provides controls,
metadata capture, events, and status through `Receiver<FrameSyncAttached>`. The
synchronizer takes the receiver's video and audio, so that receiver has no raw
video or audio capture: a raw audio capture beside a FrameSync would take audio
frames that the FrameSync then replaces with silence. Use
`frame_sync.into_receiver()` to recover an ordinary `Receiver` with raw video
and audio capture.

With `advanced` enabled, `frame_sync.controls()` reaches the receiver's
exclusive stream/tier controls and KVM without exposing a mutable receiver.
A refused or redundant stream change remains `SettingChange::Unchanged` and
need not clear the synchronizer's cached output. To hold remote input while
clocked output continues, split capture from receiver controls:

```rust
use grafton_ndi::{FrameSync, FrameSyncAudioRequest, ScanType};
use grafton_ndi::advanced::MouseButton;

fn drag(sync: &mut FrameSync) -> grafton_ndi::Result<()> {
    let (capture, controls) = sync.split();
    let mut session = controls.kvm()?;
    session.mouse_down(MouseButton::Left)?;
    let video = capture.capture_video_ref(ScanType::Progressive)?;
    let audio = capture.capture_audio_ref(FrameSyncAudioRequest::capture_at(48_000, 2, 480)?)?;
    session.move_pointer(0.75, 0.5)?;
    session.release_all()?;
    drop((video, audio));
    Ok(())
}
```

Creating the views makes no SDK call or allocation. Their capture costs and
threading preserve the owner's contracts. Split views stay on the borrowing
thread, keeping exclusive stream changes sequenced with capture. Share the
complete FrameSync for concurrent capture instead. Returned frame guards also
remain on their capture thread. Release all held inputs before changing source,
and drop the views, sessions and frames before recovering the receiver.

The receiver operation matrix is:

| Owner | Raw capture through `receiver()` | Media output | Exclusive controls with `advanced` |
| --- | --- | --- | --- |
| `Receiver` | Video, audio, metadata and events | Typed or combined capture | Direct receiver methods |
| `FrameSync` | Metadata and events | Clocked video/audio | `controls()`; `split()` also lends clocked capture |
| `advanced::AvSync` | Video, metadata and events | Audio matched to video | `controls()`; `split()` also lends synchronization |
| `advanced::PacketReceiver` | None | `capture()` packets/decoded media/events | `controls()`; KVM sessions also `capture()` |
| `advanced::AllocatedReceiver` | Video, audio, metadata and events | Typed or combined capture | `controls()` |
| `advanced::AllocatedPacketReceiver` | None | `capture()` packets/decoded media/events | `controls()`; KVM sessions also `capture()` |

Every owner's `receiver()` retains connection control, diagnostics, tally,
metadata submission, delay and PTZ operations; Advanced receivers also retain
keyframe requests. The restricted view reaches setters and KVM on that same
native receiver. It never permits replacing or recovering an attached receiver.

### Monitor reception of the expected media

Poll `frame_sync.receiver().connection_stats()` on a control or diagnostics
path. Choose `audio_frames_received()`, `video_frames_received()` or
`metadata_frames_received()` for the media this application expects. A
receiver configured for audio only has no reason to advance its video counter.
Each counter counts incoming frames or audio packets, not samples played or
individual output buffers. Receiver queue/processing drops are included in
the native received total; the matching drop percentage is cumulative within
that native counter interval. It is not packet loss or an instantaneous
quality score. Keep drop deltas and expected throughput checks separate from
recent-reception status.

For each source or receiver observation interval, record the first counter
value as a baseline. Even a positive initial total has no known arrival time.
On later polls, an increase establishes recent reception at the local
`Instant` of that poll. With a connection, classify the stream as awaiting
first observed progress, recently receiving, or starved after a
**caller-chosen positive grace period** with no progress. Choose the grace
from expected packet cadence, jitter and startup/recovery budgets.
Disconnection takes precedence. Reset the baseline and prior progress after
receiver replacement, explicit connect/disconnect/reconnect, an observed
source or connection transition, or a decrease in the selected counter.
Recovery requires new growth in the current interval. The compilable
[mono borrowing example](../examples/framesync_reception_monitor.rs) shows
this monitor beside `capture_audio_ref`, without raw audio capture or an
owned-frame copy.

`is_connected()` reports native connection state, not media progress.
Discovery membership and status events are not media heartbeats. A positive
queue depth is advisory; it can persist during starvation. Capture success,
zero amplitude, a known format, output timestamps and timecodes cannot prove
fresh input or establish which source produced a resampled buffer. FrameSync
may repeat or retain video, including when input stops: `Some(frame)` means
output is available, not that a new input frame arrived since the previous
call. Normal output at a faster cadence than the source repeats frames.
Likewise a raw receiver or packet receiver can deliver a queued frame, while
a timeout alone does not prove disconnection. After reconnect or source
change, cached formats and queued output may still belong to earlier input.
Polling cannot detect every rapid automatic reconnect or source transition
between polls, and the native connection, performance and queue calls are not
an atomic snapshot. This recipe is a recent-reception heuristic, not proof
of freshness for each capture. It does not trigger automatic reconnects.

## Monitor and control

Receivers report connection state and frame-drop statistics, and control a PTZ camera through `receiver.ptz()`. Use `set_tally` to send program/preview state upstream, where the SDK keeps it for every connection; sender tally reports receiver state. `send_metadata` sends metadata upstream and returns `Error::NotConnected` when there is no source to send it to.

```rust
use grafton_ndi::ReceiverEvent;
use std::time::Duration;

if receiver.is_connected() {
    let stats = receiver.connection_stats();
    println!("video drop rate: {:.2}%", stats.video_drop_percentage());
}

if let Some(ReceiverEvent::StatusChanged(status)) = receiver.poll_event(Duration::from_millis(100))? {
    println!("Active connections: {}", status.connections());
}

let ptz = receiver.ptz();
if ptz.is_supported() {
    ptz.zoom(0.25)?;
}
```

`poll_event` returns `None` when its wait budget expires. Receiver controls are
also available through the receiver attached to a FrameSync or Advanced wrapper;
each mode exposes only the capture operations it supports.

`connect_to(&source)` requests a new source and remembers it as the connection
target. `connection_target()` returns an owned snapshot of that target.
`disconnect()` retains it, and `reconnect()` retries it. A successful request
does not mean the source has connected yet; observe actual changes with
`poll_source_name_change` and connection statistics.

Owned audio and video frames use `set_metadata(text)?` to replace metadata and
`clear_metadata()` to remove it. An invalid replacement preserves existing text.

## Integrate with async runtimes

Enable `tokio` or `smol` to use `asynchronous::AsyncReceiver`. These adapters
run blocking SDK work on a worker pool. Capture timeouts include queue and lock
waits from the future's first poll, but do not guarantee a completion deadline.
Dropping a future cancels work only before its cancellation gate; work that has
passed the gate can still consume a frame or event.

Use async methods for capture, events and reconnects. `receiver()` exposes the
synchronous receiver; calling its waiting methods blocks the calling thread.
For `#[tokio::main]`, enable Tokio's `macros` and runtime features in your own dependency.

```rust
use grafton_ndi::{asynchronous::tokio::AsyncReceiver, ReceiverEvent, Tally};
use std::time::Duration;

let async_receiver = AsyncReceiver::new(receiver);
async_receiver.receiver().set_tally(Tally::new(true, false));
if let Some(frame) = async_receiver.video().capture(Duration::from_secs(5)).await? {
    println!("{}x{}", frame.width(), frame.height());
}
if let Some(ReceiverEvent::StatusChanged(status)) = async_receiver.poll_event(Duration::ZERO).await? {
    println!("Active connections: {}", status.connections());
}
```

## Optional workflows

- [Routing and Discovery Server examples](../examples/README.md#discovery-and-monitoring): redirect receivers or register sources with a server. Discovery Server APIs need NDI 6.4 or later; read the [current limitations](troubleshooting.md#discovery-server).
- [Image encoding](image-encoding.md): save snapshots and thumbnails.
- [Advanced SDK](advanced.md): compressed packets, queues, allocators, synchronization and KVM.
- [File API](file-io.md): record sources and play recordings.

## Borrowed YUV padding

For UYVY/UYVA on targets with nonzero `PixelFormat::send_tail_room()`,
`Sender::send_video` packs active rows into reusable staging and zeroes the tail
by default. Caller row padding and trailing memory cannot change the picture.
`BorrowedVideoFrameBuilder::allow_send_from_padding_and_tail(true)` explicitly
permits direct sending when enough initialized room follows the pixels; those
bytes may then affect the encoded picture. `unsafe readable_after` declares
initialized, stable bytes in the same allocation and does not itself opt in.
`BorrowedVideoFrame::sends_in_place()` reports the selected behavior.
