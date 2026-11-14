# Examples

Install a runtime using the [setup guide](../docs/setup.md).

Run an example with `cargo run --example <name>`. Arguments vary: read each file's opening comment and parser before running it. Some use extra IP hints; others require a source name, file or timing parameter.

Build every example, including the feature-gated ones, with:

```sh
cargo build --locked --examples --all-features
cargo clippy --locked --examples --all-features -- -D warnings
```

Compilation does not prove that a runtime supplies an optional capability. To
exercise the Advanced examples, set `NDI_SDK_DIR` to the Advanced SDK before
building and `NDI_RUNTIME_DIR_V6` to its runtime directory before running.
The Advanced examples explicitly select `SdkEdition::Advanced`, so an
incompatible runtime fails at initialization.

Start a source producing the required media before running a receiving example.
For IP hints, source selection and output options, follow that example's usage.
PTZ/KVM examples also need a source that offers those controls. The
[usage guide](../docs/usage.md) explains capture ownership and buffer reuse.

These examples use the current 3.0 APIs: borrowed `_ref` captures for immediate
processing, validated `FrameSyncAudioRequest` values, reusable send buffers and
explicit completion for async sends and recordings. Software output timers are
demonstrations; use the output device's clock for playback. See the
[migration guide](../docs/migration/2.0-to-3.0.md) when adapting older code.

## Discovery and monitoring

| Example | Command | What it shows |
| --- | --- | --- |
| [`runtime_selection.rs`](runtime_selection.rs) | `cargo run --example runtime_selection` | Select a runtime, inspect it, then start NDI. |
| [`NDIlib_Find.rs`](NDIlib_Find.rs) | `cargo run --example NDIlib_Find` | Discover NDI sources on the network. |
| [`framesync_reception_monitor.rs`](framesync_reception_monitor.rs) | `cargo run --example framesync_reception_monitor -- 'HOST (Audio)' 1500` | Monitor recent audio reception beside borrowed mono FrameSync output; choose the positive grace period in ms for your application. |
| [`status_monitor.rs`](status_monitor.rs) | `cargo run --example status_monitor` | Print a receiver's connection status changes. |
| [`routing.rs`](routing.rs) | `cargo run --example routing` | Route receivers to each discovered source in turn. |
| [`discovery_server.rs`](discovery_server.rs) | `cargo run --example discovery_server -- 127.0.0.1:5959` | Register a sender with a Discovery Server and list what the server reports. Needs a Discovery Server and an NDI 6.4.0 or later runtime. |

## Receiving

| Example | Command | What it shows |
| --- | --- | --- |
| [`NDIlib_Recv_Audio.rs`](NDIlib_Recv_Audio.rs) | `cargo run --example NDIlib_Recv_Audio` | Receive and inspect audio. |
| [`NDIlib_Recv_Audio_16bpp.rs`](NDIlib_Recv_Audio_16bpp.rs) | `cargo run --example NDIlib_Recv_Audio_16bpp` | Convert received audio with the native v3 API and explicit headroom. |
| [`framesync_audio_playback.rs`](framesync_audio_playback.rs) | `cargo run --example framesync_audio_playback -- 'HOST (Audio)' > output.f32` | Convert borrowed FrameSync audio directly into reusable interleaved f32 PCM output. |
| [`NDIlib_Recv_FrameSync.rs`](NDIlib_Recv_FrameSync.rs) | `cargo run --example NDIlib_Recv_FrameSync` | Clock-corrected capture with `FrameSync`. |
| [`NDIlib_Recv_PNG.rs`](NDIlib_Recv_PNG.rs) | `cargo run --features image-encoding --example NDIlib_Recv_PNG` | Receive video and save a PNG. Needs the `image-encoding` feature. |
| [`NDIlib_Recv_PTZ.rs`](NDIlib_Recv_PTZ.rs) | `cargo run --example NDIlib_Recv_PTZ` | Control PTZ cameras. |
| [`concurrent_capture.rs`](concurrent_capture.rs) | `cargo run --example concurrent_capture` | Capture video, audio and metadata from one receiver on separate threads. |
| [`mixed_media_relay.rs`](mixed_media_relay.rs) | `cargo run --example mixed_media_relay -- "HOST (Camera)" "Relay"` | Relay borrowed video/audio and metadata from one mixed-media wait; Ctrl-C stops it. |
| [`mixed_media_async.rs`](mixed_media_async.rs) | `cargo run --features tokio --example mixed_media_async -- "HOST (Camera)"` | Inspect borrowed mixed media on a blocking worker and return small owned summaries without payload copies; use `--features smol` for smol. |

