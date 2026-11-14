# Use grafton-ndi

Install the runtime using [setup](setup.md), then choose a workflow below. The
linked examples are complete programs that CI is configured to compile. Method-level units, errors,
allocation and threading contracts live in the
[3.0 API reference](https://grantsparks.github.io/grafton-ndi/grafton_ndi/).

## Discover sources

Create `Finder::new(&ndi, &options)` with `FinderOptions` containing the groups
and extra IPs your application needs. Choose discovery by purpose:

- `current_sources()` returns the currently known snapshot.
- `wait_for_sources(timeout)` waits for a source-list change; one change can
  represent only part of a staggered discovery response.
- `find_sources(timeout)` observes the full window and returns distinct
  descriptors seen during it.
- `find_source_by_name(name, timeout)` stops at the first exact, case-sensitive
  name match. Use `find_source(timeout, predicate)` to disambiguate names by
  inspecting their addresses too.

Selection and enumeration include snapshot-lock waits in their timeout budget.
Concurrent waits on the same finder share each reported change, so every
lookup can inspect the updated list promptly.
A zero timeout makes one non-waiting attempt, which can miss a busy snapshot
lock. Native snapshot work, copying and caller predicates can finish after the
budget. `current_sources()` uses ordinary blocking serialization. Discovery
cannot guarantee continued availability or observation of every source.

Snapshots and accumulated results have bounded, fallible allocations.
`Error::ResourceLimit` identifies the resource and units through `ResourceKind`.
Invalid snapshot arrays and allocation failures return `UnexpectedSdkResult`;
individual malformed descriptors can be skipped. See
[`Finder`](https://grantsparks.github.io/grafton-ndi/grafton_ndi/finder/struct.Finder.html)
for exact limits. Run [`NDIlib_Find`](../examples/NDIlib_Find.rs) to inspect the
names your network advertises.

### Keep discovery in server state

Move the configured finder into `SourceCache::new(finder)` and store the cache
in service state, optionally shared through `Arc<SourceCache>`. It owns the
finder and retains its runtime, so no lifetime parameter or separately stored
finder is needed. `cache.finder()` lends that same finder for uncached work.

`sources_by_host(host, timeout)` returns every matching feed, leaving feed
selection to the caller. A miss observes the full discovery window; a hit
returns owned descriptor copies. Empty results are not cached. Entries remain
stale until `invalidate(host)` or `clear()` removes them: choose an invalidation
policy for your service. IP hints belong on the finder; a lookup never changes
its scope. See the compilable server-state example in
[`SourceCache::new`](https://grantsparks.github.io/grafton-ndi/grafton_ndi/finder/struct.SourceCache.html#method.new).

Keep other owning handles in service state or workers too. Create borrowed
capture/control views for an operation; retain owned media or encoded bytes
between operations. Use `copy_into` when a copy is needed and destination
storage can be reused.

## Receive streams

Create a receiver with `Receiver::new(&ndi, &ReceiverOptions::new(source))`.
Choose bandwidth and color format explicitly for the application; a monitoring
receiver and an image snapshot need different settings. Options' `configured_*`
accessors report stored requests, including unset values, rather than a live
object's effective settings. Cloning options preserves those requests;
chainable setters replace them and `clear_*` restores inherited defaults.

Use `receiver.video()`, `.audio()` or `.metadata()` for dedicated media workers.
Their `capture_ref(timeout)` borrows an SDK buffer; `capture(timeout)` copies
into an owned frame. A wait that expires returns `Ok(None)`. Different media
workers can capture concurrently; see
[`concurrent_capture`](../examples/concurrent_capture.rs).

Borrowed video exposes initialized active pixels through `planes()` and
`rows()`. `data()` returns `None` when row or plane padding prevents a single
active slice. Owned copies pack active rows/channels and can have a smaller
stride. Use `to_owned()` to retain media after its guard is dropped, or
`copy_into(&mut frame)` to reuse storage. Receiver guards permit scoped thread
transfer; FrameSync guards stay on their capture thread. Exact restrictions are
part of each frame type's rustdoc.

### Receive whichever media arrives next

Use `receiver.capture_any_ref(timeout)` for one mixed-media loop. It returns
`ReceiverItemRef::{Video, Audio, Metadata, Event}` without wrapper payload copies;
`capture_any(timeout)` returns owned items. Both request all media in one wait,
so absent audio does not put an audio timeout ahead of video. No Cargo feature
is needed. [`mixed_media_relay`](../examples/mixed_media_relay.rs) demonstrates
matching items and forwarding borrowed media synchronously.

Combined capture consumes pending and native receiver events. Typed capture
retains events for `poll_event` or a combined reader. These consumers compete
for coalesced invalidations; they receive neither broadcasts nor an ordered
history. After `SourceChanged`, query `poll_source_name_change` for the connected
name. The shared wait budget covers reconnect-lock contention and bounded
native attempts. A queued frame may predate the current wait, and a timeout
alone does not prove disconnection or stale input.

## Publish sources

Create `Sender::new(&ndi, &SenderOptions::new(name))`. Set sender clocking for
the application's cadence, and construct video with an explicit `FrameRate`
when 60 fps is unsuitable. Owned, borrowed and compressed video construction
all default to 60 fps. The default picture aspect uses square pixels; set
`picture_aspect_ratio` for anamorphic output. Captured and copied timing/aspect
properties are preserved.

Choose the send method by storage ownership:

| Input | Method | When storage can be reused |
| --- | --- | --- |
| Owned or borrowed media | `send_video`, `send_audio`, `send_metadata` | When the synchronous call returns |
| Owned video | `send_video_async` | When a later call returns the completed frame |
| Borrowed video | `send_video_async_copy` | Input is copied before submission |

The SDK's asynchronous send pipeline is separate from Rust futures and may
block while synchronizing. Reuse two owned frames and recover the final one
with `finish_video_async()`; see [`async_send`](../examples/async_send.rs).
[`async_send_copy`](../examples/async_send_copy.rs) shows the copying variant.

`VideoSource`, `AudioSource` and `MetadataSource` let synchronous operations
accept owned frames and validated borrowed captures directly. Native audio
conversion also accepts borrowed audio and writes into caller-provided output;
see [`NDIlib_Recv_Audio_16bpp`](../examples/NDIlib_Recv_Audio_16bpp.rs).
Borrowing a frame avoids an intermediate owned copy but does not guarantee an
in-place video send: padded captures and formats needing SDK tail room use
reusable staging. `VideoSource::sends_in_place()` reports the selected path.

Recover pooled buffers with `into_data()` or `swap_data()`. A rejected owned
build or replacement returns its supplied storage in `Rejected`; recover it
with `into_inner()` or `into_parts()` to repair and retry. Owned frames edit
metadata with `set_metadata` and `clear_metadata`; invalid replacement text
preserves the old value. [`media_buffers`](../examples/media_buffers.rs) shows
buffer reuse, borrowed planar audio and checked native video conversion.

Optional per-frame metadata does not prevent reception of valid video or
samples. `metadata()` returns `None` when that text is not UTF-8 or has no
terminator within the 4 MiB cap; `metadata_status()` distinguishes these
conditions from absent text. The borrowed frame keeps this status without
allocation. Forwarding omits unreadable text, and owned copies retain only
valid metadata, so their status becomes `Absent`. Standalone metadata frames
remain strict because their text is the entire payload.

## Use FrameSync

Use `FrameSync` when output follows an application clock such as a playback
callback or render loop. `FrameSync::new(receiver)` takes ownership, returning
the receiver in `Rejected` on failure. It reserves raw video and audio capture;
`receiver()` still provides metadata, events and shared controls.
`into_receiver()` restores ordinary capture.

At each output tick, use `capture_video_ref(scan_type)` or
`capture_audio_ref(request)`. The methods without `_ref` copy to owned frames.
`FrameSyncAudioRequest::capture_at(rate, channels, samples)` fixes the output
shape. To follow the input shape, query `audio_input_format()` and build a
request with `FrameSyncAudioRequest::for_input(format, samples)`, rebuilding it
when the format changes. A known format can persist during starvation or a
source change; querying it and capturing are separate operations.

[`framesync_audio_playback`](../examples/framesync_audio_playback.rs) converts
borrowed output directly into reusable interleaved PCM. Its software timer is
a demonstration; a playback device supplies its own cadence. FrameSync may
insert silence or repeat video when input is unavailable. Successful clocked
output therefore does not establish recent reception. FrameSync audio without
an SDK source timestamp reports `None`; output without a source timecode carries
`Timecode::SYNTHESIZE`. The missing timestamp alone does not identify silence.

With `advanced`, `controls()` reaches exclusive stream/tier controls and KVM.
`split()` separates clocked capture from those controls so held remote input
can span captures. Split views stay on their borrowing thread; FrameSync frame
guards stay on their capture thread. `session.receiver()` lends a restricted
`KvmReceiver` with compatible capture and shared controls. To change source,
successfully release held inputs, end the session, change the owner's
connection, then start a new session. Drop outstanding borrows before
recovering the receiver. Source loss, remote Discovery Server changes and
failed release delivery remain best-effort limitations. See
[`advanced_kvm`](../examples/advanced_kvm.rs).

Receiver ownership determines available capture; exact contracts remain in
rustdoc:

| Owner | Media path | Receiver capture retained |
| --- | --- | --- |
| `Receiver` | Typed or combined capture | Video, audio, metadata and events |
| `FrameSync` | Clocked video/audio | Metadata and events |
| `advanced::AvSync` | Video-timestamp-matched audio | Video, metadata and events |
| `advanced::PacketReceiver` | Owned packets, decoded media and events | None |
| `advanced::AllocatedReceiver` | Typed or combined capture | Video, audio, metadata and events |
| `advanced::AllocatedPacketReceiver` | Owned packets, decoded media and events | None |

Attached owners lend shared controls through `receiver()` and exclusive Advanced
controls through `controls()` without allowing the receiver to be replaced.

### Monitor reception of the expected media

Poll `connection_stats()` on a diagnostics path, selecting
`audio_frames_received()`, `video_frames_received()` or
`metadata_frames_received()` for the expected media. Audio totals count packets,
not samples played. Received totals include receiver queue/processing drops;
drop percentages are cumulative within the native counter interval, rather
than packet-loss measurements or instantaneous quality scores.

Use the first total as a baseline. On later polls, an increase records progress
at the local `Instant`. With a connection, classify the stream as awaiting
first progress, recently receiving, or starved after a **caller-chosen positive
grace period** without growth. Choose that grace from cadence, jitter and
startup/recovery needs. Disconnection takes precedence. Reset the baseline and
prior progress after replacement, explicit connect/disconnect/reconnect, an
observed source or connection transition, or a counter decrease. Require new
growth for recovery. Keep drop deltas and expected throughput checks separate.

[`framesync_reception_monitor`](../examples/framesync_reception_monitor.rs)
implements this beside borrowed mono FrameSync audio output. A positive initial
total has no known arrival time. Connection state, queue depth, a known format,
output timestamps, silence and successful capture do not prove fresh input for
each output. Queued output or cached formats can survive source changes; polling
can miss rapid transitions and native statistics are not an atomic snapshot.
This monitor is a recent-reception heuristic and does not reconnect automatically.

## Monitor and control

Use `set_tally` for upstream program/preview state, `ptz()` for supported camera
controls, and `send_metadata` for upstream metadata (`Error::NotConnected` when
there is no source). [`status_monitor`](../examples/status_monitor.rs) shows
status events; [`NDIlib_Recv_PTZ`](../examples/NDIlib_Recv_PTZ.rs) checks PTZ
support before sending controls.

`connect_to(&source)` remembers a new requested target; `connection_target()`
returns its owned snapshot. `disconnect()` retains it and `reconnect()` retries
it. Success means the request was issued; observe connection state and media
progress separately.

## Integrate with async runtimes

Enable `tokio` or `smol` and wrap the receiver in the corresponding
`asynchronous::<runtime>::AsyncReceiver`. It runs blocking SDK work on workers.
Capture/event timeouts start at the future's first poll and include queue and
receiver-lock waits; processing can finish after that wait budget. Dropping a
future skips queued work and stops a started wait before its next attempt.
The attempt already in flight can still consume a frame/event and execute its
callback; each native wait lasts at most 100 ms.

Use typed `capture_with(timeout, callback)` or mixed
`capture_any_with(timeout, callback)` for immediate inspection. A synchronous
callback borrows validated media on the capturing worker; only its
`Send + 'static` result crosses back to the task. The callback and its captures
must also be `Send + 'static`. It runs only for a delivered value. A panic
is resumed on the awaiting task with its original payload, after the SDK guard
is freed on that worker. An abandoned future discards its panic payload.
`Error::SpawnFailed(SpawnFailure)` distinguishes a missing Tokio runtime from a
cancelled blocking task.

Keep callbacks bounded: they hold an SDK buffer and occupy a worker. The wait
timeout does not bound callback duration or cancel its side effects. Scoped
processing makes no wrapper payload copies; runtime/task and user callback
allocations still apply. Owned `capture`/`capture_any`, or `to_owned()` within a
callback, copy media for retention across awaits.
[`mixed_media_async`](../examples/mixed_media_async.rs) shows both runtimes.
Use async reconnect/event methods for waits; waiting through the synchronous
`receiver()` blocks the calling thread.

## Optional workflows

- [Routing and Discovery Server examples](../examples/README.md#discovery-and-monitoring): redirect or advertise sources; see [runtime capabilities](setup.md#runtime-capabilities).
- [Image encoding](image-encoding.md): snapshots and thumbnails.
- [Advanced SDK](advanced.md): compressed packets, queues, allocators, synchronization and KVM.
- [File API](file-io.md): recording and playback.

## Borrowed YUV padding

UYVY/UYVA sends use sanitized staging on targets needing SDK tail room. Active
rows are packed and tail bytes zeroed by default, so caller padding cannot
change the picture. Direct external sending requires enough initialized space
and `allow_send_from_padding_and_tail(true)`. The unsafe `readable_after`
declaration alone does not opt in; padding/tail bytes may affect encoded output
once opted in. Allocate with `PixelFormat::send_buffer_len` and follow the
[`BorrowedVideoFrameBuilder` safety contract](https://grantsparks.github.io/grafton-ndi/grafton_ndi/sender/struct.BorrowedVideoFrameBuilder.html).
