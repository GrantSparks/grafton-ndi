# Public API conventions

These conventions describe the public API across the default and optional
features. They guide additions and reviews as well as application code.

## Construction

A native object uses `X::new(parent, &XOptions)`. Its parent is `&Ndi`, or
`&FileApi` for File objects. `AllocatorService` takes the
`ProcessLifetimeRuntime` its callbacks need. Extensions take base options
before extension settings, as in
`QueuedSender::new(&ndi, &SenderOptions, &QueueLimits)`.

A wrapper takes the object it wraps: `FrameSync::new(receiver)` and
`SourceCache::new(finder)`. A constructor that can refuse an owned input
returns it through `Rejected<Inner>`. Constructors belong to the type they
create. Async File handles use `create` and `create_with_options` because
creation runs on their owner thread; runtime initialization also has its
`Ndi::new` and `with_options` entry points.

## Options

`XOptions` configures `X`. Shared settings retain their family name, such as
`AdvertiserOptions`, `ListenerOptions` and `AsyncFileOptions`; bundles of
limits use `XLimits`.

A setter is named for its setting. `configured_<setter>()` returns the request
exactly as stored, including whether it is unset. `clear_<setter>()` restores
an inherited default. These accessors describe requests, rather than the
runtime's effective configuration. Lists accept
`impl IntoIterator<Item = impl Into<String>>` and read back as
`Option<&[String]>`. Object creation validates settings, including rejecting
empty, comma-containing or NUL-containing list elements with `InvalidInput`.

Every options type implements `Debug + Clone + PartialEq`, and implements
`Default` when it has no required setting.

## Values

Value constructors validate their input once. Later use does not repeat the
same validation or introduce the same failure path. Signatures use the
validated type for each concept: `FrameRate`, `PictureAspectRatio`, `FourCc`,
`Timecode`, `SdkTimestamp` and `Source`. Code mappings implement `From` and
`TryFrom`. SDK-domain counts and dimensions use `u32`; indices and storage
sizes use `usize`.

## Capture

`capture_<kind>(...)` returns an owned frame, `capture_<kind>_ref(...)` a
borrowed frame, and async `capture_<kind>_with(..., f)` processes a borrowed
frame within a worker closure. Kinds are `video`, `audio` and `metadata`.
Mixed capture uses `capture_any`, `capture_any_ref` and `capture_any_with`.
AV sync keeps `synchronize` and `queued_audio` for its distinct operations.

Receiver modes determine available methods: video requires `VideoMode`, audio
requires `AudioMode`, metadata requires `MetadataMode`, and mixed raw capture
requires `RawCapture`. Attaching a synchronizer reserves its media paths.
FrameSync audio capture returns a frame because it can synthesize silence.

## Events, waits and discovery

`next_event(timeout)` returns `Result<Option<Event>>`; a batch uses
`next_events(timeout) -> Result<Vec<Event>>`. A crate queue exposes
`next_<item>(timeout)` and `try_next_<item>()`.

`wait_for_<condition>(timeout)` returns `Result<bool>`, or `Result<Option<T>>`
when it reports a value. `false` or `None` means the wait expired.
For example, `wait_for_finish` returns `Option<FinishStatus>`: `None` on
timeout, or `Some` with a final recorder outcome. The separate bounded
completion operation `finish` retains `FinishStatus::TimedOut(report)`;
`report()` supplies a current snapshot after an expired wait.
`find_*(..., timeout)` observes a discovery window; `current_*()` returns a
snapshot. Getters never wait for the SDK or network. Timeouts belong to these
operations, capture, and operations bounding their own completion, such as
`FileRecorder::finish(timeout)`. `poll_` is reserved for Future-style polling.
Arguments appear in this order: inputs, timeout, closures.

## Video submission

`send_*` returns after the SDK finishes with the input. `submit_*` hands owned
storage to the SDK pipeline and returns storage it has released. `reclaim_*`
returns frames still held; `flush_*` waits for release while retaining them.
These synchronous pipeline methods never end in `_async`. Rust futures live
in `asynchronous` and `file::Async*`. A File operation's `submit` starts that
operation immediately, without waiting for its future to be polled.

## Targets

Connecting objects use `connect_to(&Source)` and `disconnect()`, with
`reconnect()` when they remember a target. Stored targets use
`set_<target>(&Source)` and `clear_<target>()`, including routes and failover.
`SourceSpec` remains a separate File value because recorder name and URL
selection have different precedence from ordinary source connection.

## Errors and traits

Each error condition has one variant. Payload names end in `Defect` for an
invalid value, `Failure` for a failed operation, or `Refusal`, `Conflict`,
`Limit` and `Kind` for reasons and discriminators. Payloads derive
`thiserror::Error` to implement `std::error::Error`, and implement
`Debug + Clone + PartialEq`, plus `Copy`, `Eq` and `Hash`
where their fields allow. The crate only defines error variants it returns.

Return types reflect actual failure modes. `send_audio` and `send_metadata`
return `()`, whereas `send_video` can report an unavailable format capability.
An owned rejected input remains available through `Rejected<T>`.

Every public type implements `Debug`; the crate enforces
`deny(missing_debug_implementations)`. A native owner's formatter describes
its identity, such as name, source or mode, and ends with
`finish_non_exhaustive()`. Formatting makes no SDK call, takes no blocking
lock, and prints no pointers. Value types implement `Clone`, `Copy`,
`PartialEq`, `Eq` and `Hash` where their fields allow.

Thread traits follow the native object's contract:

| Types | `Send` | `Sync` |
| --- | --- | --- |
| `Ndi`, `Runtime`, `Finder`, `SourceCache`, `Receiver`, `Sender`, `FrameSync` | Yes | Yes |
| `Router`, Discovery Server listeners and advertisers | Yes | No |
| `advanced::AvSync`, `advanced::Genlock`, its audio/video clock views | Yes | No |
| Advanced packet receivers, compressed/queued senders, allocator service | Yes | Yes |
| Receiver video/audio guards, packet guards | Yes | Yes |
| FrameSync capture views and frame guards, AV sync capture/audio guards | No | No |
| `file::FilePlayer`, `file::FileRecorder` | No | No |
| Owned async receiver/finder adapters and async File handles | Yes | Yes |
| Async source-watch cursor | Yes | No |

Borrowed views remain subject to their owner's lifetime and mode. A `Send`
owner does not imply its borrowed capture views can move between threads.

## Paths and documentation

The root re-exports the default workflow: runtime, capabilities, finder,
frames, FrameSync, receiver, sender, errors and media values. Core types may
also have one module path. Routing, Discovery Servers and converters use
`routing`, `discovery_server`, `audio_conversion` and `video_conversion`.
Optional APIs use `advanced`, `file`, `asynchronous` and `image_encoding`;
their only root exports are `Error` variants and error payloads. Each item has
at most one module path. Root names do not shadow `tokio`, `smol` or `image`.

Public parameter names form part of the documentation. They have no leading
underscore, and common inputs consistently use `frame`, `metadata`, `source`,
`options` and `timeout`. Public wrappers carry a `doc(alias = "NDIlib_...")`
for their SDK entry points. The documentation checker verifies parameter
names in generated HTML and native table alias coverage, with explicit
reasons for entry points used only internally. Example target names use
snake_case; ports of SDK samples identify the original sample in their header.
