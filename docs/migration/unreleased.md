# Migrate from 3.0 to the unreleased API

## Video timing

`VideoFrame::builder`, `BorrowedVideoFrame::builder` and `VideoPacketFormat::new`
now all default to 60 fps. Borrowed frames and compressed formats previously
defaulted to 30 fps. Set `FrameRate::new(30, 1)?` explicitly where that timing is
required. Converting or copying a frame still preserves its supplied timing.

## Packet capture notifications

Rename `PacketEvent` to `PacketItem`. Its media variants are unchanged.
Notifications now use the common receiver event model:

```diff
-PacketEvent::StatusChanged => { /* query connection count separately */ }
+PacketItem::Event(ReceiverEvent::StatusChanged(status)) => {
+    println!("connections: {}", status.connections());
+}
-PacketEvent::SourceChanged => { /* ... */ }
+PacketItem::Event(ReceiverEvent::SourceChanged) => { /* ... */ }
-PacketEvent::Unknown(code) => { /* ... */ }
+PacketItem::Event(ReceiverEvent::Unknown(code)) => { /* ... */ }
```

These are coalesced receiver invalidations with the same current-state status
snapshot as `ReceiverItem::Event`. Packet capture consumes pending events once,
without making a native capture wait first.

## AV synchronization failures

Remove matches on `AvSyncOutcome::InternalError` and `AvSyncOutcome::Unknown`.
`synchronize(...)?` propagates SDK internal failures as
`Error::NativeOperationFailed { operation: "NDIlib_avsync_synchronize" }` and
unknown status codes as `Error::UnexpectedSdkResult`. Expected lack of audio,
empty results and format changes remain successful `AvSyncOutcome` values.

## Owned frame editing

Compressed and scatter audio/video now use the same editing pattern as owned
raw frames:

```diff
-let frame = CompressedVideoFrame::new(format, bytes)?
-    .with_timecode(timecode)
-    .with_metadata("<cue/>")?;
+let mut frame = CompressedVideoFrame::new(format, bytes)?;
+frame.set_timecode(timecode);
+frame.set_metadata("<cue/>")?;
+frame.clear_metadata();
```

`set_metadata` returns `Result<()>` and leaves the prior value unchanged on
failure; it never consumes the frame. `clear_metadata` removes metadata, while
`set_metadata("")` stores present but empty metadata. Audio frames replace
`with_protected(value)` with `set_protected(value)`. `VideoPacketFormat` remains
a reusable format value with fluent `with_*` methods.

## File statistics and async metadata

Replace `player_stats.video_frames()[0]` with
`player_stats.video_frames(StreamTier::Program)` and index `1` with
`StreamTier::Preview`. Recorder statistics replace their arrays with named
getters:

| Previous getter | Video | Audio | Metadata |
| --- | --- | --- | --- |
| `receiver_dropped()` | `video_frames_dropped_by_receiver()` | `audio_frames_dropped_by_receiver()` | `metadata_frames_dropped_by_receiver()` |
| `pipeline_dropped()` | `video_frames_dropped_by_pipeline()` | `audio_frames_dropped_by_pipeline()` | `metadata_frames_dropped_by_pipeline()` |
| `last_pts()` | `last_video_pts()` | `last_audio_pts()` | `last_metadata_pts()` |

Async recorder `inject_metadata` now accepts the same `MetadataSource` inputs
as synchronous recording. Pass captured metadata directly, without a preliminary
`to_owned()` call. The method copies text and timecode immediately, so the
source can be released before polling the returned operation.

## Source cache ownership

Move the configured finder into the cache:

```diff
-let cache = SourceCache::new(&finder);
+let cache = SourceCache::new(finder);
```

Replace `SourceCache<'a>` field and return types with `SourceCache`. The cache
owns the finder and retains its runtime; it can be returned from a constructor,
stored directly in server state and shared through `Arc<SourceCache>`.

Use `cache.finder()` wherever subsequent code previously used `finder` for
uncached discovery or selection. This returns `&Finder`, preserving its original
configuration and snapshot synchronization. Cache construction moves the finder
without an allocation, reference count or SDK call. There is no borrowed cache
variant or deprecated constructor.

Host matching, full-window discovery, timeout budgets, copy limits and
invalidation semantics are unchanged. See the current
[server-state pattern](../usage.md#keep-discovery-in-server-state).
