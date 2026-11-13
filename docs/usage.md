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

Use `SourceCache` for repeated lookups by complete hostname or IP address. Entries
remain until `invalidate()` or `clear()` removes them. For partial names or labels,
filter a `Finder` snapshot instead.

```rust
use grafton_ndi::SourceCache;
use std::time::Duration;

let cache = SourceCache::new();
let camera = cache.find_by_host(&ndi, "192.168.1.100", Duration::from_secs(5))?;
```

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
whether the wrapper copies pixels. Some UYVY/UYVA buffers need a staging copy
because the SDK reads beyond the active frame. The method's
[API documentation](https://docs.rs/grafton-ndi/latest/grafton_ndi/struct.BorrowedVideoFrame.html#method.sends_in_place)
specifies the required trailing space, including field-layout exceptions.
After `send_video` returns, you can reuse the input buffer.

See [sending examples](../examples/README.md#sending) for allocation reuse.

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

An audio capture always returns samples in the requested shape, silence until
the source's audio arrives. `frame_sync.audio_input_format()` reports the
source's sample rate and channel count, or `None` before any audio arrives; to
follow the source's format, wait for `Some` and build the request with
`FrameSyncAudioRequest::for_input(format, 1024)`; query again and rebuild to
follow later changes.

`FrameSync::new` hands the receiver back inside `Rejected` when the SDK creates no
synchronizer. `FrameSync` owns its receiver. `frame_sync.receiver()` provides controls,
metadata capture, events, and status through `Receiver<FrameSyncAttached>`. The
synchronizer takes the receiver's video and audio, so that receiver has no raw
video or audio capture: a raw audio capture beside a FrameSync would take audio
frames that the FrameSync then replaces with silence. Use
`frame_sync.into_receiver()` to recover an ordinary `Receiver` with raw video
and audio capture.

## Monitor and control

Receivers report connection health and frame-drop statistics, and control a PTZ camera through `receiver.ptz()`. Use `set_tally` to send program/preview state upstream, where the SDK keeps it for every connection; sender tally reports receiver state. `send_metadata` sends metadata upstream and returns `Error::NotConnected` when there is no source to send it to.

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