## Sending

| Example | Command | What it shows |
| --- | --- | --- |
| [`NDIlib_Send_Audio.rs`](NDIlib_Send_Audio.rs) | `cargo run --example NDIlib_Send_Audio` | Send audio. |
| [`NDIlib_Send_Video.rs`](NDIlib_Send_Video.rs) | `cargo run --example NDIlib_Send_Video` | Send video. |
| [`async_send.rs`](async_send.rs) | `cargo run --example async_send` | Reuse two owned frames for async sending and recover completed frames. |
| [`async_send_copy.rs`](async_send_copy.rs) | `cargo run --example async_send_copy` | Copy borrowed pixels into async sends, then into the frames they return. |
| [`zero_copy_send.rs`](zero_copy_send.rs) | `cargo run --example zero_copy_send` | Render into two reusable owned frames without per-send pixel copies. |
| [`media_buffers.rs`](media_buffers.rs) | `cargo run --example media_buffers` | Recover and swap pooled buffers, borrow planar audio, inspect trait objects and convert borrowed P216. |

## Advanced SDK

See [Advanced requirements](../docs/advanced.md#requirements) for headers, feature and runtime selection:

| Example | Command | What it shows |
| --- | --- | --- |
| [`advanced_source_metadata.rs`](advanced_source_metadata.rs) | `cargo run --features advanced --example advanced_source_metadata` | List sources with the metadata they advertise. |
| [`advanced_passthrough.rs`](advanced_passthrough.rs) | `cargo run --features advanced --example advanced_passthrough` | Relay compressed video and audio without decoding. |
| [`advanced_queued_send.rs`](advanced_queued_send.rs) | `cargo run --features advanced --example advanced_queued_send` | Queue video frames and reuse each after SDK completion. |
| [`advanced_allocator.rs`](advanced_allocator.rs) | `cargo run --features advanced --example advanced_allocator` | Receive frames into memory the application supplies. |
| [`advanced_avsync.rs`](advanced_avsync.rs) | `cargo run --features advanced --example advanced_avsync` | Fetch the audio that belongs to each video frame. |
| [`advanced_genlock.rs`](advanced_genlock.rs) | `cargo run --features advanced --example advanced_genlock` | Pace a sender from another source's clock. |
| [`advanced_kvm.rs`](advanced_kvm.rs) | `cargo run --features advanced --example advanced_kvm` | Remote keyboard and mouse control of a source that offers it. |

## File API

See [File API requirements](../docs/file-io.md) for the `file-io` feature and runtime capabilities:

| Example | Command | What it shows |
| --- | --- | --- |
| [`file_record.rs`](file_record.rs) | `cargo run --example file_record --features file-io -- "MACHINE (Source)" take 10` | Record without a proxy track, wait for finalization and check the session report. |
| [`file_play.rs`](file_play.rs) | `cargo run --example file_play --features file-io -- take_001.mkv "Replay"` | Play a recording back as an NDI source. |
| [`file_thumbnails.rs`](file_thumbnails.rs) | `cargo run --example file_thumbnails --features file-io,image-encoding -- take_001.mkv 8` | Seek through a recording with one video track and write quarter-size JPEGs with reusable conversion and output buffers. |
